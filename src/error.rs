//! Failure classes for every dbgraph verdict path. The variant decides the
//! exit class: `Constraint` answers 2, every variant answered by
//! [`Error::is_operational`] answers 70, and the rest answer 1 — stale
//! drift of the generated artifact only.

/// Any failure in spec loading, introspection, or rendering.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The committed graph file (the artifact under verdict) cannot be
    /// parsed: an artifact that cannot match a fresh render is drift (1).
    #[error("{0}")]
    Spec(String),
    /// The database could not be read: the verdict cannot run (70).
    #[error("{0}")]
    Sqlite(String),
    /// The generated graph file (the artifact under verdict) is missing,
    /// unreadable, or stale: drift (1). Input and environment IO failures
    /// are `Operational`, never this variant.
    #[error("{0}")]
    Io(String),
    /// An internal render/serialize failure: the verdict cannot run (70).
    #[error("{0}")]
    Render(String),
    /// The spec declares a dialect this build does not implement: the
    /// verdict cannot run (70).
    #[error("{0}")]
    UnsupportedDialect(String),
    #[error("{0}")]
    Constraint(String),
    /// An operational precondition failed: the verdict never ran (70).
    /// Missing or unreadable spec file, invalid spec contents, unset
    /// `url_env` variable, or a refused write target.
    #[error("{0}")]
    Operational(String),
}

impl Error {
    /// Artifact IO failure: reading or writing the generated graph file.
    /// Answers drift (1). Spec/input IO answers `Operational` (70).
    pub fn io(path: &std::path::Path, err: &std::io::Error) -> Self {
        Self::Io(format!("{}: {err}", path.display()))
    }

    /// The exit-class question: does this failure mean "the verdict could
    /// not run because an input or the environment is not there"? True for
    /// every class-70 variant; false for drift (1) and constraint (2).
    pub fn is_operational(&self) -> bool {
        matches!(
            self,
            Self::Operational(_) | Self::Sqlite(_) | Self::UnsupportedDialect(_) | Self::Render(_)
        )
    }
}
