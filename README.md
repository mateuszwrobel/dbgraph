# dbgraph

Generate and verify a schema graph from a live database. dbgraph never
mutates the database: adapters open every connection read-only.

```
dbgraph generate [--spec dbgraph.toml] [--out graph.toml] [--check]
dbgraph verify   [--spec dbgraph.toml] [--db PATH] [--strict] [--json]
```

- `graph.toml` is the one generated view. Everything in it is sorted, so
  two runs over the same schema produce byte-identical files.
- `--check` regenerates in memory and compares. A stale file exits 1 and
  is left untouched; the message names the regenerate command.
- Database URLs come from environment variables only (`url_env`).
  `verify --db PATH` names the SQLite file on the command line instead.

## Verify

`verify` compares a live database with the committed `graph.toml`. It
writes no file and mutates nothing. Each finding prints one red line
naming its check id and the object; `--json` prints the named findings
array alone — the exit code carries the verdict.

| Check | What it compares | Exit |
|-------|------------------|------|
| C1 schema drift | added / removed / changed tables and columns | 1 |
| C2 fk edge drift | added / removed foreign keys | 1 |
| C5 no_cycles | foreign-key cycle in the live graph, under `--strict` only; `[[allow]]` edge rules are honored | 2 |
| C8 freshness | `graph.toml` bytes versus a fresh render | 1 |

Without `--strict`, verify answers the drift question only; a cycle is a
constraint question, not drift. With findings of both classes the exit
code is 2 and every finding still prints its line.

## The graph

- Nodes are the tables the scope selects. Edges are foreign keys
  (`reference`), view sources (`derives`) and indexes (`indexed_by`).
- A `derives` edge comes from a relation name written in a `FROM` or `JOIN`
  clause of the stored view SQL. Text inside string literals and comments
  is dropped before that scan, so a name that only appears in a literal or
  a comment cannot create an edge. A name introduced by a `WITH` clause
  (a CTE) names the CTE inside that SQL, never a real relation of the same
  name, so a shadowed relation gains no edge. A name that matches no
  relation in the scope creates no edge either; dbgraph never invents one.
- A `reference` or `derives` edge points at a relation inside the graph. An
  edge whose target a scope rule excludes, or that the schema does not have,
  is dropped rather than left dangling. The target of an `indexed_by` edge
  is an index name, not a relation node. Indexes SQLite generates itself
  (`sqlite_autoindex_*`) are internal bookkeeping and emit no edges.

## Safety

- `--out` refuses every existing target it cannot prove to be a plain
  file before writing: an existing SQLite database, a WAL sidecar name
  (`<db>-wal` / `<db>-shm`) whose sibling database exists, and an existing
  file whose bytes it cannot read. Each refusal exits 70 and writes zero
  bytes. A graph can never overwrite a database or its sidecars.
- The read-only open does not create `-wal` or `-shm` files next to a
  database. A database with a live `-wal` file is read through it, because
  that file can hold committed pages.

Exit codes: 0 ok (`--help` and `--version` print their text and exit 0);
1 stale drift (stale `--check`, a `graph.toml` that is missing,
unparseable, or differs from a fresh render, or a `verify` drift finding);
2 constraint failed (`no_cycles` in generate, or `verify --strict` finding
a cycle); 64 usage error (a wrong command line, or a `help` topic typo);
70 operational precondition (the verdict never ran: missing or unreadable
spec, invalid spec, unset `url_env`, unsupported dialect, unreadable
database, or a refused `--out` target — never read 70 as drift).

Supported dialects: `sqlite`. `postgres` and `turso` are rejected by both
commands with "dialect not yet supported" until their adapters exist;
`verify --db` cannot bypass that refusal.
