//! The backend contract: the `Dialect` trait and the table-selection
//! `Scope`. This module is a leaf: it names no adapter, so adapters and
//! callers depend on it without depending on each other.

use crate::{
    error::Error,
    model::SchemaGraph,
    spec::DatabaseSpec,
};

/// Table-selection scope from the spec (`include` / `exclude`).
pub struct Scope {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

impl Scope {
    pub fn from_spec(db: &DatabaseSpec) -> Self {
        Self {
            include: db.include.clone(),
            exclude: db.exclude.clone(),
        }
    }

    /// True when the table name belongs in the graph.
    pub fn selects(&self, name: &str) -> bool {
        let included =
            self.include.is_empty() || self.include.iter().any(|p| glob_match(p, name));
        let excluded = self.exclude.iter().any(|p| glob_match(p, name));
        included && !excluded
    }
}

/// Case-sensitive `*`-only glob match. `*` matches any run of characters.
fn glob_match(pattern: &str, name: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == name;
    }
    let mut parts = pattern.split('*');
    let mut rest = name;
    if let Some(first) = parts.next() {
        match rest.strip_prefix(first) {
            Some(r) => rest = r,
            None => return false,
        }
    }
    for part in parts.by_ref() {
        match find_any(part, rest) {
            Some(idx) => rest = &rest[idx + part.len()..],
            None => return false,
        }
    }
    rest.is_empty()
}

fn find_any(needle: &str, haystack: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack.find(needle)
}

/// One database backend. Implementations must never mutate the database.
#[async_trait::async_trait]
pub trait Dialect: Send + Sync {
    /// Read the schema and build the canonical graph.
    async fn introspect(&self, scope: &Scope) -> Result<SchemaGraph, Error>;
}
