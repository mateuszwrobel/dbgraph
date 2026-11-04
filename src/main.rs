use clap::Parser;
use dbgraph::{
    EXIT_CONSTRAINT, EXIT_ERROR, EXIT_OPERATIONAL, EXIT_USAGE,
    command::{GenerateArgs, VerifyArgs, generate, verify},
    help,
};

#[derive(Parser)]
#[command(
    name = "dbgraph",
    version,
    about = "Generate and verify a schema graph. Never mutates the database.",
    disable_help_subcommand = true
)]
enum Cli {
    /// Introspect the databases in the spec and write or check graph.toml.
    Generate {
        /// Path to the dbgraph spec file. Default: dbgraph.toml.
        #[arg(long)]
        spec: Option<std::path::PathBuf>,
        /// Path to the generated graph file. Default: graph.toml.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
        /// Compare the file with a fresh regeneration. Do not write it.
        #[arg(long)]
        check: bool,
    },
    /// Compare a live database with the committed graph.toml. Never mutates
    /// the database and writes no file.
    Verify {
        /// Path to the dbgraph spec file. Default: dbgraph.toml.
        #[arg(long)]
        spec: Option<std::path::PathBuf>,
        /// Path to the SQLite database file. Overrides the url_env URL.
        #[arg(long)]
        db: Option<std::path::PathBuf>,
        /// Also enforce the spec no_cycles constraint on the live graph.
        #[arg(long)]
        strict: bool,
        /// Print the findings as a JSON report on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Print the built-in manual. Without a topic, print the topic index.
    Help {
        /// One of: commands, exit-codes, configuration, diagnostics.
        topic: Option<String>,
    },
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    // A wrong command line is a usage error, not a constraint failure.
    // Help and version print their text and exit 0.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            let _ = err.print();
            if err.use_stderr() {
                return std::process::ExitCode::from(EXIT_USAGE as u8);
            }
            return std::process::ExitCode::SUCCESS;
        }
    };
    match cli {
        Cli::Help { topic } => match help::run(topic.as_deref()) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            // A help typo is a usage mistake like any other: the family
            // contract pins 64 for it, never a verdict code.
            Err(msg) => {
                eprintln!("dbgraph: {msg}");
                std::process::ExitCode::from(EXIT_USAGE as u8)
            }
        },
        Cli::Generate { spec, out, check } => {
            let args = GenerateArgs {
                spec_path: spec,
                out_path: out,
                check,
            };
            match generate(args).await {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(dbgraph::spec::Error::Constraint(msg)) => {
                    eprintln!("dbgraph: constraint failed: {msg}");
                    std::process::ExitCode::from(EXIT_CONSTRAINT as u8)
                }
                // An operational failure means the verdict never ran:
                // never answer it as drift (1) or as a constraint (2).
                Err(err) if err.is_operational() => {
                    eprintln!("dbgraph: operational: {err}");
                    std::process::ExitCode::from(EXIT_OPERATIONAL as u8)
                }
                Err(err) => {
                    eprintln!("dbgraph: {err}");
                    std::process::ExitCode::from(EXIT_ERROR as u8)
                }
            }
        }
        Cli::Verify {
            spec,
            db,
            strict,
            json,
        } => {
            let args = VerifyArgs {
                spec_path: spec,
                db_path: db,
                strict,
                json,
            };
            match verify(args).await {
                Ok(code) => std::process::ExitCode::from(code as u8),
                Err(dbgraph::spec::Error::Constraint(msg)) => {
                    eprintln!("dbgraph: constraint failed: {msg}");
                    std::process::ExitCode::from(EXIT_CONSTRAINT as u8)
                }
                Err(err) if err.is_operational() => {
                    eprintln!("dbgraph: operational: {err}");
                    std::process::ExitCode::from(EXIT_OPERATIONAL as u8)
                }
                Err(err) => {
                    eprintln!("dbgraph: {err}");
                    std::process::ExitCode::from(EXIT_ERROR as u8)
                }
            }
        }
    }
}
