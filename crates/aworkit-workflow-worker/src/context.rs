//! Immutable Harness Context revisions and explicit branch reconciliation.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};
use thiserror::Error;

/// A fully immutable context revision with explicit ancestry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextRevision {
    pub id: u64,
    pub parents: Vec<u64>,
    pub value: Value,
}

/// The only supported, visible reconciliation algorithms.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JoinStrategy {
    RequireEqual,
    ObjectUnion,
}

/// Errors from an invalid history or undeclared merge.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum ContextError {
    #[error("unknown context revision {0}")]
    UnknownRevision(u64),
    #[error("join requires at least two distinct branch heads")]
    InvalidJoin,
    #[error("branch values are not identical")]
    UnequalBranches,
    #[error("object-union join requires objects with non-conflicting keys")]
    ConflictingJoin,
}

/// Keeps a revision DAG; previous revisions are never mutated or removed.
#[derive(Clone, Debug)]
pub struct ContextStore {
    revisions: BTreeMap<u64, ContextRevision>,
    next_id: u64,
}

impl ContextStore {
    /// Starts a new revision lineage from one immutable root value.
    #[must_use]
    pub fn new(root: Value) -> Self {
        let revision = ContextRevision {
            id: 1,
            parents: Vec::new(),
            value: root,
        };
        Self {
            revisions: BTreeMap::from([(1, revision)]),
            next_id: 2,
        }
    }
    /// Reads one immutable revision.
    pub fn get(&self, id: u64) -> Result<&ContextRevision, ContextError> {
        self.revisions
            .get(&id)
            .ok_or(ContextError::UnknownRevision(id))
    }
    /// Appends a child revision after checking its parent is known.
    pub fn append(&mut self, parent: u64, value: Value) -> Result<u64, ContextError> {
        self.get(parent)?;
        Ok(self.insert(vec![parent], value))
    }
    /// Forks one parent into independent immutable branch heads.
    pub fn fork(&mut self, parent: u64, copies: usize) -> Result<Vec<u64>, ContextError> {
        let base = self.get(parent)?.value.clone();
        (0..copies)
            .map(|_| self.append(parent, base.clone()))
            .collect()
    }
    /// Projects a JSON field path into a new child revision without touching the parent.
    pub fn project(&mut self, parent: u64, keys: &[&str]) -> Result<u64, ContextError> {
        let mut current = &self.get(parent)?.value;
        for key in keys {
            current = current.get(*key).unwrap_or(&Value::Null);
        }
        self.append(parent, current.clone())
    }
    /// Expands array values lazily into child branch heads in source index order.
    pub fn for_each(&mut self, parent: u64) -> Result<Vec<u64>, ContextError> {
        let items = self
            .get(parent)?
            .value
            .as_array()
            .cloned()
            .unwrap_or_default();
        items
            .into_iter()
            .map(|item| self.append(parent, item))
            .collect()
    }
    /// Reconciles distinct declared branches once using the requested deterministic strategy.
    pub fn join(&mut self, heads: &[u64], strategy: JoinStrategy) -> Result<u64, ContextError> {
        let unique: BTreeSet<_> = heads.iter().copied().collect();
        if unique.len() < 2 {
            return Err(ContextError::InvalidJoin);
        }
        let revisions: Result<Vec<_>, _> = unique.iter().map(|id| self.get(*id).cloned()).collect();
        let revisions = revisions?;
        let value = match strategy {
            JoinStrategy::RequireEqual => {
                if revisions
                    .windows(2)
                    .all(|pair| pair[0].value == pair[1].value)
                {
                    revisions[0].value.clone()
                } else {
                    return Err(ContextError::UnequalBranches);
                }
            }
            JoinStrategy::ObjectUnion => {
                let mut joined = Map::new();
                for revision in &revisions {
                    let object = revision
                        .value
                        .as_object()
                        .ok_or(ContextError::ConflictingJoin)?;
                    for (key, value) in object {
                        if let Some(previous) = joined.insert(key.clone(), value.clone())
                            && previous != *value
                        {
                            return Err(ContextError::ConflictingJoin);
                        }
                    }
                }
                Value::Object(joined)
            }
        };
        Ok(self.insert(unique.into_iter().collect(), value))
    }
    fn insert(&mut self, parents: Vec<u64>, value: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.revisions
            .insert(id, ContextRevision { id, parents, value });
        id
    }
}
