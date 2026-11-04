# dbgraph capability matrix

Every command, the exit code it produces, and the test that proves it. This
page is a map of the contract, not a restatement of behavior: the binary owns
each claim (`dbgraph help commands`, `dbgraph help exit-codes`), and this table
points at the golden test that pins it.

## Commands

| Command | What it does |
|---------|--------------|
| `generate` | Introspect the databases in the spec and write or check `graph.toml`. Never mutates the database. |
| `help [topic]` | Print the built-in manual (`commands`, `exit-codes`, `configuration`, `diagnostics`). |
| `verify` | Compare a live database with the committed `graph.toml`. Checks C1 schema drift, C2 fk edge drift, C5 `no_cycles` (under `--strict`; C6 = `[[allow]]` rules honored), C8 freshness. Writes no file. |

## Command × exit code × tested-in

Exit classes: 0 green, 1 stale drift, 2 constraint, 64 usage, 70
operational precondition (the verdict never ran).

| Command | Exit | Meaning | Proven by |
|---------|-----:|---------|-----------|
| `generate` (write) | 0 | Wrote a fresh `graph.toml`. | `generate_twice_is_byte_stable`, `captures_fk_edges_regardless_of_pragma`, `captures_view_derives`, `view_over_view_derives_edge`, `duplicate_fk_declaration_order_does_not_change_bytes` |
| `generate --check` (fresh) | 0 | The existing `graph.toml` matches a fresh regeneration. | `check_fresh_and_stale` |
| `generate --check` (stale) | 1 | The generated file differs from a fresh regeneration; it is left untouched. | `check_fresh_and_stale` |
| `generate` (constraint) | 2 | A spec `no_cycles` rule found a foreign-key cycle. | `no_cycles_constraint_exits_two` |
| `verify` (clean) | 0 | No findings; the graph file stays byte-identical and the JSON report is an empty findings array. | `verify_green_path_is_byte_clean`, `verify_db_flag_overrides_url_env` |
| `verify` (drift) | 1 | C1 schema drift, C2 fk edge drift, or C8 byte staleness; a missing or unparseable `graph.toml` is drift too — an artifact that cannot match a fresh render. Each red line names its check and object. | `verify_detects_dropped_column`, `verify_detects_added_fk`, `verify_reports_stale_graph_bytes`, `verify_graph_file_errors_are_named` |
| `verify --strict` (constraint) | 2 | C5 foreign-key cycle in the live graph; an `[[allow]]` edge rule (C6) exempts its edge and keeps the run green. Without `--strict` a cycle is not drift. | `verify_cycle_is_red_only_under_strict`, `verify_honours_allow_declarations` |
| `generate` / `verify` (operational) | 70 | The verdict never ran: missing or unreadable spec file, invalid spec (bad TOML, unknown key, wrong section count), unset `url_env`, unsupported dialect (fail-closed, `--db` cannot bypass), unreadable database, or a refused overwrite target. Never answered as 1 or 2. | `missing_spec_exits_operational_70`, `unknown_spec_key_fails_with_named_key`, `postgres_dialect_is_rejected`, `turso_dialect_is_rejected`, `verify_refuses_unsupported_dialects`, `verify_db_flag_overrides_url_env`, `generate_refuses_to_overwrite_a_database_file`, `generate_refuses_an_existing_file_it_cannot_read`, `generate_refuses_a_wal_sidecar_target` |
| `help <topic>` | 0 | A known topic printed its page; the index prints with no topic. | `help_topics_exit_zero_unknown_exits_64` |
| `help <unknown>` | 64 | A topic typo is a usage mistake, never a verdict code. | `help_topics_exit_zero_unknown_exits_64`, `usage_errors_exit_64` |
| `--help` / `--version` | 0 | Help and version print and exit 0. | `usage_errors_exit_64`, `version_flag_exits_zero`, `help_commands_topic_lists_every_command_flag` |
| (wrong command line) | 64 | No command, an unknown flag, or a missing value. | `usage_errors_exit_64`, `verify_graph_file_errors_are_named` |

## Exit-code contract

Exit codes are defined once in `dbgraph::` (`EXIT_OK`, `EXIT_ERROR`,
`EXIT_CONSTRAINT`, `EXIT_USAGE`, `EXIT_OPERATIONAL`) and printed by `main`.
See `dbgraph help exit-codes` for the same contract from the binary itself;
`exit_codes_topic_enumerates_the_contract` fails if the manual and the
constants ever diverge.

Family note: a `help` topic typo answers the usage code **64** here and in
lanespec. A published release predating the taxonomy flip keeps its
historical code — published surfaces are immutable history, corrected by
deprecation note, never by retro-shim.
