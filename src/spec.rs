//! `dbgraph.toml` spec parsing. Database URLs come from environment
//! variables only: a URL never lives in the spec file.

use std::path::Path;

use serde::Deserialize;

pub use crate::error::Error;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub project: ProjectSpec,
    #[serde(default)]
    pub database: Vec<DatabaseSpec>,
    #[serde(default)]
    pub constraint: Vec<Constraint>,
    #[serde(default)]
    pub allow: Vec<Allow>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSpec {
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseSpec {
    pub id: String,
    pub dialect: String,
    /// Name of the environment variable that holds the connection URL.
    pub url_env: String,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Constraint {
    /// Constraint kind. Supported: `no_cycles`.
    pub kind: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Allow {
    /// Rule kind. Supported: `edge`.
    pub kind: String,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: String,
    #[serde(default)]
    pub edge_kind: Option<String>,
}

/// Load and validate a spec file. Every failure here is an operational
/// precondition (exit 70): the file is missing or unreadable, or its
/// contents are not a spec this tool can act on.
pub fn load(path: &Path) -> Result<Spec, Error> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::Operational(format!("{}: {e}", path.display())))?;
    let spec: Spec = toml::from_str(&text)
        .map_err(|e| Error::Operational(format!("{}: {e}", path.display())))?;
    if spec.project.name.is_empty() {
        return Err(Error::Operational(
            "project.name must not be empty".into(),
        ));
    }
    for db in &spec.database {
        if db.id.is_empty() {
            return Err(Error::Operational(
                "[[database]] id must not be empty".into(),
            ));
        }
    }
    for c in &spec.constraint {
        if c.kind != "no_cycles" {
            return Err(Error::Operational(format!(
                "unknown constraint kind {:?}: expected \"no_cycles\"",
                c.kind
            )));
        }
    }
    for a in &spec.allow {
        if a.kind != "edge" {
            return Err(Error::Operational(format!(
                "unknown allow kind {:?}: expected \"edge\"",
                a.kind
            )));
        }
    }
    Ok(spec)
}

/// Read the database URL from the environment. Never from the spec file.
/// An unset variable is a bad environment: an operational precondition
/// failure (exit 70), never drift.
pub fn url_from_env(db: &DatabaseSpec) -> Result<String, Error> {
    std::env::var(&db.url_env).map_err(|_| {
        Error::Operational(format!(
            "database {:?}: environment variable {} is not set",
            db.id, db.url_env
        ))
    })
}

fn edge_allowed(edge: &crate::model::Edge, allows: &[Allow]) -> bool {
    allows.iter().any(|a| {
        let kind_ok = match &a.edge_kind {
            Some(k) => k == "reference" && edge.kind() == crate::model::EdgeKind::Reference,
            None => true,
        };
        let from_ok = a.from.is_empty() || a.from == edge.from;
        let to_ok = a.to.is_empty() || a.to == edge.to;
        kind_ok && from_ok && to_ok
    })
}

/// Check `no_cycles` over foreign-key reference edges. A self-referencing
/// table is not a cycle. Returns the sorted table names that sit on, or
/// feed into, a cycle on failure.
pub fn check_no_cycles(graph: &crate::model::SchemaGraph, allows: &[Allow]) -> Result<(), Vec<String>> {
    let mut nodes: Vec<&str> = Vec::new();
    let mut edges: Vec<(&str, &str)> = Vec::new();
    for edge in &graph.edges {
        if edge.kind() != crate::model::EdgeKind::Reference || edge_allowed(edge, allows) {
            continue;
        }
        for n in [&edge.from, &edge.to] {
            if !nodes.contains(&n.as_str()) {
                nodes.push(n);
            }
        }
        if edge.from != edge.to {
            edges.push((&edge.from, &edge.to));
        }
    }
    // Kahn-style fixpoint: drop nodes with no remaining outgoing edge.
    let mut remaining: std::collections::BTreeSet<&str> = nodes.into_iter().collect();
    loop {
        let removable: Vec<&str> = remaining
            .iter()
            .copied()
            .filter(|n| !edges.iter().any(|e| e.0 == *n && remaining.contains(e.1)))
            .collect();
        let before = remaining.len();
        for n in removable {
            remaining.remove(n);
        }
        if remaining.len() == before {
            break;
        }
    }
    if remaining.is_empty() {
        Ok(())
    } else {
        Err(remaining.into_iter().map(str::to_string).collect())
    }
}
