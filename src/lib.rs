//! dbgraph — database schema graph generator and verifier.
//!
//! The tool reads a database and writes `graph.toml` (`generate`) or
//! compares a live database with the committed `graph.toml` (`verify`). It
//! never mutates the database: adapters open every connection read-only.

pub mod command;
pub mod dialect;
mod dialect_api;
mod error;
pub mod help;
pub mod model;
pub mod spec;
pub mod sqlite;

/// Exit code: success. generate wrote or checked a fresh graph; verify
/// reported no findings.
pub const EXIT_OK: i32 = 0;
/// Exit code: stale drift — `--check` found the generated file stale, or
/// `verify` reported a drift finding (C1, C2, C8), or the committed graph
/// file is missing, unreadable, or unparseable: an artifact that cannot
/// match a fresh render is drift. Input and environment failures are never
/// this code; they are `EXIT_OPERATIONAL`.
pub const EXIT_ERROR: i32 = 1;
/// Exit code: a spec constraint failed (generate found a `no_cycles`
/// violation; `verify --strict` reported a C5 finding).
/// Usage errors never use this code; they use `EXIT_USAGE`.
pub const EXIT_CONSTRAINT: i32 = 2;
/// Exit code: the command line itself is wrong (no command, unknown flag,
/// missing value, `help` topic typo). Kept apart from `EXIT_CONSTRAINT` so
/// a usage mistake is never read as a constraint failure. A `help` typo is
/// a usage mistake like any other: the family contract pins this code for it.
pub const EXIT_USAGE: i32 = 64;
/// Exit code: an operational precondition failed — the verdict never ran.
/// Missing or unreadable spec file, invalid spec, unset `url_env` variable,
/// unsupported dialect, unreadable database, or a refused write target.
/// Never answered as drift (1) and never as a constraint (2): a harness can
/// always tell "the schema drifted" from "the inputs are not there".
pub const EXIT_OPERATIONAL: i32 = 70;
