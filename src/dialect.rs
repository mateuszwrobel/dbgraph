//! Dialect selection. The trait and scope types live in the leaf module
//! `crate::dialect_api`, so adapters depend on the contract and never on
//! this selector; this module is the one place that names a concrete
//! backend.

pub use crate::dialect_api::{Dialect, Scope};

use crate::{
    error::Error,
    spec::DatabaseSpec,
};

/// Pick the adapter for the spec entry.
pub fn build(db: &DatabaseSpec, url: &str) -> Result<Box<dyn Dialect>, Error> {
    match db.dialect.as_str() {
        "sqlite" => Ok(Box::new(crate::sqlite::SqliteDialect::new(url))),
        // Implemented later; keep the reason exact for tests and users.
        "postgres" | "turso" => Err(Error::UnsupportedDialect(format!(
            "{}: dialect not yet supported",
            db.dialect
        ))),
        other => Err(Error::UnsupportedDialect(format!(
            "{other}: dialect not yet supported"
        ))),
    }
}
