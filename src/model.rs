//! Canonical schema graph model, deterministic TOML rendering, and the
//! graph-vs-graph drift comparison used by `verify`.
//!
//! `graph.toml` is the one generated view. Every collection inside the
//! graph is sorted, so two renders of the same schema are byte-identical.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// One table with its columns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableNode {
    pub name: String,
    pub columns: Vec<Column>,
}

/// One column. `ordinal` keeps the database column order inside the table.
/// `sql_type` is the normalized type string: uppercase, no inner runs of
/// whitespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Column {
    pub ordinal: usize,
    pub name: String,
    pub sql_type: String,
}

/// Edge kinds in the graph. The `Deserialize` names match `as_str`, so a
/// rendered graph parses back into the same kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// Foreign key: `from` table references `to` table.
    Reference,
    /// View `from` derives from table `to`.
    Derives,
    /// Index `to` on table `from` covers `columns`.
    IndexedBy,
}

impl EdgeKind {
    fn as_str(self) -> &'static str {
        match self {
            EdgeKind::Reference => "reference",
            EdgeKind::Derives => "derives",
            EdgeKind::IndexedBy => "indexed_by",
        }
    }
}

/// One edge of the graph. All fields serialize inline into one table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    #[serde(serialize_with = "kind_str")]
    pub kind: EdgeKind,
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ref_columns: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_delete: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_update: Option<String>,
}

fn kind_str<S: serde::Serializer>(kind: &EdgeKind, s: S) -> Result<S::Ok, S::Error> {
    kind.as_str().serialize(s)
}

impl Edge {
    /// Foreign key edge. `columns` are the referencing columns, ordered.
    pub fn reference(
        from: impl Into<String>,
        to: impl Into<String>,
        columns: Vec<String>,
        ref_columns: Option<Vec<String>>,
        on_delete: Option<String>,
        on_update: Option<String>,
    ) -> Self {
        Self {
            kind: EdgeKind::Reference,
            from: from.into(),
            to: to.into(),
            columns,
            ref_columns,
            on_delete,
            on_update,
        }
    }

    /// View-derives-source edge.
    pub fn derives(view: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            kind: EdgeKind::Derives,
            from: view.into(),
            to: source.into(),
            columns: Vec::new(),
            ref_columns: None,
            on_delete: None,
            on_update: None,
        }
    }

    /// Index-on-table edge.
    pub fn indexed_by(
        table: impl Into<String>,
        index: impl Into<String>,
        columns: Vec<String>,
    ) -> Self {
        Self {
            kind: EdgeKind::IndexedBy,
            from: table.into(),
            to: index.into(),
            columns,
            ref_columns: None,
            on_delete: None,
            on_update: None,
        }
    }

    /// `kind|from|to|columns|ref_columns|actions` key used for sorting.
    /// Duplicate foreign keys on the same columns are separated by their
    /// referenced columns and actions, so declaration order never decides
    /// the rendered bytes.
    pub fn sort_key(&self) -> String {
        let ref_columns = self.ref_columns.as_deref().unwrap_or(&[]).join(",");
        format!(
            "{}|{}|{}|{}|{}|{}|{}",
            self.kind.as_str(),
            self.from,
            self.to,
            self.columns.join(","),
            ref_columns,
            self.on_delete.as_deref().unwrap_or(""),
            self.on_update.as_deref().unwrap_or(""),
        )
    }

    pub fn kind(&self) -> EdgeKind {
        self.kind
    }

    /// Object name for report lines: `orders->customers(id -> customer_id)`.
    /// The referenced-column side appears only when the graph recorded it.
    pub fn describe(&self) -> String {
        let columns = self.columns.join(",");
        match &self.ref_columns {
            Some(rc) => format!("{}->{}({} -> {})", self.from, self.to, columns, rc.join(",")),
            None => format!("{}->{}({})", self.from, self.to, columns),
        }
    }
}

/// The canonical graph for one database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaGraph {
    pub project: String,
    pub database: String,
    pub tables: Vec<TableNode>,
    pub edges: Vec<Edge>,
}

impl SchemaGraph {
    /// Sort tables by name, columns by ordinal, edges by `sort_key`.
    /// Rendering and `--check` call this first, so output is stable.
    pub fn sort(&mut self) {
        self.tables.sort_by(|a, b| a.name.cmp(&b.name));
        for table in &mut self.tables {
            table.columns.sort_by_key(|c| c.ordinal);
        }
        self.edges.sort_by_key(|e| e.sort_key());
    }

    /// Render the canonical graph.toml bytes. Byte-stable for equal graphs.
    pub fn render_toml(&self) -> Result<String, crate::error::Error> {
        toml::to_string_pretty(self).map_err(|e| crate::error::Error::Render(e.to_string()))
    }

    /// Parse a committed graph file. Unknown keys are refused: the file is
    /// a generated view, so a key outside this model means it is not a
    /// graph file. The error names the path.
    pub fn parse_committed(text: &str, path: &Path) -> Result<SchemaGraph, crate::error::Error> {
        toml::from_str(text).map_err(|e| crate::error::Error::Spec(format!("{}: {e}", path.display())))
    }
}

/// One red line of the `verify` report: the check that found it, the
/// object it found, and one factual sentence. No verdict prose: the exit
/// code carries the verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// Check id: `C1`, `C2`, `C5` or `C8`.
    pub check: &'static str,
    /// Machine-readable finding kind inside the check.
    pub kind: &'static str,
    /// The object the finding names: a table, `table.column`, an edge, or a file.
    pub object: String,
    pub detail: String,
}

impl Finding {
    pub(crate) fn new(
        check: &'static str,
        kind: &'static str,
        object: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            check,
            kind,
            object: object.into(),
            detail: detail.into(),
        }
    }
}

/// Human-readable name of a check id, used in report lines.
pub fn check_label(check: &str) -> &'static str {
    match check {
        "C1" => "schema drift",
        "C2" => "fk edge drift",
        "C5" => "no_cycles",
        "C8" => "freshness",
        _ => "finding",
    }
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}: {}: {}",
            self.check,
            check_label(self.check),
            self.object,
            self.detail
        )
    }
}

/// C1 schema drift: added, removed, and changed tables and columns
/// between the committed graph and a fresh render of the live database.
pub fn schema_drift(committed: &SchemaGraph, live: &SchemaGraph) -> Vec<Finding> {
    let mut findings = Vec::new();
    for table in &committed.tables {
        if !live.tables.iter().any(|t| t.name == table.name) {
            findings.push(Finding::new(
                "C1",
                "table_removed",
                &table.name,
                "table is in the graph file, absent from the database",
            ));
        }
    }
    for table in &live.tables {
        if !committed.tables.iter().any(|t| t.name == table.name) {
            findings.push(Finding::new(
                "C1",
                "table_added",
                &table.name,
                "table is in the database, absent from the graph file",
            ));
        }
    }
    for ctable in &committed.tables {
        let Some(ltable) = live.tables.iter().find(|t| t.name == ctable.name) else {
            continue;
        };
        for column in &ctable.columns {
            match ltable.columns.iter().find(|c| c.name == column.name) {
                None => findings.push(Finding::new(
                    "C1",
                    "column_removed",
                    format!("{}.{}", ctable.name, column.name),
                    "column is in the graph file, absent from the database",
                )),
                Some(live_column) if live_column.sql_type != column.sql_type => {
                    findings.push(Finding::new(
                        "C1",
                        "column_type_changed",
                        format!("{}.{}", ctable.name, column.name),
                        format!(
                            "type {} in the graph file, type {} in the database",
                            column.sql_type, live_column.sql_type
                        ),
                    ));
                }
                Some(_) => {}
            }
        }
        for column in &ltable.columns {
            if !ctable.columns.iter().any(|c| c.name == column.name) {
                findings.push(Finding::new(
                    "C1",
                    "column_added",
                    format!("{}.{}", ltable.name, column.name),
                    "column is in the database, absent from the graph file",
                ));
            }
        }
        let order: Vec<&str> = ctable.columns.iter().map(|c| c.name.as_str()).collect();
        let live_order: Vec<&str> = ltable.columns.iter().map(|c| c.name.as_str()).collect();
        if order != live_order
            && order.iter().collect::<BTreeSet<_>>() == live_order.iter().collect::<BTreeSet<_>>()
        {
            findings.push(Finding::new(
                "C1",
                "column_order_changed",
                &ctable.name,
                format!(
                    "column order {} in the graph file, {} in the database",
                    order.join(","),
                    live_order.join(",")
                ),
            ));
        }
    }
    findings
}

/// C2 foreign-key edge drift: reference edges present in only one side.
/// A changed foreign key appears as one removed edge plus one added edge,
/// because its identity is its full key (columns and actions).
pub fn fk_drift(committed: &SchemaGraph, live: &SchemaGraph) -> Vec<Finding> {
    let keys = |graph: &SchemaGraph| -> BTreeSet<String> {
        graph
            .edges
            .iter()
            .filter(|e| e.kind() == EdgeKind::Reference)
            .map(Edge::sort_key)
            .collect()
    };
    let committed_keys = keys(committed);
    let live_keys = keys(live);
    let mut findings = Vec::new();
    for edge in committed
        .edges
        .iter()
        .filter(|e| e.kind() == EdgeKind::Reference && !live_keys.contains(&e.sort_key()))
    {
        findings.push(Finding::new(
            "C2",
            "fk_removed",
            edge.describe(),
            "foreign key is in the graph file, absent from the database",
        ));
    }
    for edge in live
        .edges
        .iter()
        .filter(|e| e.kind() == EdgeKind::Reference && !committed_keys.contains(&e.sort_key()))
    {
        findings.push(Finding::new(
            "C2",
            "fk_added",
            edge.describe(),
            "foreign key is in the database, absent from the graph file",
        ));
    }
    findings
}
