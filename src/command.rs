//! The commands: `generate` introspects, verifies constraints, and writes
//! or checks the one generated view, `graph.toml`; `verify` compares a
//! live database with the committed `graph.toml` and reports drift.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::{dialect, dialect_api, error, model, spec};

/// Arguments of `dbgraph generate`.
pub struct GenerateArgs {
    pub spec_path: Option<PathBuf>,
    pub out_path: Option<PathBuf>,
    pub check: bool,
}

/// Arguments of `dbgraph verify`.
pub struct VerifyArgs {
    pub spec_path: Option<PathBuf>,
    /// Path to the SQLite database file; overrides the `url_env` URL.
    pub db_path: Option<PathBuf>,
    /// Also enforce the spec `no_cycles` constraint on the live graph.
    pub strict: bool,
    /// Print the findings as a JSON report instead of one line per finding.
    pub json: bool,
}

const DEFAULT_SPEC: &str = "dbgraph.toml";
const DEFAULT_OUT: &str = "graph.toml";

/// First bytes of every SQLite database file: `"SQLite format 3\0"`.
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";

/// Header verdict for `path`: `Some(true)` when it is a SQLite database,
/// `Some(false)` when it exists but is proven not to be one, `None` when
/// it exists yet cannot be read to check. A file whose bytes cannot be
/// read is never proven safe.
fn sqlite_header(path: &Path) -> Option<bool> {
    let mut head = [0u8; 16];
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Some(false),
        Err(_) => return None,
    };
    match file.read(&mut head) {
        Ok(n) => Some(n == 16 && head == *SQLITE_HEADER),
        Err(_) => None,
    }
}

/// Why writing a graph to `path` would destroy database content; `None`
/// means writing is safe. Refused: an existing SQLite database, a WAL
/// sidecar name (`<db>-wal` / `<db>-shm`) whose sibling stem is a database
/// — that file can hold the only copy of committed pages — and any
/// existing file whose bytes cannot be read to check them. The check runs
/// before anything is written and never depends on the target being
/// readable.
fn out_target_refusal(path: &Path) -> Option<String> {
    if let Some(name) = path.file_name().and_then(|n| n.to_str())
        && let Some(stem) = name.strip_suffix("-wal").or_else(|| name.strip_suffix("-shm"))
    {
        let sibling = path.with_file_name(stem);
        if sibling.exists() && sqlite_header(&sibling) != Some(false) {
            return Some(format!(
                "refusing to overwrite: this is a SQLite WAL sidecar (-wal/-shm) name of {}; overwriting that sidecar can destroy committed database content",
                sibling.display()
            ));
        }
    }
    if !path.exists() {
        return None;
    }
    match sqlite_header(path) {
        Some(true) => Some(
            "refusing to overwrite: this is a SQLite database file, not a graph file".to_string(),
        ),
        Some(false) => None,
        None => Some(
            "refusing to overwrite: the file exists but cannot be read to check that it is not a SQLite database"
                .to_string(),
        ),
    }
}


/// Run generate (or `--check`). Ok(()) on success; errors map to exit
/// classes by their variant: constraint 2, operational 70, artifact drift 1.
pub async fn generate(args: GenerateArgs) -> Result<(), error::Error> {
    let spec_path = args.spec_path.unwrap_or_else(|| PathBuf::from(DEFAULT_SPEC));
    let out_path = args.out_path.unwrap_or_else(|| PathBuf::from(DEFAULT_OUT));
    let regenerate = format!(
        "dbgraph generate --spec {} --out {}",
        spec_path.display(),
        out_path.display()
    );

    let spec = spec::load(&spec_path)?;
    if spec.database.len() != 1 {
        return Err(error::Error::Operational(format!(
            "expected exactly one [[database]] section, found {}",
            spec.database.len()
        )));
    }
    let db = &spec.database[0];
    let url = spec::url_from_env(db)?;
    let adapter = dialect::build(db, &url)?;
    let mut graph = adapter.introspect(&dialect_api::Scope::from_spec(db)).await?;
    graph.project = spec.project.name.clone();
    graph.database = db.id.clone();
    graph.sort();

    if spec.constraint.iter().any(|c| c.kind == "no_cycles")
        && let Err(nodes) = spec::check_no_cycles(&graph, &spec.allow)
    {
        return Err(error::Error::Constraint(format!(
            "no_cycles: foreign-key cycle through tables: {}",
            nodes.join(", ")
        )));
    }

    let rendered = graph.render_toml()?;
    if args.check {
        match std::fs::read_to_string(&out_path) {
            Ok(current) if current == rendered => {
                println!("{} is fresh", out_path.display());
                Ok(())
            }
            Ok(_) => Err(error::Error::Io(format!(
                "{} is stale; regenerate with: {}",
                out_path.display(),
                regenerate
            ))),
            Err(err) => Err(error::Error::io(&out_path, &err)),
        }
    } else {
        if let Some(reason) = out_target_refusal(&out_path) {
            return Err(error::Error::Operational(format!(
                "{}: {reason}",
                out_path.display()
            )));
        }
        if let Some(parent) = out_path.parent() && !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                error::Error::Operational(format!("{}: {e}", parent.display()))
            })?;
        }
        std::fs::write(&out_path, &rendered).map_err(|e| {
            error::Error::Operational(format!("{}: {e}", out_path.display()))
        })?;
        println!(
            "wrote {} ({} tables, {} edges)",
            out_path.display(),
            graph.tables.len(),
            graph.edges.len()
        );
        Ok(())
    }
}

/// The JSON report of `verify`: the named findings array and nothing else.
/// The exit code carries the verdict, so the JSON carries no verdict prose.
#[derive(serde::Serialize)]
struct Report<'a> {
    findings: &'a [model::Finding],
}

/// Run verify: compare the live database with the committed graph file and
/// report findings on stdout. Writes no file and never mutates the database.
/// `Ok(code)`: 0 without findings, 1 with drift findings (C1, C2, C8),
/// 2 with a constraint finding (C5, under `--strict`). Errors map to exit
/// classes by their variant — constraint 2, operational 70, artifact drift
/// 1 — exactly as in `generate`.
pub async fn verify(args: VerifyArgs) -> Result<i32, error::Error> {
    let spec_path = args.spec_path.unwrap_or_else(|| PathBuf::from(DEFAULT_SPEC));
    let graph_path = PathBuf::from(DEFAULT_OUT);

    let spec = spec::load(&spec_path)?;
    if spec.database.len() != 1 {
        return Err(error::Error::Operational(format!(
            "expected exactly one [[database]] section, found {}",
            spec.database.len()
        )));
    }
    let db = &spec.database[0];
    let url = match &args.db_path {
        Some(path) => path.display().to_string(),
        None => spec::url_from_env(db)?,
    };
    let adapter = dialect::build(db, &url)?;
    let mut live = adapter.introspect(&dialect_api::Scope::from_spec(db)).await?;
    live.project = spec.project.name.clone();
    live.database = db.id.clone();
    live.sort();

    let text = std::fs::read_to_string(&graph_path).map_err(|e| error::Error::io(&graph_path, &e))?;
    let mut committed = model::SchemaGraph::parse_committed(&text, &graph_path)?;
    committed.sort();

    // C1 and C2 name the drifted objects. C8 is the byte-level freshness
    // check — `generate --check` inside verify — so a red run can carry
    // object lines and the file-level staleness line together; nothing a
    // byte difference hides stays unreported.
    let mut findings = model::schema_drift(&committed, &live);
    findings.extend(model::fk_drift(&committed, &live));
    if live.render_toml()? != text {
        findings.push(model::Finding::new(
            "C8",
            "graph_stale",
            graph_path.display().to_string(),
            format!(
                "the committed graph differs from a fresh render byte for byte; regenerate with: dbgraph generate --spec {} --out {}",
                spec_path.display(),
                graph_path.display()
            ),
        ));
    }
    if args.strict
        && spec.constraint.iter().any(|c| c.kind == "no_cycles")
        && let Err(nodes) = spec::check_no_cycles(&live, &spec.allow)
    {
        findings.push(model::Finding::new(
            "C5",
            "fk_cycle",
            nodes.join(", "),
            "foreign-key cycle through tables",
        ));
    }

    if args.json {
        let line = serde_json::to_string(&Report { findings: &findings })
            .map_err(|e| error::Error::Render(e.to_string()))?;
        println!("{line}");
    } else if findings.is_empty() {
        println!(
            "{} matches the database ({} tables, {} edges)",
            graph_path.display(),
            live.tables.len(),
            live.edges.len()
        );
    } else {
        for finding in &findings {
            println!("{finding}");
        }
    }

    Ok(if findings.iter().any(|f| f.check == "C5") {
        crate::EXIT_CONSTRAINT
    } else if findings.is_empty() {
        crate::EXIT_OK
    } else {
        crate::EXIT_ERROR
    })
}
