//! SQLite adapter. Opens the database file read-only and introspects it.
//!
//! GOTCHA: `PRAGMA foreign_keys` is a per-connection setting, not schema
//! state. Foreign keys therefore come from `PRAGMA foreign_key_list`
//! regardless of whether enforcement was ever enabled on any connection.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    dialect_api::{Dialect, Scope},
    error::Error,
    model::{Column, Edge, EdgeKind, SchemaGraph, TableNode},
};

/// SQLite dialect adapter. `url` is a file path, optionally prefixed with
/// `sqlite://` or `file:`.
pub struct SqliteDialect {
    path: String,
}

impl SqliteDialect {
    pub fn new(url: &str) -> Self {
        let path = url
            .strip_prefix("sqlite://")
            .or_else(|| url.strip_prefix("file:"))
            .unwrap_or(url)
            .to_string();
        Self { path }
    }
}

#[async_trait::async_trait]
impl Dialect for SqliteDialect {
    async fn introspect(&self, scope: &Scope) -> Result<SchemaGraph, Error> {
        let path = self.path.clone();
        let scope = Scope {
            include: scope.include.clone(),
            exclude: scope.exclude.clone(),
        };
        tokio::task::spawn_blocking(move || introspect(&path, &scope))
            .await
            .map_err(|e| Error::Operational(format!("introspect task panicked: {e}")))?
    }
}

/// One row of `PRAGMA foreign_key_list`.
type FkRow = (i64, i64, String, String, Option<String>, String, String);

fn escape_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn normalize_type(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ").to_uppercase()
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// One lexical piece of stored SQL text.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SqlToken {
    /// An identifier-like word.
    Word(String),
    /// An identifier written with quotes (`"x"`, `` `x` ``, `[x]`); its
    /// content is kept exactly as written.
    Quoted(String),
    /// One punctuation character.
    Punct(char),
}

impl SqlToken {
    /// Text of any identifier, quoted or not: a possible relation name.
    fn text(&self) -> Option<&str> {
        match self {
            SqlToken::Word(text) | SqlToken::Quoted(text) => Some(text),
            SqlToken::Punct(_) => None,
        }
    }

    /// Text of a bare word only. A quoted identifier is a name, never a
    /// keyword, so `"from"` cannot act as the FROM keyword.
    fn bare(&self) -> Option<&str> {
        match self {
            SqlToken::Word(text) => Some(text),
            SqlToken::Quoted(_) | SqlToken::Punct(_) => None,
        }
    }
}

/// Split stored SQL into words and punctuation. String literals (`'...'`)
/// and comments (`-- ...`, `/* ... */`) are dropped, so no text inside a
/// literal or a comment can look like a table name.
fn sql_tokens(sql: &str) -> Vec<SqlToken> {
    let c: Vec<char> = sql.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < c.len() {
        match c[i] {
            '-' if c.get(i + 1) == Some(&'-') => {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
            }
            '/' if c.get(i + 1) == Some(&'*') => {
                i += 2;
                while i + 1 < c.len() && !(c[i] == '*' && c[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            '\'' => {
                i += 1;
                while i < c.len() {
                    if c[i] == '\'' {
                        if c.get(i + 1) == Some(&'\'') {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            q @ ('"' | '`') => {
                i += 1;
                let mut text = String::new();
                while i < c.len() {
                    if c[i] == q {
                        if c.get(i + 1) == Some(&q) {
                            text.push(q);
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    text.push(c[i]);
                    i += 1;
                }
                out.push(SqlToken::Quoted(text));
            }
            '[' => {
                i += 1;
                let mut text = String::new();
                while i < c.len() && c[i] != ']' {
                    text.push(c[i]);
                    i += 1;
                }
                i = (i + 1).min(c.len());
                out.push(SqlToken::Quoted(text));
            }
            ch if is_ident_char(ch) => {
                let mut text = String::new();
                while i < c.len() && is_ident_char(c[i]) {
                    text.push(c[i]);
                    i += 1;
                }
                out.push(SqlToken::Word(text));
            }
            ch if ch.is_whitespace() => {
                i += 1; // separators carry no name
            }
            ch => {
                out.push(SqlToken::Punct(ch));
                i += 1;
            }
        }
    }
    out
}

/// Words that end a table list in a FROM clause: never a table alias.
const NOT_ALIAS: &[&str] = &[
    "as", "and", "cross", "except", "from", "full", "group", "having", "inner", "intersect",
    "join", "left", "natural", "not", "offset", "on", "or", "order", "right", "select", "set",
    "union", "using", "values", "where", "window",
];

fn word_at(toks: &[SqlToken], i: usize) -> Option<&str> {
    toks.get(i).and_then(SqlToken::text)
}

fn bare_at(toks: &[SqlToken], i: usize) -> Option<&str> {
    toks.get(i).and_then(SqlToken::bare)
}

fn is_word(toks: &[SqlToken], i: usize, kw: &str) -> bool {
    bare_at(toks, i).is_some_and(|w| w.eq_ignore_ascii_case(kw))
}

/// Skip a balanced paren group that starts at `start` (which must be a
/// `Punct('(')` token); returns the index just after its `')'`.
fn skip_balanced(toks: &[SqlToken], start: usize) -> usize {
    let mut depth = 0usize;
    let mut i = start;
    while i < toks.len() {
        match toks[i] {
            SqlToken::Punct('(') => depth += 1,
            SqlToken::Punct(')') => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    i
}

/// Names introduced by WITH clauses in the token slice, at any nesting
/// level. Inside this SQL such a name is the CTE, whatever the schema
/// holds under the same name, so these names shadow real relations.
fn collect_cte_names(toks: &[SqlToken], out: &mut BTreeSet<String>) {
    let mut i = 0usize;
    while i < toks.len() {
        if !is_word(toks, i, "with") {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        if is_word(toks, j, "recursive") {
            j += 1;
        }
        // The WITH list: `name [(cols)] AS ( body )` , repeated.
        while let Some(name) = word_at(toks, j) {
            out.insert(name.to_lowercase());
            j += 1;
            if matches!(toks.get(j), Some(SqlToken::Punct('('))) {
                j = skip_balanced(toks, j); // column list
            }
            if !is_word(toks, j, "as") {
                break;
            }
            j += 1;
            if !matches!(toks.get(j), Some(SqlToken::Punct('('))) {
                break;
            }
            let end = skip_balanced(toks, j);
            if end > j + 1 {
                collect_cte_names(&toks[j + 1..end - 1], out); // nested WITH
            }
            j = end;
            if !matches!(toks.get(j), Some(SqlToken::Punct(','))) {
                break;
            }
            j += 1;
        }
        i = j.max(i + 1);
    }
}

/// Candidate relation names from the FROM and JOIN clauses of tokenized
/// SQL. A qualified name `sch.x` contributes `x`. Aliases, string literals
/// and comments contribute nothing. Names are candidates only: the caller
/// resolves them against real schema relations.
fn from_join_candidates(toks: &[SqlToken]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < toks.len() {
        if !is_word(toks, i, "from") && !is_word(toks, i, "join") {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while let Some(name) = word_at(toks, j) {
            let mut name = name.to_string();
            // `sch.name`: the relation is the part after the dot.
            if matches!(toks.get(j + 1), Some(SqlToken::Punct('.')))
                && let Some(qual) = word_at(toks, j + 2)
            {
                name = qual.to_string();
                j += 2;
            }
            out.push(name);
            j += 1;
            if is_word(toks, j, "as") {
                j += 1;
            }
            // An alias: any identifier that is not a clause keyword.
            let alias = match toks.get(j) {
                Some(SqlToken::Quoted(_)) => true,
                Some(SqlToken::Word(w)) => {
                    !NOT_ALIAS.iter().any(|k| w.eq_ignore_ascii_case(k))
                }
                _ => false,
            };
            if alias {
                j += 1;
            }
            if !matches!(toks.get(j), Some(SqlToken::Punct(','))) {
                break;
            }
            j += 1;
        }
        // Resume after the last consumed token, but never skip a token: a
        // JOIN keyword sitting at `j` must still be seen.
        i = j.max(i + 1);
    }
    out
}

fn norm_action(raw: Option<String>) -> Option<String> {
    let v = raw.unwrap_or_default().trim().to_uppercase();
    if v.is_empty() || v == "NO ACTION" {
        None
    } else {
        Some(v)
    }
}

/// Percent-encode a file path for a `file:` URI. Every byte outside the
/// URI unreserved set plus `/` is encoded, so `?`, `#` and spaces in a path
/// cannot change what the URI selects.
fn uri_path(path: &str) -> String {
    let mut out = String::new();
    for b in path.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(*b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Open the database without a write side effect.
///
/// With no `-wal` sidecar the main file holds every committed page, so the
/// file is opened `immutable=1`: SQLite then creates no `-wal`/`-shm` next
/// to the database. When a `-wal` file exists it can hold committed pages,
/// so it is read through a plain read-only open instead — the truth of the
/// schema comes before sidecar avoidance.
fn open_read_only(path: &str) -> Result<rusqlite::Connection, rusqlite::Error> {
    if std::path::Path::new(&format!("{path}-wal")).exists() {
        return rusqlite::Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        );
    }
    let uri = format!("file:{}?immutable=1", uri_path(path));
    rusqlite::Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
}

fn introspect(path: &str, scope: &Scope) -> Result<SchemaGraph, Error> {
    let conn = open_read_only(path).map_err(|e| Error::Sqlite(format!("{path}: {e}")))?;

    let all_tables: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        )
        .map_err(|e| Error::Sqlite(e.to_string()))?
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| Error::Sqlite(e.to_string()))?
        .collect::<Result<_, _>>()
        .map_err(|e| Error::Sqlite(e.to_string()))?;

    let names: Vec<String> = all_tables
        .iter()
        .filter(|n| scope.selects(n))
        .cloned()
        .collect();

    let mut tables = Vec::new();
    let mut edges = Vec::new();

    for name in &names {
        let ident = escape_ident(name);

        // Columns: PRAGMA table_info(cid, name, type, notnull, dflt_value, pk).
        let rows: Vec<(String, String)> = conn
            .prepare(&format!("PRAGMA table_info({ident})"))
            .map_err(|e| Error::Sqlite(e.to_string()))?
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                ))
            })
            .map_err(|e| Error::Sqlite(e.to_string()))?
            .collect::<Result<_, _>>()
            .map_err(|e| Error::Sqlite(e.to_string()))?;
        let columns = rows
            .iter()
            .enumerate()
            .map(|(i, (n, t))| Column {
                ordinal: i,
                name: n.clone(),
                sql_type: normalize_type(t),
            })
            .collect();
        tables.push(TableNode {
            name: name.clone(),
            columns,
        });

        // FK edges from foreign_key_list — independent of PRAGMA foreign_keys.
        let rows: Vec<FkRow> = conn
            .prepare(&format!("PRAGMA foreign_key_list({ident})"))
            .map_err(|e| Error::Sqlite(e.to_string()))?
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                ))
            })
            .map_err(|e| Error::Sqlite(e.to_string()))?
            .collect::<Result<_, _>>()
            .map_err(|e| Error::Sqlite(e.to_string()))?;
        let mut fks: BTreeMap<i64, Vec<(i64, String, Option<String>)>> = BTreeMap::new();
        let mut meta: BTreeMap<i64, (String, String, String)> = BTreeMap::new();
        for (id, seq, table, from, to, on_update, on_delete) in rows {
            fks.entry(id).or_default().push((seq, from, to));
            meta.insert(id, (table, on_update, on_delete));
        }
        for (id, mut rows) in fks {
            rows.sort_by_key(|(seq, _, _)| *seq);
            let cols: Vec<String> = rows.iter().map(|(_, from, _)| from.clone()).collect();
            let ref_cols: Option<Vec<String>> = if rows.iter().all(|(_, _, to)| to.is_none()) {
                None
            } else {
                Some(
                    rows.iter()
                        .map(|(_, _, to)| to.clone().unwrap_or_default())
                        .collect(),
                )
            };
            let (table, on_update, on_delete) = meta.remove(&id).unwrap_or_default();
            edges.push(Edge::reference(
                name.clone(),
                table,
                cols,
                ref_cols,
                norm_action(Some(on_delete)),
                norm_action(Some(on_update)),
            ));
        }

        // Index edges from index_list + index_info.
        let indexes: Vec<String> = conn
            .prepare(&format!("PRAGMA index_list({ident})"))
            .map_err(|e| Error::Sqlite(e.to_string()))?
            .query_map([], |r| r.get::<_, String>(1))
            .map_err(|e| Error::Sqlite(e.to_string()))?
            .collect::<Result<_, _>>()
            .map_err(|e| Error::Sqlite(e.to_string()))?;
        for index in indexes {
            // `sqlite_autoindex_*` is internal bookkeeping SQLite creates
            // for UNIQUE / PRIMARY KEY constraints, not an authored index.
            // Its internals are skipped entirely: no edge names one.
            if index.to_lowercase().starts_with("sqlite_autoindex_") {
                continue;
            }
            let cols: Vec<String> = conn
                .prepare("SELECT name FROM pragma_index_info(?) ORDER BY seqno")
                .map_err(|e| Error::Sqlite(e.to_string()))?
                .query_map([&index], |r| {
                    Ok(r.get::<_, Option<String>>(0)?.unwrap_or_default())
                })
                .map_err(|e| Error::Sqlite(e.to_string()))?
                .collect::<Result<_, _>>()
                .map_err(|e| Error::Sqlite(e.to_string()))?;
            edges.push(Edge::indexed_by(name.clone(), index, cols));
        }
    }

    // Relations in the graph: every table in scope, then every view in
    // scope. A derives edge may point at either; anything else points at
    // nothing that exists.
    let views: Vec<(String, Option<String>)> = conn
        .prepare("SELECT name, sql FROM sqlite_master WHERE type = 'view'")
        .map_err(|e| Error::Sqlite(e.to_string()))?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))
        .map_err(|e| Error::Sqlite(e.to_string()))?
        .collect::<Result<_, _>>()
        .map_err(|e| Error::Sqlite(e.to_string()))?;
    let mut relations: BTreeMap<String, String> = BTreeMap::new();
    for name in &names {
        relations.insert(name.to_lowercase(), name.clone());
    }
    for (view, _) in &views {
        if scope.selects(view) {
            relations.insert(view.to_lowercase(), view.clone());
        }
    }

    // View-derive edges: relation names written in the FROM and JOIN
    // clauses of the stored view SQL, resolved against real relations. Text
    // inside string literals and comments is dropped before parsing, and a
    // candidate that resolves to no relation emits no edge.
    for (view, sql) in &views {
        if !scope.selects(view) {
            continue;
        }
        let Some(sql) = sql else { continue };
        let toks = sql_tokens(sql);
        // A WITH clause shadows a real relation of the same name: inside
        // this SQL the name is the CTE, and a CTE is no node of the graph.
        // A shadowed name never resolves to the table — never both.
        let mut ctes: BTreeSet<String> = BTreeSet::new();
        collect_cte_names(&toks, &mut ctes);
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for candidate in from_join_candidates(&toks) {
            if candidate.eq_ignore_ascii_case(view) {
                continue; // a view does not derive from itself
            }
            if ctes.contains(&candidate.to_lowercase()) {
                continue; // the CTE shadows the relation
            }
            // Unresolvable candidate (CTE, table-valued function, table
            // outside the scope): no edge, never a fabricated one.
            let Some(target) = relations.get(&candidate.to_lowercase()) else {
                continue;
            };
            if seen.insert(target.clone()) {
                edges.push(Edge::derives(view.clone(), target.clone()));
            }
        }
    }

    // Every edge lands on something in the graph. A reference or derives
    // edge whose target relation is excluded or absent is dropped, never
    // left dangling. An indexed_by edge names an index, which is not a
    // relation node; its table side comes from the scope by construction.
    edges.retain(|edge| match edge.kind() {
        EdgeKind::Reference | EdgeKind::Derives => relations.contains_key(&edge.to.to_lowercase()),
        EdgeKind::IndexedBy => true,
    });

    let mut graph = SchemaGraph {
        project: String::new(),
        database: String::new(),
        tables,
        edges,
    };
    graph.sort();
    Ok(graph)
}
