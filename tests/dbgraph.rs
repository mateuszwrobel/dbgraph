//! Hermetic nextest suite. Every SQLite database is created in-test under
//! TMPDIR (default /var/tmp).

use std::{fs, path::Path, path::PathBuf, process::Command};

fn testdir(tag: &str) -> PathBuf {
    let base = std::env::var("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/tmp"));
    let dir = base.join(format!("dbgraph-test-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create test dir");
    dir
}

fn make_db(path: &Path, sql: &str) {
    // PRAGMA foreign_keys stays OFF: enforcement is per-connection, the
    // schema still records the FK. The graph must capture it anyway.
    let conn = rusqlite::Connection::open(path).expect("create db");
    conn.execute_batch(sql).expect("create schema");
}

fn write_spec(dir: &Path, dialect: &str, url_env: &str, extra: &str) -> PathBuf {
    let p = dir.join("dbgraph.toml");
    fs::write(
        &p,
        format!(
            "[project]\nname = \"t\"\n\n[[database]]\nid = \"app\"\ndialect = \"{dialect}\"\nurl_env = \"{url_env}\"\n{extra}"
        ),
    )
    .unwrap();
    p
}

fn dbgraph(dir: &Path, args: &[&str], env: &[(String, String)]) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dbgraph"));
    cmd.args(args).current_dir(dir);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

const RICH_SCHEMA: &str = "
CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
CREATE TABLE orders (
    id INTEGER PRIMARY KEY,
    customer_id INTEGER NOT NULL REFERENCES customers(id)
        ON DELETE CASCADE ON UPDATE RESTRICT,
    total REAL
);
CREATE INDEX idx_orders_customer ON orders(customer_id);
CREATE VIEW active_orders AS SELECT id, total FROM orders WHERE total > 0;
";

fn rich(dir: &Path) -> (PathBuf, Vec<(String, String)>) {
    let db = dir.join("app.db");
    make_db(&db, RICH_SCHEMA);
    let spec = write_spec(dir, "sqlite", "DBGRAPH_TEST_URL", "");
    (spec, vec![("DBGRAPH_TEST_URL".into(), db.display().to_string())])
}

fn edges(out_file: &Path) -> Vec<toml::Table> {
    let text = fs::read_to_string(out_file).unwrap();
    let doc: toml::Table = toml::from_str(&text).unwrap();
    doc["edges"].as_array().unwrap().iter().map(|v| v.as_table().unwrap().clone()).collect()
}

#[test]
fn generate_twice_is_byte_stable() {
    let dir = testdir("stable");
    let (spec, env) = rich(&dir);
    let a = dir.join("a.toml");
    let b = dir.join("b.toml");
    for out in [&a, &b] {
        let o = dbgraph(
            &dir,
            &["generate", "--spec", "dbgraph.toml", "--out", out.to_str().unwrap()],
            &env,
        );
        assert!(o.status.success(), "generate failed: {}", String::from_utf8_lossy(&o.stderr));
    }
    // Real double-generate compare: the second run overwrites the first
    // file, and a third run into a different path must agree byte for byte.
    let first = fs::read(&a).unwrap();
    let o = dbgraph(
        &dir,
        &["generate", "--spec", "dbgraph.toml", "--out", a.to_str().unwrap()],
        &env,
    );
    assert!(o.status.success(), "regenerate failed: {}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(fs::read(&a).unwrap(), first, "overwriting the same path must not change bytes");
    assert_eq!(fs::read(&a).unwrap(), fs::read(&b).unwrap(), "graph.toml bytes differ");
    assert!(spec.exists());
}

#[test]
fn captures_fk_edges_regardless_of_pragma() {
    let dir = testdir("fk");
    let (spec, env) = rich(&dir);
    let out = dir.join("graph.toml");
    let o = dbgraph(
        &dir,
        &["generate", "--spec", "dbgraph.toml", "--out", out.to_str().unwrap()],
        &env,
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let edge = edges(&out)
        .into_iter()
        .find(|e| e["kind"].as_str() == Some("reference"))
        .expect("reference edge missing");
    assert_eq!(edge["from"].as_str().unwrap(), "orders");
    assert_eq!(edge["to"].as_str().unwrap(), "customers");
    assert_eq!(edge["columns"].as_array().unwrap()[0].as_str().unwrap(), "customer_id");
    assert_eq!(edge["ref_columns"].as_array().unwrap()[0].as_str().unwrap(), "id");
    assert_eq!(edge["on_delete"].as_str().unwrap(), "CASCADE");
    assert_eq!(edge["on_update"].as_str().unwrap(), "RESTRICT");
    let _ = spec;
}

#[test]
fn captures_view_derives() {
    let dir = testdir("view");
    let (spec, env) = rich(&dir);
    let out = dir.join("graph.toml");
    let o = dbgraph(
        &dir,
        &["generate", "--spec", "dbgraph.toml", "--out", out.to_str().unwrap()],
        &env,
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let derives: Vec<(String, String)> = edges(&out)
        .into_iter()
        .filter(|e| e["kind"].as_str() == Some("derives"))
        .map(|e| (e["from"].as_str().unwrap().into(), e["to"].as_str().unwrap().into()))
        .collect();
    assert_eq!(derives, vec![("active_orders".to_string(), "orders".to_string())]);
    let _ = spec;
}

#[test]
fn check_fresh_and_stale() {
    let dir = testdir("check");
    let (spec, env) = rich(&dir);
    let out = dir.join("graph.toml");
    let o = dbgraph(
        &dir,
        &["generate", "--spec", "dbgraph.toml", "--out", out.to_str().unwrap()],
        &env,
    );
    assert!(o.status.success());
    // Fresh: exit 0, file untouched.
    let before = fs::read(&out).unwrap();
    let o = dbgraph(
        &dir,
        &["generate", "--spec", "dbgraph.toml", "--out", out.to_str().unwrap(), "--check"],
        &env,
    );
    assert!(o.status.success(), "fresh check must pass: {}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(fs::read(&out).unwrap(), before);
    // Stale: exit 1, message names the regenerate command, file untouched.
    fs::write(&out, "# stale\n").unwrap();
    let o = dbgraph(
        &dir,
        &["generate", "--spec", "dbgraph.toml", "--out", out.to_str().unwrap(), "--check"],
        &env,
    );
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("stale"), "{err}");
    assert!(err.contains("dbgraph generate"), "message must name the regenerate cmd: {err}");
    assert_eq!(fs::read(&out).unwrap(), b"# stale\n", "--check must not write the file");
    let _ = spec;
}

#[test]
fn postgres_dialect_is_rejected() {
    let dir = testdir("pg");
    let spec = write_spec(&dir, "postgres", "DBGRAPH_TEST_URL", "");
    let o = dbgraph(&dir, &["generate", "--spec", "dbgraph.toml"], &[("DBGRAPH_TEST_URL".into(), "postgres://localhost/x".into())]);
    assert_eq!(o.status.code(), Some(70), "an unsupported dialect is operational: the verdict never ran");
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("dialect not yet supported"),
        "stderr: {}", String::from_utf8_lossy(&o.stderr)
    );
    let _ = spec;
}

#[test]
fn turso_dialect_is_rejected() {
    let dir = testdir("turso");
    let spec = write_spec(&dir, "turso", "DBGRAPH_TEST_URL", "");
    let o = dbgraph(&dir, &["generate", "--spec", "dbgraph.toml"], &[("DBGRAPH_TEST_URL".into(), "libsql://x".into())]);
    assert_eq!(o.status.code(), Some(70), "an unsupported dialect is operational: the verdict never ran");
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("dialect not yet supported"),
        "stderr: {}", String::from_utf8_lossy(&o.stderr)
    );
    let _ = spec;
}

/// F1 adversarial: `--out` pointing at the database file itself. The tool
/// must refuse, exit nonzero, and leave the SQLite header intact.
#[test]
fn generate_refuses_to_overwrite_a_database_file() {
    let dir = testdir("noclobber");
    let db = dir.join("app.db");
    make_db(&db, RICH_SCHEMA);
    let spec = write_spec(&dir, "sqlite", "DBGRAPH_TEST_URL", "");
    let before = fs::read(&db).unwrap();
    assert_eq!(&before[..16], b"SQLite format 3\0", "fixture must be a db");
    let o = dbgraph(
        &dir,
        &["generate", "--spec", "dbgraph.toml", "--out", "app.db"],
        &[("DBGRAPH_TEST_URL".into(), db.display().to_string())],
    );
    assert_eq!(o.status.code(), Some(70), "a refusal is operational: the verdict never ran");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("SQLite database"), "clear refusal: {err}");
    assert_eq!(fs::read(&db).unwrap(), before, "database bytes must not change");
    let _ = spec;
}

/// F2 adversarial: a string literal and a comment that name a table must
/// not create derives edges; a real FROM reference still does.
#[test]
fn literals_and_comments_in_view_sql_create_no_edges() {
    let dir = testdir("sqlnoise");
    let db = dir.join("app.db");
    make_db(
        &db,
        "CREATE TABLE plain(id INTEGER PRIMARY KEY);
         CREATE TABLE other(id INTEGER PRIMARY KEY);
         CREATE VIEW v_lit AS SELECT 1 WHERE 'plain'='plain';
         CREATE VIEW v_cmt AS SELECT 1 /* plain */ -- plain
         ;
         CREATE VIEW v_quoted AS SELECT id FROM \"plain\";
         CREATE VIEW v_real AS SELECT id FROM other;",
    );
    let spec = write_spec(&dir, "sqlite", "DBGRAPH_TEST_URL", "");
    let out = dir.join("graph.toml");
    let o = dbgraph(
        &dir,
        &["generate", "--spec", "dbgraph.toml", "--out", out.to_str().unwrap()],
        &[("DBGRAPH_TEST_URL".into(), db.display().to_string())],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let mut derives: Vec<(String, String)> = edges(&out)
        .into_iter()
        .filter(|e| e["kind"].as_str() == Some("derives"))
        .map(|e| (e["from"].as_str().unwrap().into(), e["to"].as_str().unwrap().into()))
        .collect();
    derives.sort();
    assert_eq!(
        derives,
        vec![
            ("v_quoted".to_string(), "plain".to_string()),
            ("v_real".to_string(), "other".to_string()),
        ],
        "only real FROM references may create edges"
    );
    let _ = spec;
}

/// F3 adversarial: a typo'd scope key must fail with the key named, never
/// silently widen the scope.
#[test]
fn unknown_spec_key_fails_with_named_key() {
    let dir = testdir("typo");
    let (spec, env) = rich(&dir);
    let typo = dir.join("typo.toml");
    fs::write(
        &typo,
        "[project]\nname = \"t\"\n\n[[database]]\nid = \"app\"\ndialect = \"sqlite\"\nurl_env = \"DBGRAPH_TEST_URL\"\ninclud = [\"customers\"]\n",
    )
    .unwrap();
    let out = dir.join("graph.toml");
    let o = dbgraph(
        &dir,
        &["generate", "--spec", "typo.toml", "--out", out.to_str().unwrap()],
        &env,
    );
    assert_eq!(o.status.code(), Some(70), "an invalid spec is operational, never drift");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("includ"), "error must name the key: {err}");
    assert!(!out.exists(), "no graph may be written from a bad spec");
    let _ = spec;
}

/// F4 adversarial: reading a WAL-mode database must not leave `-wal` or
/// `-shm` files next to it, and the database bytes must not change.
#[test]
fn reading_a_wal_database_leaves_no_sidecar_files() {
    let dir = testdir("wal");
    let db = dir.join("app.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE t(a);")
        .unwrap();
    drop(conn); // last connection close checkpoints and removes the sidecars
    assert!(!Path::new(&format!("{}-wal", db.display())).exists(), "fixture must close clean");
    let before = fs::read(&db).unwrap();
    let spec = write_spec(&dir, "sqlite", "DBGRAPH_TEST_URL", "");
    let out = dir.join("graph.toml");
    let o = dbgraph(
        &dir,
        &["generate", "--spec", "dbgraph.toml", "--out", out.to_str().unwrap()],
        &[("DBGRAPH_TEST_URL".into(), db.display().to_string())],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!Path::new(&format!("{}-wal", db.display())).exists(), "no -wal may appear");
    assert!(!Path::new(&format!("{}-shm", db.display())).exists(), "no -shm may appear");
    assert_eq!(fs::read(&db).unwrap(), before, "database bytes must not change");
    let _ = spec;
}

/// F5 adversarial: two databases that differ only in the declaration order
/// of two identical-column foreign keys must render identical bytes.
#[test]
fn duplicate_fk_declaration_order_does_not_change_bytes() {
    let dir = testdir("dupid");
    let one = dir.join("one.db");
    let two = dir.join("two.db");
    let head = "CREATE TABLE plain(id INTEGER PRIMARY KEY);\nCREATE TABLE dupfk(a INTEGER,\n";
    make_db(
        &one,
        &format!(
            "{head}    FOREIGN KEY(a) REFERENCES plain(id) ON DELETE CASCADE ON UPDATE SET NULL,\n    FOREIGN KEY(a) REFERENCES plain(id) ON DELETE SET NULL ON UPDATE CASCADE\n);"
        ),
    );
    make_db(
        &two,
        &format!(
            "{head}    FOREIGN KEY(a) REFERENCES plain(id) ON DELETE SET NULL ON UPDATE CASCADE,\n    FOREIGN KEY(a) REFERENCES plain(id) ON DELETE CASCADE ON UPDATE SET NULL\n);"
        ),
    );
    let spec = write_spec(&dir, "sqlite", "DBGRAPH_TEST_URL", "");
    let mut bytes = Vec::new();
    for (tag, db) in [("one", &one), ("two", &two)] {
        let out = dir.join(format!("{tag}.toml"));
        let o = dbgraph(
            &dir,
            &["generate", "--spec", "dbgraph.toml", "--out", out.to_str().unwrap()],
            &[("DBGRAPH_TEST_URL".into(), db.display().to_string())],
        );
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        bytes.push(fs::read(&out).unwrap());
    }
    assert_eq!(bytes[0], bytes[1], "declaration order must not leak into bytes");
    let _ = spec;
}

/// F7 adversarial: excluding a table must also drop the edges that point
/// at it; no edge may name a node the graph does not have.
#[test]
fn edges_to_excluded_tables_are_dropped() {
    let dir = testdir("excl");
    let db = dir.join("app.db");
    make_db(
        &db,
        "CREATE TABLE plain(id INTEGER PRIMARY KEY);
         CREATE TABLE orders(id INTEGER PRIMARY KEY, p INTEGER REFERENCES plain(id));
         CREATE VIEW v AS SELECT id FROM plain;",
    );
    let spec = write_spec(&dir, "sqlite", "DBGRAPH_TEST_URL", "exclude = [\"plain\"]\n");
    let out = dir.join("graph.toml");
    let o = dbgraph(
        &dir,
        &["generate", "--spec", "dbgraph.toml", "--out", out.to_str().unwrap()],
        &[("DBGRAPH_TEST_URL".into(), db.display().to_string())],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let doc: toml::Table = toml::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    let nodes: Vec<&str> = doc["tables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(!nodes.contains(&"plain"), "plain must be out of the graph");
    for edge in edges(&out) {
        let kind = edge["kind"].as_str().unwrap();
        let to = edge["to"].as_str().unwrap();
        if kind == "indexed_by" {
            continue; // the target of an index edge is an index, not a node
        }
        assert!(nodes.contains(&to), "{kind} edge points at missing node {to:?}");
    }
    assert!(!fs::read_to_string(&out).unwrap().contains("\"plain\""), "no edge may name plain");
    let _ = spec;
}

/// F8 adversarial: a view over a view must produce both derives edges.
#[test]
fn view_over_view_derives_edge() {
    let dir = testdir("vv");
    let db = dir.join("app.db");
    make_db(
        &db,
        "CREATE TABLE t(a INTEGER);
         CREATE VIEW v1 AS SELECT a FROM t;
         CREATE VIEW v2 AS SELECT a FROM v1;",
    );
    let spec = write_spec(&dir, "sqlite", "DBGRAPH_TEST_URL", "");
    let out = dir.join("graph.toml");
    let o = dbgraph(
        &dir,
        &["generate", "--spec", "dbgraph.toml", "--out", out.to_str().unwrap()],
        &[("DBGRAPH_TEST_URL".into(), db.display().to_string())],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let mut derives: Vec<(String, String)> = edges(&out)
        .into_iter()
        .filter(|e| e["kind"].as_str() == Some("derives"))
        .map(|e| (e["from"].as_str().unwrap().into(), e["to"].as_str().unwrap().into()))
        .collect();
    derives.sort();
    assert_eq!(
        derives,
        vec![
            ("v1".to_string(), "t".to_string()),
            ("v2".to_string(), "v1".to_string()),
        ]
    );
    let _ = spec;
}

/// F9 adversarial: a wrong command line exits EX_USAGE (64), never the
/// constraint code (2). A `help` topic typo is a usage mistake too.
#[test]
fn usage_errors_exit_64() {
    let dir = testdir("usage");
    let o = dbgraph(&dir, &[], &[]);
    assert_eq!(o.status.code(), Some(64), "no arguments must exit EX_USAGE");
    let o = dbgraph(&dir, &["--bogus"], &[]);
    assert_eq!(o.status.code(), Some(64), "unknown flag must exit EX_USAGE");
    let o = dbgraph(&dir, &["generate", "--spec"], &[]);
    assert_eq!(o.status.code(), Some(64), "missing value must exit EX_USAGE");
    let o = dbgraph(&dir, &["help", "zzz-not-a-topic"], &[]);
    assert_eq!(o.status.code(), Some(64), "a help typo must exit EX_USAGE, never a verdict code");
    // Help is not an error: it exits 0.
    let o = dbgraph(&dir, &["--help"], &[]);
    assert_eq!(o.status.code(), Some(0), "--help must exit 0");
}

/// Family exit taxonomy: a missing spec file is an operational-precondition
/// failure (exit 70) on every verdict path — never drift (1), never a
/// constraint (2). The error names the file.
#[test]
fn missing_spec_exits_operational_70() {
    let dir = testdir("missing-spec");
    let missing = "absent-spec.dbgraph.toml";
    for args in [
        vec!["verify", "--spec", missing],
        vec!["generate", "--spec", missing],
        vec!["generate", "--check", "--spec", missing],
    ] {
        let o = dbgraph(&dir, &args, &[]);
        assert_eq!(
            o.status.code(),
            Some(70),
            "{args:?} with a missing spec must exit operational 70: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        assert!(
            String::from_utf8_lossy(&o.stderr).contains(missing),
            "{args:?} must name the missing spec file: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// F4 guard: the immutable open must never hide committed content. With a
/// live `-wal` file the schema inside it stays visible.
#[test]
fn committed_content_in_a_live_wal_stays_visible() {
    let dir = testdir("wallive");
    let db = dir.join("app.db");
    let writer = rusqlite::Connection::open(&db).unwrap();
    writer
        .execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE t(a); CREATE TABLE in_wal(x);")
        .unwrap();
    assert!(
        Path::new(&format!("{}-wal", db.display())).exists(),
        "the writer must still hold a -wal file"
    );
    let spec = write_spec(&dir, "sqlite", "DBGRAPH_TEST_URL", "");
    let out = dir.join("graph.toml");
    let o = dbgraph(
        &dir,
        &["generate", "--spec", "dbgraph.toml", "--out", out.to_str().unwrap()],
        &[("DBGRAPH_TEST_URL".into(), db.display().to_string())],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let text = fs::read_to_string(&out).unwrap();
    assert!(text.contains("in_wal"), "committed schema in the -wal must be visible: {text}");
    drop(writer);
    let _ = spec;
}

/// N1 adversarial: `--out` onto an existing file that cannot be read
/// (mode 200) must be refused before any write, not truncated via the
/// write-only descriptor. The bytes must survive.
#[test]
fn generate_refuses_an_existing_file_it_cannot_read() {
    use std::os::unix::fs::PermissionsExt;
    let dir = testdir("woclobber");
    let (spec, env) = rich(&dir);
    let target = dir.join("wo.db");
    fs::write(&target, b"precious bytes\n").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o200)).unwrap();
    let o = dbgraph(
        &dir,
        &["generate", "--spec", "dbgraph.toml", "--out", "wo.db"],
        &env,
    );
    // Restore read permission to inspect what the run left behind.
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(o.status.code(), Some(70), "an unreadable existing file must be refused (operational)");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("refusing"), "clear refusal: {err}");
    assert_eq!(fs::read(&target).unwrap(), b"precious bytes\n", "file bytes must not change");
    let _ = spec;
}

/// N2 adversarial: `--out` onto a WAL sidecar of a database must be
/// refused — by name once the sibling stem is a database — and the
/// sidecar bytes must survive. A `-wal`-suffixed name with no database
/// sibling stays writable: the rule is the sidecar risk, not the suffix.
#[test]
fn generate_refuses_a_wal_sidecar_target() {
    let dir = testdir("sidecar");
    let db = dir.join("w.db");
    make_db(&db, "CREATE TABLE walc(x);");
    let wal = dir.join("w.db-wal");
    fs::write(&wal, b"wal payload: committed pages\n").unwrap();
    let spec = write_spec(&dir, "sqlite", "DBGRAPH_TEST_URL", "");
    let env = vec![("DBGRAPH_TEST_URL".to_string(), db.display().to_string())];
    let o = dbgraph(&dir, &["generate", "--spec", "dbgraph.toml", "--out", "w.db-wal"], &env);
    assert_eq!(o.status.code(), Some(70), "a live -wal sidecar must be refused (operational)");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("sidecar"), "refusal must name the sidecar risk: {err}");
    assert_eq!(fs::read(&wal).unwrap(), b"wal payload: committed pages\n");
    // The read-through path may recreate a -shm next to a db that shows a
    // -wal (documented read-side behavior). Clear both sidecars so the
    // next step tests only the --out name rule.
    let _ = fs::remove_file(&wal);
    let _ = fs::remove_file(dir.join("w.db-shm"));
    // A -shm sidecar that does not exist yet is refused by name too:
    // writing it would plant a sidecar next to a live database.
    let o = dbgraph(&dir, &["generate", "--spec", "dbgraph.toml", "--out", "w.db-shm"], &env);
    assert_eq!(o.status.code(), Some(70), "a -shm name of an existing db must be refused (operational)");
    assert!(!dir.join("w.db-shm").exists(), "no sidecar may be created");
    // Control: the same suffix with no database sibling is not a sidecar.
    let o = dbgraph(&dir, &["generate", "--spec", "dbgraph.toml", "--out", "notes-wal"], &env);
    assert!(o.status.success(), "a non-sidecar -wal-suffixed name stays writable: {}", String::from_utf8_lossy(&o.stderr));
    assert!(dir.join("notes-wal").exists());
    let _ = spec;
}

/// F6 adversarial: UNIQUE and PRIMARY KEY constraints make SQLite create
/// `sqlite_autoindex_*` internals. No edge may name one; authored indexes
/// still produce edges.
#[test]
fn autoindexes_emit_no_edges() {
    let dir = testdir("autoidx");
    let db = dir.join("app.db");
    make_db(
        &db,
        "CREATE TABLE u(id INTEGER PRIMARY KEY, k TEXT NOT NULL UNIQUE);
         CREATE TABLE pk2(a INTEGER, b INTEGER, PRIMARY KEY(a, b));
         CREATE INDEX idx_u_k ON u(k);",
    );
    let spec = write_spec(&dir, "sqlite", "DBGRAPH_TEST_URL", "");
    let out = dir.join("graph.toml");
    let o = dbgraph(
        &dir,
        &["generate", "--spec", "dbgraph.toml", "--out", out.to_str().unwrap()],
        &[("DBGRAPH_TEST_URL".into(), db.display().to_string())],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let text = fs::read_to_string(&out).unwrap();
    assert!(
        !text.contains("sqlite_autoindex"),
        "no edge may name an autoindex internal: {text}"
    );
    let indexed: Vec<String> = edges(&out)
        .into_iter()
        .filter(|e| e["kind"].as_str() == Some("indexed_by"))
        .map(|e| e["to"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(indexed, vec!["idx_u_k".to_string()], "authored indexes stay edges");
    let _ = spec;
}

/// N3 adversarial: a CTE name that shadows a real table must shadow, not
/// augment — the view reads the CTE, so no edge to the table, while every
/// other real relation keeps its edge.
#[test]
fn cte_name_shadows_a_real_table() {
    let dir = testdir("cteshadow");
    let db = dir.join("app.db");
    make_db(
        &db,
        "CREATE TABLE plain(id INTEGER PRIMARY KEY);
         CREATE TABLE other(id INTEGER PRIMARY KEY);
         CREATE VIEW s6 AS WITH plain AS (SELECT 1 AS x) SELECT x FROM plain;
         CREATE VIEW s7 AS WITH plain AS (SELECT id AS x FROM other) SELECT x FROM plain;
         CREATE VIEW s8 AS WITH t AS (SELECT 1 AS y) SELECT y FROM t, plain;",
    );
    let spec = write_spec(&dir, "sqlite", "DBGRAPH_TEST_URL", "");
    let out = dir.join("graph.toml");
    let o = dbgraph(
        &dir,
        &["generate", "--spec", "dbgraph.toml", "--out", out.to_str().unwrap()],
        &[("DBGRAPH_TEST_URL".into(), db.display().to_string())],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let mut derives: Vec<(String, String)> = edges(&out)
        .into_iter()
        .filter(|e| e["kind"].as_str() == Some("derives"))
        .map(|e| (e["from"].as_str().unwrap().into(), e["to"].as_str().unwrap().into()))
        .collect();
    derives.sort();
    assert_eq!(
        derives,
        vec![
            ("s7".to_string(), "other".to_string()),
            ("s8".to_string(), "plain".to_string()),
        ],
        "a shadowed table gets no fabricated edge; real relations keep theirs"
    );
    let _ = spec;
}

/// N5: `--version` and `-V` print the version and exit 0.
#[test]
fn version_flag_exits_zero() {
    let dir = testdir("version");
    for flag in ["--version", "-V"] {
        let o = dbgraph(&dir, &[flag], &[]);
        assert_eq!(o.status.code(), Some(0), "{flag} must exit 0");
        let text = String::from_utf8_lossy(&o.stdout);
        assert!(
            text.contains(env!("CARGO_PKG_VERSION")),
            "{flag} must print the crate version: {text}"
        );
    }
}

/// The built-in manual: every known topic prints text and exits 0, the topic
/// index exits 0, and an unknown topic is a usage error (exit 64, the family
/// contract). All help runs happen in an empty directory: `help` must create
/// no files and touch no database.
#[test]
fn help_topics_exit_zero_unknown_exits_64() {
    let dir = testdir("help");
    for topic in ["commands", "exit-codes", "configuration", "diagnostics"] {
        let o = dbgraph(&dir, &["help", topic], &[]);
        assert_eq!(o.status.code(), Some(0), "help {topic} must exit 0");
        assert!(!o.stdout.is_empty(), "help {topic} must print text");
    }
    // The index prints with no topic and with the `topics` alias.
    for o in [dbgraph(&dir, &["help"], &[]), dbgraph(&dir, &["help", "topics"], &[])] {
        assert_eq!(o.status.code(), Some(0), "help index must exit 0");
        let text = String::from_utf8_lossy(&o.stdout);
        assert!(text.contains("topics:"), "index must list topics: {text}");
    }
    // A help typo is a usage mistake, never a verdict code: exit 64.
    let o = dbgraph(&dir, &["help", "nonexistent-topic"], &[]);
    assert_eq!(o.status.code(), Some(64), "unknown help topic must exit 64");
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("unknown help topic"),
        "unknown topic must name the problem on stderr"
    );
    // No side effects: help created nothing in the directory.
    let entries: Vec<_> = std::fs::read_dir(&dir).expect("dir readable").collect();
    assert!(entries.is_empty(), "help must not create files: {entries:?}");
}

/// A `no_cycles` constraint over a foreign-key cycle fails with the constraint
/// exit code (2), never the error code (1): the graph built, the rule rejected it.
#[test]
fn no_cycles_constraint_exits_two() {
    let dir = testdir("nocycle");
    let db = dir.join("app.db");
    // Two tables reference each other: a reference-edge cycle. Foreign-key
    // enforcement stays OFF in the fixture, but the FK clauses are still
    // recorded and the graph must capture both edges.
    make_db(
        &db,
        "CREATE TABLE a (id INTEGER PRIMARY KEY, b_id INTEGER REFERENCES b(id));
         CREATE TABLE b (id INTEGER PRIMARY KEY, a_id INTEGER REFERENCES a(id));",
    );
    let spec = write_spec(
        &dir,
        "sqlite",
        "DBGRAPH_TEST_URL",
        "\n[[constraint]]\nkind = \"no_cycles\"\n",
    );
    let o = dbgraph(
        &dir,
        &["generate", "--spec", "dbgraph.toml"],
        &[("DBGRAPH_TEST_URL".into(), db.display().to_string())],
    );
    let stderr = String::from_utf8_lossy(&o.stderr).to_string();
    assert_eq!(o.status.code(), Some(2), "a foreign-key cycle must exit 2: {stderr}");
    assert!(stderr.contains("no_cycles"), "stderr: {stderr}");
    let _ = spec;
}

/// decision-check F-3 guard: the cli->adapters allowance in
/// architecture.spec.toml maps the real edges `command -> dialect` and
/// `command -> dialect_api`. The
/// other half (`command -> sqlite`) has no real edge and archspec cannot
/// declare a per-module allowance, so the seam is sealed here: if dispatch
/// ever imports the concrete backend, the dialect::build choke point is gone
/// and this test fails the commit.
#[test]
fn command_never_names_the_sqlite_adapter() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/command.rs"))
        .expect("read src/command.rs");
    for banned in ["crate::sqlite", "dbgraph::sqlite"] {
        assert!(
            !src.contains(banned),
            "command.rs must construct adapters only via dialect::build; found {banned}"
        );
    }
}

/// decision-check F-2 guard: the `help exit-codes` topic must enumerate
/// exactly the exit-code constants the library publishes — a documented code
/// with no constant, or a constant absent from the manual, fails here.
/// Enumerating both directions also pins the taxonomy: a retired code (the
/// former help-typo 3) may never survive in the manual.
#[test]
fn exit_codes_topic_enumerates_the_contract() {
    let codes = [
        dbgraph::EXIT_OK,
        dbgraph::EXIT_ERROR,
        dbgraph::EXIT_CONSTRAINT,
        dbgraph::EXIT_USAGE,
        dbgraph::EXIT_OPERATIONAL,
    ];
    for code in codes {
        assert!(
            dbgraph::help::EXIT_CODES
                .lines()
                .filter(|l| l.trim_start().starts_with(&code.to_string()))
                .any(|l| l.trim_start().starts_with(&format!("{code} "))),
            "exit-codes topic must document code {code}"
        );
    }
    for line in dbgraph::help::EXIT_CODES.lines() {
        let line = line.trim_start();
        let first = match line.split_whitespace().next() {
            Some(word) if !word.is_empty() && word.chars().all(|c| c.is_ascii_digit()) => word,
            _ => continue,
        };
        if !line[first.len()..].starts_with(' ') {
            continue;
        }
        let code: i32 = first.parse().expect("documented code is numeric");
        assert!(
            codes.contains(&code),
            "exit-codes topic documents code {code} with no constant: {line}"
        );
    }
}

/// decision-check F-2 guard, second half: every long flag the parser accepts
/// for `generate` and `verify` must appear in the `help commands` topic —
/// drift between clap and the manual fails the commit.
#[test]
fn help_commands_topic_lists_every_command_flag() {
    let dir = testdir("helpdrift");
    for command in ["generate", "verify"] {
        let o = dbgraph(&dir, &[command, "--help"], &[]);
        assert_eq!(o.status.code(), Some(0));
        let help = String::from_utf8_lossy(&o.stdout).to_string();
        for word in help.split_whitespace() {
            let flag = word.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '-');
            if flag.starts_with("--") && flag != "--help" {
                assert!(
                    dbgraph::help::COMMANDS.contains(flag),
                    "flag {flag} accepted by {command} but missing from help commands"
                );
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- verify ----

/// Generate the committed graph.toml inside `dir` and return its path.
fn generate_committed(dir: &Path, env: &[(String, String)]) -> PathBuf {
    let o = dbgraph(dir, &["generate"], env);
    assert!(o.status.success(), "generate failed: {}", String::from_utf8_lossy(&o.stderr));
    dir.join("graph.toml")
}

fn json_findings(out: &std::process::Output) -> Vec<serde_json::Value> {
    let doc: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("--json stdout must be one json object");
    doc["findings"].as_array().expect("findings must be an array").clone()
}

/// A line of the text report naming a check id and an object.
fn finding_line(out: &std::process::Output, check: &str, object: &str) -> String {
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    text.lines()
        .find(|l| l.starts_with(&format!("{check} ")) && l.contains(object))
        .unwrap_or_else(|| panic!("expected a {check} line naming {object:?} in: {text}"))
        .to_string()
}

/// Green path: exit 0, one green line, zero json findings, and the graph
/// file stays byte-identical — verify writes nothing.
#[test]
fn verify_green_path_is_byte_clean() {
    let dir = testdir("verify-green");
    let (_spec, env) = rich(&dir);
    let graph = generate_committed(&dir, &env);
    let before = fs::read(&graph).unwrap();
    let o = dbgraph(&dir, &["verify"], &env);
    assert_eq!(o.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&o.stderr));
    let text = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(text.contains("graph.toml matches the database"), "green line: {text}");
    assert_eq!(fs::read(&graph).unwrap(), before, "verify must not write the graph file");
    let o = dbgraph(&dir, &["verify", "--json"], &env);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(json_findings(&o).len(), 0, "green json reports no findings");
}

/// C1 fixture: a column dropped from the database is red, names the check
/// id and the object, and exits 1. The fixture has no view: SQLite would
/// refuse to drop a column a stored view still names.
#[test]
fn verify_detects_dropped_column() {
    let dir = testdir("verify-column");
    let db = dir.join("app.db");
    make_db(
        &db,
        "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
         CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER NOT NULL REFERENCES customers(id), total REAL);",
    );
    let spec = write_spec(&dir, "sqlite", "DBGRAPH_TEST_URL", "");
    let env = vec![("DBGRAPH_TEST_URL".to_string(), db.display().to_string())];
    generate_committed(&dir, &env);
    make_db(&db, "ALTER TABLE orders DROP COLUMN total;");
    let o = dbgraph(&dir, &["verify"], &env);
    assert_eq!(o.status.code(), Some(1), "stderr: {}", String::from_utf8_lossy(&o.stderr));
    let line = finding_line(&o, "C1", "orders.total");
    assert!(line.contains("dropped") || line.contains("graph file"), "line names the drift: {line}");
    let o = dbgraph(&dir, &["verify", "--json"], &env);
    assert_eq!(o.status.code(), Some(1));
    let c1 = json_findings(&o)
        .into_iter()
        .find(|f| f["check"] == "C1" && f["object"] == "orders.total")
        .expect("json finding C1 for orders.total");
    assert_eq!(c1["kind"], "column_removed");
    let _ = spec;
}

/// C2 fixture: a foreign key added to the database is red, names the edge,
/// and exits 1.
#[test]
fn verify_detects_added_fk() {
    let dir = testdir("verify-fk");
    let (_spec, env) = rich(&dir);
    generate_committed(&dir, &env);
    make_db(
        &dir.join("app.db"),
        "CREATE TABLE payments(id INTEGER PRIMARY KEY, order_id INTEGER NOT NULL REFERENCES orders(id));",
    );
    let o = dbgraph(&dir, &["verify"], &env);
    assert_eq!(o.status.code(), Some(1), "stderr: {}", String::from_utf8_lossy(&o.stderr));
    let line = finding_line(&o, "C2", "payments->orders");
    assert!(line.contains("foreign key"), "line names the fk drift: {line}");
    let o = dbgraph(&dir, &["verify", "--json"], &env);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        json_findings(&o).iter().any(|f| f["check"] == "C2" && f["kind"] == "fk_added"),
        "json finding C2 fk_added missing"
    );
}

/// C5 fixture: an introduced foreign-key cycle is red with the constraint
/// exit code (2) under `--strict`, and never red without it — a schema
/// change that keeps graph and database equal is not drift.
#[test]
fn verify_cycle_is_red_only_under_strict() {
    let dir = testdir("verify-cycle");
    let db = dir.join("app.db");
    make_db(
        &db,
        "CREATE TABLE a (id INTEGER PRIMARY KEY, b_id INTEGER REFERENCES b(id));
         CREATE TABLE b (id INTEGER PRIMARY KEY, a_id INTEGER REFERENCES a(id));",
    );
    let plain = write_spec(&dir, "sqlite", "DBGRAPH_TEST_URL", "");
    let env = vec![("DBGRAPH_TEST_URL".to_string(), db.display().to_string())];
    let graph = generate_committed(&dir, &env);
    let before = fs::read(&graph).unwrap();
    // The constraint lands in the spec after the graph was committed.
    let strict = dir.join("strict.toml");
    fs::write(
        &strict,
        "[project]\nname = \"t\"\n\n[[database]]\nid = \"app\"\ndialect = \"sqlite\"\nurl_env = \"DBGRAPH_TEST_URL\"\n\n[[constraint]]\nkind = \"no_cycles\"\n",
    )
    .unwrap();
    let o = dbgraph(&dir, &["verify", "--spec", "strict.toml"], &env);
    assert_eq!(o.status.code(), Some(0), "without --strict a cycle is no drift: {}", String::from_utf8_lossy(&o.stderr));
    let o = dbgraph(&dir, &["verify", "--spec", "strict.toml", "--strict"], &env);
    assert_eq!(o.status.code(), Some(2), "a cycle must exit the constraint code: {o:?}");
    let line = finding_line(&o, "C5", "a, b");
    assert!(line.contains("cycle"), "line names the cycle: {line}");
    assert_eq!(fs::read(&graph).unwrap(), before, "verify must not write the graph file");
    let _ = plain;
}

/// C6 fixture: an `[[allow]]` declaration is honored — the same cycle under
/// `--strict` stays green when an allow rule exempts one of its edges.
#[test]
fn verify_honours_allow_declarations() {
    let dir = testdir("verify-allow");
    let db = dir.join("app.db");
    make_db(
        &db,
        "CREATE TABLE a (id INTEGER PRIMARY KEY, b_id INTEGER REFERENCES b(id));
         CREATE TABLE b (id INTEGER PRIMARY KEY, a_id INTEGER REFERENCES a(id));",
    );
    let spec = write_spec(
        &dir,
        "sqlite",
        "DBGRAPH_TEST_URL",
        "\n[[constraint]]\nkind = \"no_cycles\"\n\n[[allow]]\nkind = \"edge\"\nfrom = \"b\"\nto = \"a\"\n",
    );
    let env = vec![("DBGRAPH_TEST_URL".to_string(), db.display().to_string())];
    generate_committed(&dir, &env);
    let o = dbgraph(&dir, &["verify", "--strict"], &env);
    assert_eq!(o.status.code(), Some(0), "an allowed edge must not make the cycle red: stdout {}", String::from_utf8_lossy(&o.stdout));
    let _ = spec;
}

/// C8 fixture: bytes changed in the graph file with no object-level drift
/// are still red, and the red line names the regenerate command.
#[test]
fn verify_reports_stale_graph_bytes() {
    let dir = testdir("verify-stale");
    let (_spec, env) = rich(&dir);
    let graph = generate_committed(&dir, &env);
    fs::write(&graph, format!("{}\n# hand edit\n", fs::read_to_string(&graph).unwrap())).unwrap();
    let o = dbgraph(&dir, &["verify"], &env);
    assert_eq!(o.status.code(), Some(1));
    let line = finding_line(&o, "C8", "graph.toml");
    assert!(line.contains("dbgraph generate"), "line names the regenerate command: {line}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Fail-closed: `verify` refuses the dialects without an adapter, even when
/// `--db` names a readable SQLite file — the spec dialect decides, never
/// the file. A wrong dialect is operational (exit 70) and never inspects
/// the database.
#[test]
fn verify_refuses_unsupported_dialects() {
    let dir = testdir("verify-dialect");
    let db = dir.join("app.db");
    make_db(&db, RICH_SCHEMA);
    for dialect in ["postgres", "turso"] {
        let spec = write_spec(&dir, dialect, "DBGRAPH_TEST_URL", "");
        let o = dbgraph(
            &dir,
            &["verify", "--db", "app.db"],
            &[("DBGRAPH_TEST_URL".into(), "unused".into())],
        );
        assert_eq!(o.status.code(), Some(70), "{dialect} must be refused (operational): {o:?}");
        assert!(
            String::from_utf8_lossy(&o.stderr).contains("dialect not yet supported"),
            "{dialect} refusal must say why: {}", String::from_utf8_lossy(&o.stderr)
        );
        let _ = spec;
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `--db` points at the database directly and overrides the url_env
/// variable; without `--db` the unset variable fails as in generate.
#[test]
fn verify_db_flag_overrides_url_env() {
    let dir = testdir("verify-dbflag");
    let (spec, env) = rich(&dir);
    generate_committed(&dir, &env);
    let o = dbgraph(&dir, &["verify", "--db", "app.db"], &[]);
    assert_eq!(o.status.code(), Some(0), "--db must connect without the env var: {}", String::from_utf8_lossy(&o.stderr));
    let o = dbgraph(&dir, &["verify"], &[]);
    assert_eq!(o.status.code(), Some(70), "no --db and no env var is an operational environment failure, never drift");
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("DBGRAPH_TEST_URL"),
        "error must name the variable: {}", String::from_utf8_lossy(&o.stderr)
    );
    let _ = spec;
}

/// Errors before any finding: a missing or unparseable graph file is an
/// error (exit 1) naming the file, never an empty green report.
#[test]
fn verify_graph_file_errors_are_named() {
    let dir = testdir("verify-graphfile");
    let (_spec, env) = rich(&dir);
    let o = dbgraph(&dir, &["verify"], &env);
    assert_eq!(o.status.code(), Some(1), "a missing graph.toml must fail: {o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("graph.toml"), "error must name the file");
    fs::write(dir.join("graph.toml"), "project = 3\n").unwrap();
    let o = dbgraph(&dir, &["verify"], &env);
    assert_eq!(o.status.code(), Some(1), "an unparseable graph.toml must fail: {o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("graph.toml"), "error must name the file");
    // A wrong command line for verify is a usage error, not a verdict.
    let o = dbgraph(&dir, &["verify", "--nope"], &env);
    assert_eq!(o.status.code(), Some(64), "unknown flag must exit 64");
    let _ = std::fs::remove_dir_all(&dir);
}

/// decision-check F-6 guard: the `help` module's boundary claim is "pure
/// output". If it ever imports crate internals, the boundary note in
/// architecture.spec.toml goes stale silently — this test fails instead.
#[test]
fn help_module_is_pure_output() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/help.rs"))
        .expect("read src/help.rs");
    for banned in ["use crate::", "crate::"] {
        assert!(
            !src.contains(banned),
            "src/help.rs must be pure output; found {banned}"
        );
    }
}

