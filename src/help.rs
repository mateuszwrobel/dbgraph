//! The built-in manual: `dbgraph help [topic]`.
//!
//! This module is pure output. It reads no files, opens no database
//! connection, makes no network request, and writes nothing. A topic that
//! exists prints its text and the command exits 0; a topic that does not
//! exist is a usage mistake the caller maps to `EXIT_USAGE` (64).

/// The topic index printed by `dbgraph help` with no topic, and by
/// `dbgraph help topics`.
pub const HELP: &str = "\
usage: dbgraph help [topic]
dbgraph built-in manual: the commands, the exit-code contract, the spec
configuration, and the diagnostics catalog.

topics:
  commands       every command with its purpose and flags
  exit-codes     what every exit code means and which command emits it
  configuration  the dbgraph.toml spec file and its sections
  diagnostics    every error class: meaning, cause, and the fix

run 'dbgraph help <topic>' for a topic
";

/// The commands topic: every command with its purpose and flags.
pub const COMMANDS: &str = "\
# dbgraph help commands — the command surface
usage: dbgraph <command> [flags]

commands:
  generate   Introspect the databases in the spec and write or check graph.toml.
               Never mutates the database: every connection opens read-only.
    --spec PATH    Path to the dbgraph spec file. Default: dbgraph.toml.
    --out PATH     Path to the generated graph file. Default: graph.toml.
    --check        Compare the file with a fresh regeneration. Do not write it.
  verify     Compare a live database with the committed graph.toml and report
               drift. Never mutates the database and writes no file.
               Checks: C1 schema drift, C2 fk edge drift, C5 no_cycles under
               --strict ([[allow]] edges honored), C8 graph-file freshness.
    --spec PATH    Path to the dbgraph spec file. Default: dbgraph.toml.
    --db PATH      Path to the SQLite database file. Overrides the url_env URL.
    --strict       Also enforce the spec no_cycles constraint on the live graph.
    --json         Print the findings as a JSON report on stdout.
  help       Print this built-in manual. 'help <topic>' prints one topic.
";

/// The exit-codes topic: the contract every command obeys.
pub const EXIT_CODES: &str = "\
# dbgraph help exit-codes — the exit-code contract
Scripts branch on these numbers.

   0   Success. generate wrote a fresh graph, --check found graph.toml fresh,
       or verify reported no findings.
   1   Stale drift: --check found graph.toml stale, or verify reported drift
       findings (C1, C2, C8), or the committed graph.toml is missing,
       unreadable, or unparseable — an artifact that cannot match a fresh
       render is drift.
   2   A spec constraint failed: generate found a foreign-key cycle, or verify
       --strict reported a C5 fk_cycle finding.
  64   The command line itself is wrong: no command, an unknown flag, a
       missing value, or help named a topic the manual does not have. Kept
       apart from 2 so a usage mistake is never read as a constraint failure.
  70   Operational precondition: the verdict never ran. The spec file is
       missing or unreadable, the spec is invalid (bad TOML, unknown key,
       bad section), the URL environment variable is not set, the dialect is
       not supported, the database could not be read, or --out refused its
       target. Never read 70 as drift or as a constraint failure.

verify precedence: with findings of both classes (drift and constraint), the
exit code is 2; every finding still prints its own red line.
";

/// The configuration topic: the dbgraph.toml spec file.
pub const CONFIGURATION: &str = "\
# dbgraph help configuration — the dbgraph.toml spec file
The spec is TOML. Unknown keys are refused. Sections:

  [project]
    name = \"...\"        Project name. Must not be empty. Recorded in graph.toml.

  [[database]]          Exactly one database section per run.
    id = \"...\"          Database id. Must not be empty. Recorded in graph.toml.
    dialect = \"sqlite\"  Backend. Only \"sqlite\" is supported; others are refused.
    url_env = \"NAME\"    Name of the environment variable that holds the
                        connection URL. The URL never lives in the spec file.
    include = [...]     Optional table-name globs; empty means every table.
    exclude = [...]     Optional table-name globs removed after include.

  [[constraint]]
    kind = \"no_cycles\"  Fail with exit 2 when foreign-key reference edges form a
                        cycle. A self-referencing table is not a cycle. generate
                        enforces it on every run; verify enforces it under
                        --strict.

  [[allow]]
    kind = \"edge\"       Exempt matching edges from no_cycles. generate and
                          verify --strict honor these rules identically.
    from = \"...\"        Match the source table. Empty matches any source.
    to = \"...\"          Match the target table. Empty matches any target.
    edge_kind = \"...\"   Match the edge kind, for example \"reference\".

graph.toml is the only generated view. Every collection in it is sorted, so two
renders of the same schema are byte-identical. generate writes and checks it;
verify compares a live database with it and never writes it.
";

/// The diagnostics topic: every error class, its meaning and its fix.
pub const DIAGNOSTICS: &str = "\
# dbgraph help diagnostics — what each failure means and the fix
  \"...: No such file or directory\" (spec)   (exit 70)
      The spec file is missing or unreadable. Point --spec at an existing,
      readable dbgraph.toml, or create one in the current directory.
  \"unknown key / invalid TOML\" (spec)       (exit 70)
      The spec has a key dbgraph does not know, or is not valid TOML. Correct
      it against 'dbgraph help configuration'.
  \"environment variable NAME is not set\"   (exit 70)
      The database's url_env names a variable that is absent. Export the
      connection URL under that name, or point url_env at a set variable.
  \"dialect not yet supported\"              (exit 70)
      The spec asks for a backend dbgraph does not implement. Use
      dialect = \"sqlite\"; postgres and turso are refused on purpose.
  \"no_cycles: foreign-key cycle through tables: ...\"   (exit 2)
      Reference edges form a cycle the [allow] rules do not exempt. Break the
      cycle in the schema, or add an [[allow]] rule that exempts the edge.
  \"... is stale; regenerate with: ...\"     (exit 1)
      --check compared graph.toml with a fresh regeneration and they differ.
      Run the printed 'dbgraph generate' command to refresh the file.
  \"refusing to overwrite: ... SQLite database\"          (exit 70)
      The --out path is a database, a WAL sidecar of one, or cannot be proven
      not to be either. Point --out at a graph file path, never a database path.

verify findings — each red line starts with the check id and names the object:
  C1 schema drift              (exit 1)
      A table or column was added, removed, or changed between the committed
      graph.toml and the live database. Regenerate: the line's file is stale.
  C2 fk edge drift             (exit 1)
      A foreign key was added or removed. The object names the edge,
      for example payments->orders(order_id -> id). A changed foreign key
      appears as one removed edge plus one added edge.
  C5 no_cycles                 (exit 2, only with --strict)
      The live database has a foreign-key cycle. Without --strict a cycle is
      not reported: it is a constraint question, not drift. [[allow]] edge
      rules are honored — an allowed edge never makes the cycle red.
  C8 freshness                 (exit 1)
      graph.toml differs byte for byte from a fresh render, even where no
      C1/C2 object finding applies. The line names the regenerate command.
";

/// The topics the manual has, in index order. `help topics` and `help` with no
/// argument print the index; every name here prints its own page.
pub const TOPICS: [&str; 4] = ["commands", "exit-codes", "configuration", "diagnostics"];

/// Print one topic. `None` prints the index. An unknown name is a usage
/// mistake the caller maps to `EXIT_USAGE`. This function only writes to stdout.
pub fn run(topic: Option<&str>) -> Result<(), String> {
    let Some(topic) = topic else {
        print!("{HELP}");
        return Ok(());
    };
    match topic {
        "topics" => print!("{HELP}"),
        "commands" => print!("{COMMANDS}"),
        "exit-codes" => print!("{EXIT_CODES}"),
        "configuration" => print!("{CONFIGURATION}"),
        "diagnostics" => print!("{DIAGNOSTICS}"),
        other => {
            return Err(format!(
                "unknown help topic: {other}\nvalid topics: {}\nrun 'dbgraph help'",
                TOPICS.join(", ")
            ));
        }
    }
    Ok(())
}
