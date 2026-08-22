//! Immutable Harness Context revisions, lineage, projections, and explicit joins.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

const DEFAULT_MAX_REVISIONS: usize = 16_384;
const DEFAULT_MAX_INLINE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionKind {
    Root,
    Replace,
    Patch,
    Fork,
    Projection,
    ForEach,
    Join,
    ChildRoot,
    ChildIntegration,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextProvenance {
    pub kind: RevisionKind,
    pub operation_id: String,
    pub source_pointer: Option<String>,
    pub omitted_pointers: Vec<String>,
    pub child_id: Option<String>,
}

impl ContextProvenance {
    fn simple(kind: RevisionKind, operation_id: impl Into<String>) -> Self {
        Self {
            kind,
            operation_id: operation_id.into(),
            source_pointer: None,
            omitted_pointers: Vec::new(),
            child_id: None,
        }
    }
}

/// A fully immutable context revision with explicit ancestry and content identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextRevision {
    pub id: u64,
    pub parents: Vec<u64>,
    pub value: Value,
    pub content_hash: String,
    pub lineage: Vec<String>,
    pub provenance: ContextProvenance,
}

/// Visible, deterministic reconciliation algorithms.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinStrategy {
    RequireEqual,
    ObjectUnion,
    OrderedArray,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JoinContract {
    pub contract_id: String,
    pub common_ancestor: u64,
    pub ordered_heads: Vec<u64>,
    pub strategy: JoinStrategy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildContextSpec {
    pub child_id: String,
    pub parent_revision: u64,
    pub selected_pointers: Vec<Vec<String>>,
    pub instructions: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChildIntegration {
    ReplaceAtKey { key: String },
    MergeObjectAtKey { key: String },
    AppendSummaryAtKey { key: String },
}

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
    #[error("projection path does not exist: {0}")]
    MissingPath(String),
    #[error("for-each input must be an array")]
    NotArray,
    #[error("revision exceeds the bounded inline context size")]
    TooLarge,
    #[error("revision limit reached before a committed checkpoint allowed pruning")]
    RevisionLimit,
    #[error("join heads do not belong to the declared ancestor and order")]
    UndeclaredJoin,
    #[error("revision ID arithmetic overflow")]
    RevisionExhausted,
    #[error("child result was already integrated or does not match its child lineage")]
    InvalidChildIntegration,
    #[error("patch requires an object base")]
    PatchRequiresObject,
}

#[derive(Clone, Debug)]
struct ChildRecord {
    root_revision: u64,
    integrated: bool,
}

/// Keeps a bounded revision DAG. Previous revisions are immutable; pruning is
/// explicit and may only remove revisions unreachable from committed heads.
#[derive(Clone, Debug)]
pub struct ContextStore {
    revisions: BTreeMap<u64, ContextRevision>,
    heads: BTreeSet<u64>,
    children: BTreeMap<String, ChildRecord>,
    next_id: u64,
    max_revisions: usize,
    max_inline_bytes: usize,
}

impl ContextStore {
    #[must_use]
    pub fn new(root: Value) -> Self {
        Self::with_limits(root, DEFAULT_MAX_REVISIONS, DEFAULT_MAX_INLINE_BYTES)
            .expect("default context limits admit ordinary roots")
    }

    pub fn with_limits(
        root: Value,
        max_revisions: usize,
        max_inline_bytes: usize,
    ) -> Result<Self, ContextError> {
        if max_revisions == 0 || encoded_size(&root)? > max_inline_bytes {
            return Err(ContextError::TooLarge);
        }
        let provenance = ContextProvenance::simple(RevisionKind::Root, "root");
        let revision = ContextRevision {
            id: 1,
            parents: Vec::new(),
            content_hash: hash_revision(&[], &root, &provenance),
            value: root,
            lineage: vec!["root".to_owned()],
            provenance,
        };
        Ok(Self {
            revisions: BTreeMap::from([(1, revision)]),
            heads: BTreeSet::from([1]),
            children: BTreeMap::new(),
            next_id: 2,
            max_revisions,
            max_inline_bytes,
        })
    }

    pub fn get(&self, id: u64) -> Result<&ContextRevision, ContextError> {
        self.revisions
            .get(&id)
            .ok_or(ContextError::UnknownRevision(id))
    }

    /// Compatibility replacement revision. Prefer `append_with_provenance` in
    /// runtime code so every transform remains inspectable.
    pub fn append(&mut self, parent: u64, value: Value) -> Result<u64, ContextError> {
        self.append_with_provenance(
            vec![parent],
            value,
            ContextProvenance::simple(RevisionKind::Replace, format!("replace-{parent}")),
            None,
        )
    }

    pub fn append_with_provenance(
        &mut self,
        parents: Vec<u64>,
        value: Value,
        provenance: ContextProvenance,
        lineage_suffix: Option<String>,
    ) -> Result<u64, ContextError> {
        if parents.is_empty() {
            return Err(ContextError::InvalidJoin);
        }
        if self.revisions.len() >= self.max_revisions {
            return Err(ContextError::RevisionLimit);
        }
        if encoded_size(&value)? > self.max_inline_bytes {
            return Err(ContextError::TooLarge);
        }
        let mut unique = BTreeSet::new();
        for parent in &parents {
            self.get(*parent)?;
            if !unique.insert(*parent) {
                return Err(ContextError::InvalidJoin);
            }
        }
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(ContextError::RevisionExhausted)?;
        let mut lineage = self.get(parents[0])?.lineage.clone();
        if let Some(suffix) = lineage_suffix {
            lineage.push(suffix);
        }
        let content_hash = hash_revision(&parents, &value, &provenance);
        self.revisions.insert(
            id,
            ContextRevision {
                id,
                parents: parents.clone(),
                value,
                content_hash,
                lineage,
                provenance,
            },
        );
        for parent in parents {
            self.heads.remove(&parent);
        }
        self.heads.insert(id);
        Ok(id)
    }

    /// Atomically applies sorted object fields to an explicit base revision.
    pub fn patch_object(
        &mut self,
        parent: u64,
        operation_id: impl Into<String>,
        patch: BTreeMap<String, Value>,
    ) -> Result<u64, ContextError> {
        let mut object = self
            .get(parent)?
            .value
            .as_object()
            .cloned()
            .ok_or(ContextError::PatchRequiresObject)?;
        for (key, value) in patch {
            object.insert(key, value);
        }
        self.append_with_provenance(
            vec![parent],
            Value::Object(object),
            ContextProvenance::simple(RevisionKind::Patch, operation_id),
            None,
        )
    }

    /// Forks one parent into independent immutable branch heads.
    pub fn fork(&mut self, parent: u64, copies: usize) -> Result<Vec<u64>, ContextError> {
        if copies == 0 {
            return Err(ContextError::InvalidJoin);
        }
        let base = self.get(parent)?.value.clone();
        let mut heads = Vec::with_capacity(copies);
        // Keep the parent visible as a branch point while creating every child.
        for index in 0..copies {
            let id = self.append_with_provenance(
                vec![parent],
                base.clone(),
                ContextProvenance::simple(RevisionKind::Fork, format!("fork-{parent}-{index}")),
                Some(format!("branch-{index}")),
            )?;
            heads.push(id);
        }
        Ok(heads)
    }

    /// Projects a required JSON path into a new child revision.
    pub fn project(&mut self, parent: u64, keys: &[&str]) -> Result<u64, ContextError> {
        let mut current = &self.get(parent)?.value;
        for key in keys {
            current = current
                .get(*key)
                .ok_or_else(|| ContextError::MissingPath(format!("/{}", keys.join("/"))))?;
        }
        let mut provenance = ContextProvenance::simple(
            RevisionKind::Projection,
            format!("project-{parent}-{}", keys.join(".")),
        );
        provenance.source_pointer = Some(format!("/{}", keys.join("/")));
        self.append_with_provenance(vec![parent], current.clone(), provenance, None)
    }

    /// Expands array values into child branch heads in source-index order.
    pub fn for_each(&mut self, parent: u64) -> Result<Vec<u64>, ContextError> {
        let length = self
            .get(parent)?
            .value
            .as_array()
            .ok_or(ContextError::NotArray)?
            .len();
        self.for_each_window(parent, 0, length)
    }

    /// Bounded lazy for-each admission. A caller can checkpoint after each
    /// window without materializing an unbounded branch set.
    pub fn for_each_window(
        &mut self,
        parent: u64,
        start: usize,
        limit: usize,
    ) -> Result<Vec<u64>, ContextError> {
        let items = self
            .get(parent)?
            .value
            .as_array()
            .ok_or(ContextError::NotArray)?
            .iter()
            .skip(start)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let mut heads = Vec::with_capacity(items.len());
        for (offset, item) in items.into_iter().enumerate() {
            let index = start + offset;
            heads.push(self.append_with_provenance(
                vec![parent],
                item,
                ContextProvenance::simple(
                    RevisionKind::ForEach,
                    format!("foreach-{parent}-{index}"),
                ),
                Some(format!("item-{index}")),
            )?);
        }
        Ok(heads)
    }

    /// Compatibility join with deterministic ascending-head order.
    pub fn join(&mut self, heads: &[u64], strategy: JoinStrategy) -> Result<u64, ContextError> {
        let unique: BTreeSet<_> = heads.iter().copied().collect();
        if unique.len() < 2 {
            return Err(ContextError::InvalidJoin);
        }
        self.join_values(
            unique.into_iter().collect(),
            strategy,
            ContextProvenance::simple(RevisionKind::Join, "compat-join"),
        )
    }

    /// Reconciles exactly the heads and order frozen in a join contract.
    pub fn join_declared(&mut self, contract: &JoinContract) -> Result<u64, ContextError> {
        if contract.ordered_heads.len() < 2
            || contract
                .ordered_heads
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .len()
                != contract.ordered_heads.len()
            || contract.ordered_heads.iter().any(|head| {
                !self.heads.contains(head)
                    || !self.is_descendant_of(*head, contract.common_ancestor)
            })
        {
            return Err(ContextError::UndeclaredJoin);
        }
        self.join_values(
            contract.ordered_heads.clone(),
            contract.strategy,
            ContextProvenance::simple(RevisionKind::Join, contract.contract_id.clone()),
        )
    }

    fn join_values(
        &mut self,
        ordered_heads: Vec<u64>,
        strategy: JoinStrategy,
        provenance: ContextProvenance,
    ) -> Result<u64, ContextError> {
        let revisions = ordered_heads
            .iter()
            .map(|id| self.get(*id).cloned())
            .collect::<Result<Vec<_>, _>>()?;
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
                        if let Some(previous) = joined.get(key)
                            && previous != value
                        {
                            return Err(ContextError::ConflictingJoin);
                        }
                        joined.insert(key.clone(), value.clone());
                    }
                }
                Value::Object(joined)
            }
            JoinStrategy::OrderedArray => Value::Array(
                revisions
                    .into_iter()
                    .map(|revision| revision.value)
                    .collect(),
            ),
        };
        self.append_with_provenance(ordered_heads, value, provenance, Some("join".to_owned()))
    }

    pub fn spawn_child(&mut self, spec: &ChildContextSpec) -> Result<u64, ContextError> {
        if self.children.contains_key(&spec.child_id) {
            return Err(ContextError::InvalidChildIntegration);
        }
        let parent = self.get(spec.parent_revision)?.value.clone();
        let mut selected = Map::new();
        for pointer in &spec.selected_pointers {
            let mut value = &parent;
            for key in pointer {
                value = value
                    .get(key)
                    .ok_or_else(|| ContextError::MissingPath(format!("/{}", pointer.join("/"))))?;
            }
            selected.insert(pointer.join("."), value.clone());
        }
        selected.insert("instructions".to_owned(), spec.instructions.clone());
        let mut provenance =
            ContextProvenance::simple(RevisionKind::ChildRoot, format!("spawn-{}", spec.child_id));
        provenance.child_id = Some(spec.child_id.clone());
        let root_revision = self.append_with_provenance(
            vec![spec.parent_revision],
            Value::Object(selected),
            provenance,
            Some(format!("child-{}", spec.child_id)),
        )?;
        // A child is a temporary isolated branch. Spawning it must not consume
        // the parent's live head or make child-private revisions the parent's
        // implicit state.
        self.heads.insert(spec.parent_revision);
        self.children.insert(
            spec.child_id.clone(),
            ChildRecord {
                root_revision,
                integrated: false,
            },
        );
        Ok(root_revision)
    }

    pub fn integrate_child(
        &mut self,
        child_id: &str,
        child_head: u64,
        parent_head: u64,
        result: Value,
        integration: ChildIntegration,
    ) -> Result<u64, ContextError> {
        let child = self
            .children
            .get(child_id)
            .ok_or(ContextError::InvalidChildIntegration)?;
        if child.integrated
            || !self.heads.contains(&child_head)
            || !self.heads.contains(&parent_head)
            || !self.is_descendant_of(child_head, child.root_revision)
        {
            return Err(ContextError::InvalidChildIntegration);
        }
        let mut parent = self
            .get(parent_head)?
            .value
            .as_object()
            .cloned()
            .ok_or(ContextError::PatchRequiresObject)?;
        match integration {
            ChildIntegration::ReplaceAtKey { key } => {
                parent.insert(key, result);
            }
            ChildIntegration::MergeObjectAtKey { key } => {
                let result = result.as_object().ok_or(ContextError::ConflictingJoin)?;
                let target = parent
                    .entry(key)
                    .or_insert_with(|| Value::Object(Map::new()))
                    .as_object_mut()
                    .ok_or(ContextError::ConflictingJoin)?;
                for (field, value) in result {
                    if let Some(existing) = target.get(field)
                        && existing != value
                    {
                        return Err(ContextError::ConflictingJoin);
                    }
                    target.insert(field.clone(), value.clone());
                }
            }
            ChildIntegration::AppendSummaryAtKey { key } => {
                let target = parent
                    .entry(key)
                    .or_insert_with(|| Value::Array(Vec::new()))
                    .as_array_mut()
                    .ok_or(ContextError::ConflictingJoin)?;
                target.push(result);
            }
        }
        let mut provenance = ContextProvenance::simple(
            RevisionKind::ChildIntegration,
            format!("integrate-{child_id}"),
        );
        provenance.child_id = Some(child_id.to_owned());
        let id = self.append_with_provenance(
            vec![parent_head, child_head],
            Value::Object(parent),
            provenance,
            Some(format!("integrated-{child_id}")),
        )?;
        self.children
            .get_mut(child_id)
            .expect("child checked above")
            .integrated = true;
        Ok(id)
    }

    #[must_use]
    pub fn heads(&self) -> Vec<u64> {
        self.heads.iter().copied().collect()
    }

    /// Drops only revisions not reachable from the supplied committed heads.
    /// The roots and complete ancestor chain of every retained head remain.
    pub fn prune_unreachable(&mut self, committed_heads: &[u64]) -> Result<usize, ContextError> {
        let mut reachable = BTreeSet::new();
        let mut pending: VecDeque<_> = committed_heads.iter().copied().collect();
        while let Some(id) = pending.pop_front() {
            if !reachable.insert(id) {
                continue;
            }
            let revision = self.get(id)?;
            pending.extend(revision.parents.iter().copied());
        }
        let before = self.revisions.len();
        self.revisions.retain(|id, _| reachable.contains(id));
        self.heads.retain(|id| reachable.contains(id));
        self.children
            .retain(|_, child| reachable.contains(&child.root_revision));
        Ok(before - self.revisions.len())
    }

    fn is_descendant_of(&self, candidate: u64, ancestor: u64) -> bool {
        let mut pending = vec![candidate];
        let mut seen = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if id == ancestor {
                return true;
            }
            if !seen.insert(id) {
                continue;
            }
            if let Some(revision) = self.revisions.get(&id) {
                pending.extend(revision.parents.iter().copied());
            }
        }
        false
    }
}

fn encoded_size(value: &Value) -> Result<usize, ContextError> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|_| ContextError::TooLarge)
}

fn hash_revision(parents: &[u64], value: &Value, provenance: &ContextProvenance) -> String {
    let bytes = serde_json::to_vec(&(parents, value, provenance))
        .expect("Aworkit context values are JSON serializable");
    format!("{:x}", Sha256::digest(bytes))
}
