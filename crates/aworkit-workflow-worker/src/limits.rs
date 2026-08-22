//! Hierarchical, deterministic execution-limit accounting.
//!
//! The worker can consume or narrow limits frozen by the core.  It cannot mint
//! budget, extend a deadline, or charge one committed outcome more than once.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The compact compatibility budget used by the original worker API.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Budget {
    pub turns: u32,
    pub attempts: u32,
    pub deadline_tick: u64,
}

/// A compatibility reservation.  Calling `LimitController::reserve` consumes
/// it immediately; the richer [`LimitLedger`] supports prepare/charge/release.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Reservation {
    pub turns: u32,
    pub attempts: u32,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum LimitError {
    #[error("budget exhausted")]
    Exhausted,
    #[error("deadline exceeded")]
    DeadlineExceeded,
    #[error("invalid reservation")]
    InvalidReservation,
    #[error("unknown budget scope {0}")]
    UnknownScope(String),
    #[error("duplicate budget scope, reservation, or charge id {0}")]
    DuplicateId(String),
    #[error("reservation {0} is unknown or already settled")]
    UnknownReservation(String),
    #[error("usage exceeds the amount reserved")]
    ReservationExceeded,
    #[error("nested depth, fan-out, or parallelism limit exceeded")]
    StructuralLimit,
    #[error("budget arithmetic overflow")]
    Overflow,
    #[error("checkpoint would extend a frozen deadline")]
    DeadlineExtension,
}

/// Holds remaining, non-negative budget values and deterministic logical time.
#[derive(Clone, Debug)]
pub struct LimitController {
    remaining: Budget,
    tick: u64,
}

impl LimitController {
    #[must_use]
    pub fn new(budget: Budget) -> Self {
        Self {
            remaining: budget,
            tick: 0,
        }
    }

    pub fn reserve(&mut self, reservation: Reservation) -> Result<(), LimitError> {
        if reservation.turns == 0 && reservation.attempts == 0 {
            return Err(LimitError::InvalidReservation);
        }
        self.ensure_before_deadline()?;
        if reservation.turns > self.remaining.turns
            || reservation.attempts > self.remaining.attempts
        {
            return Err(LimitError::Exhausted);
        }
        self.remaining.turns -= reservation.turns;
        self.remaining.attempts -= reservation.attempts;
        Ok(())
    }

    pub fn advance(&mut self) -> Result<(), LimitError> {
        self.tick = self.tick.checked_add(1).ok_or(LimitError::Overflow)?;
        self.ensure_before_deadline()
    }

    fn ensure_before_deadline(&self) -> Result<(), LimitError> {
        if self.tick >= self.remaining.deadline_tick {
            Err(LimitError::DeadlineExceeded)
        } else {
            Ok(())
        }
    }

    #[must_use]
    pub fn remaining(&self) -> Budget {
        self.remaining
    }
}

/// Every resource that can be frozen for a Run or inherited child scope.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BudgetEnvelope {
    pub turns: u64,
    pub attempts: u64,
    pub tool_calls: u64,
    pub tokens: u64,
    pub cost_micros: u64,
    pub actions: u64,
    pub max_depth: u32,
    pub max_fan_out: u32,
    pub max_parallel: u32,
    /// Absolute deterministic tick in the current worker generation.
    pub deadline_tick: u64,
}

impl BudgetEnvelope {
    fn component_min(self, other: Self) -> Self {
        Self {
            turns: self.turns.min(other.turns),
            attempts: self.attempts.min(other.attempts),
            tool_calls: self.tool_calls.min(other.tool_calls),
            tokens: self.tokens.min(other.tokens),
            cost_micros: self.cost_micros.min(other.cost_micros),
            actions: self.actions.min(other.actions),
            max_depth: self.max_depth.min(other.max_depth),
            max_fan_out: self.max_fan_out.min(other.max_fan_out),
            max_parallel: self.max_parallel.min(other.max_parallel),
            deadline_tick: self.deadline_tick.min(other.deadline_tick),
        }
    }

    fn checked_sub(self, usage: Usage) -> Result<Self, LimitError> {
        Ok(Self {
            turns: self
                .turns
                .checked_sub(usage.turns)
                .ok_or(LimitError::Exhausted)?,
            attempts: self
                .attempts
                .checked_sub(usage.attempts)
                .ok_or(LimitError::Exhausted)?,
            tool_calls: self
                .tool_calls
                .checked_sub(usage.tool_calls)
                .ok_or(LimitError::Exhausted)?,
            tokens: self
                .tokens
                .checked_sub(usage.tokens)
                .ok_or(LimitError::Exhausted)?,
            cost_micros: self
                .cost_micros
                .checked_sub(usage.cost_micros)
                .ok_or(LimitError::Exhausted)?,
            actions: self
                .actions
                .checked_sub(usage.actions)
                .ok_or(LimitError::Exhausted)?,
            ..self
        })
    }

    fn checked_add_usage(self, usage: Usage) -> Result<Self, LimitError> {
        Ok(Self {
            turns: self
                .turns
                .checked_add(usage.turns)
                .ok_or(LimitError::Overflow)?,
            attempts: self
                .attempts
                .checked_add(usage.attempts)
                .ok_or(LimitError::Overflow)?,
            tool_calls: self
                .tool_calls
                .checked_add(usage.tool_calls)
                .ok_or(LimitError::Overflow)?,
            tokens: self
                .tokens
                .checked_add(usage.tokens)
                .ok_or(LimitError::Overflow)?,
            cost_micros: self
                .cost_micros
                .checked_add(usage.cost_micros)
                .ok_or(LimitError::Overflow)?,
            actions: self
                .actions
                .checked_add(usage.actions)
                .ok_or(LimitError::Overflow)?,
            ..self
        })
    }
}

/// Reservation and committed-usage dimensions. Structural limits are admitted
/// separately because they describe concurrent state rather than consumption.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Usage {
    pub turns: u64,
    pub attempts: u64,
    pub tool_calls: u64,
    pub tokens: u64,
    pub cost_micros: u64,
    pub actions: u64,
}

impl Usage {
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.turns == 0
            && self.attempts == 0
            && self.tool_calls == 0
            && self.tokens == 0
            && self.cost_micros == 0
            && self.actions == 0
    }

    fn contains(self, actual: Self) -> bool {
        actual.turns <= self.turns
            && actual.attempts <= self.attempts
            && actual.tool_calls <= self.tool_calls
            && actual.tokens <= self.tokens
            && actual.cost_micros <= self.cost_micros
            && actual.actions <= self.actions
    }

    fn checked_sub(self, actual: Self) -> Result<Self, LimitError> {
        Ok(Self {
            turns: self
                .turns
                .checked_sub(actual.turns)
                .ok_or(LimitError::Overflow)?,
            attempts: self
                .attempts
                .checked_sub(actual.attempts)
                .ok_or(LimitError::Overflow)?,
            tool_calls: self
                .tool_calls
                .checked_sub(actual.tool_calls)
                .ok_or(LimitError::Overflow)?,
            tokens: self
                .tokens
                .checked_sub(actual.tokens)
                .ok_or(LimitError::Overflow)?,
            cost_micros: self
                .cost_micros
                .checked_sub(actual.cost_micros)
                .ok_or(LimitError::Overflow)?,
            actions: self
                .actions
                .checked_sub(actual.actions)
                .ok_or(LimitError::Overflow)?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReservationRecord {
    pub reservation_id: String,
    pub scope_id: String,
    pub reserved: Usage,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BudgetScopeCheckpoint {
    pub scope_id: String,
    pub parent_scope_id: Option<String>,
    pub depth: u32,
    pub remaining: BudgetEnvelope,
    pub active_children: u32,
    pub active_parallel: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LimitCheckpoint {
    pub current_tick: u64,
    pub scopes: Vec<BudgetScopeCheckpoint>,
    pub reservations: Vec<ReservationRecord>,
    pub charge_ids: Vec<String>,
    pub loop_iterations: Vec<(String, u32)>,
}

#[derive(Clone, Debug)]
struct BudgetScope {
    parent: Option<String>,
    depth: u32,
    remaining: BudgetEnvelope,
    active_children: u32,
    active_parallel: u32,
}

/// Hierarchical reservation ledger used by loops, attempts, invocations,
/// parallel regions, and temporary subagents.
#[derive(Clone, Debug)]
pub struct LimitLedger {
    tick: u64,
    scopes: BTreeMap<String, BudgetScope>,
    reservations: BTreeMap<String, ReservationRecord>,
    charge_ids: BTreeSet<String>,
    loop_iterations: BTreeMap<String, u32>,
}

impl LimitLedger {
    pub fn new(
        root_scope_id: impl Into<String>,
        budget: BudgetEnvelope,
    ) -> Result<Self, LimitError> {
        let root_scope_id = root_scope_id.into();
        validate_logical_id(&root_scope_id)?;
        Ok(Self {
            tick: 0,
            scopes: BTreeMap::from([(
                root_scope_id,
                BudgetScope {
                    parent: None,
                    depth: 0,
                    remaining: budget,
                    active_children: 0,
                    active_parallel: 0,
                },
            )]),
            reservations: BTreeMap::new(),
            charge_ids: BTreeSet::new(),
            loop_iterations: BTreeMap::new(),
        })
    }

    /// Creates a child whose effective envelope is the component-wise minimum
    /// of its configuration and the parent's currently remaining allowance.
    pub fn create_child(
        &mut self,
        scope_id: impl Into<String>,
        parent_scope_id: &str,
        configured: BudgetEnvelope,
    ) -> Result<(), LimitError> {
        let scope_id = scope_id.into();
        validate_logical_id(&scope_id)?;
        if self.scopes.contains_key(&scope_id) {
            return Err(LimitError::DuplicateId(scope_id));
        }
        let parent = self
            .scopes
            .get_mut(parent_scope_id)
            .ok_or_else(|| LimitError::UnknownScope(parent_scope_id.to_owned()))?;
        let depth = parent.depth.checked_add(1).ok_or(LimitError::Overflow)?;
        if depth > parent.remaining.max_depth
            || parent.active_children >= parent.remaining.max_fan_out
        {
            return Err(LimitError::StructuralLimit);
        }
        parent.active_children += 1;
        let remaining = configured.component_min(parent.remaining);
        self.scopes.insert(
            scope_id,
            BudgetScope {
                parent: Some(parent_scope_id.to_owned()),
                depth,
                remaining,
                active_children: 0,
                active_parallel: 0,
            },
        );
        Ok(())
    }

    pub fn close_child(&mut self, scope_id: &str) -> Result<(), LimitError> {
        self.can_close_child(scope_id)?;
        let scope = self
            .scopes
            .get(scope_id)
            .expect("child closure validated above");
        let parent = scope.parent.clone();
        self.scopes.remove(scope_id);
        if let Some(parent) = parent {
            let parent = self
                .scopes
                .get_mut(&parent)
                .ok_or_else(|| LimitError::UnknownScope(parent.clone()))?;
            parent.active_children = parent.active_children.saturating_sub(1);
        }
        Ok(())
    }

    pub fn can_close_child(&self, scope_id: &str) -> Result<(), LimitError> {
        let scope = self
            .scopes
            .get(scope_id)
            .ok_or_else(|| LimitError::UnknownScope(scope_id.to_owned()))?;
        if scope.parent.is_none() {
            return Err(LimitError::InvalidReservation);
        }
        if self
            .reservations
            .values()
            .any(|record| record.scope_id == scope_id)
            || self
                .scopes
                .values()
                .any(|candidate| candidate.parent.as_deref() == Some(scope_id))
        {
            return Err(LimitError::InvalidReservation);
        }
        Ok(())
    }

    pub fn reserve(
        &mut self,
        scope_id: &str,
        reservation_id: impl Into<String>,
        usage: Usage,
    ) -> Result<(), LimitError> {
        let reservation_id = reservation_id.into();
        validate_logical_id(&reservation_id)?;
        if usage.is_zero() {
            return Err(LimitError::InvalidReservation);
        }
        if self.reservations.contains_key(&reservation_id)
            || self.charge_ids.contains(&reservation_id)
        {
            return Err(LimitError::DuplicateId(reservation_id));
        }
        let lineage = self.scope_lineage(scope_id)?;
        // Prove the complete hierarchical reservation before mutating any
        // scope so a failed ancestor admission is atomic.
        for ancestor in &lineage {
            let scope = self
                .scopes
                .get(ancestor)
                .ok_or_else(|| LimitError::UnknownScope(ancestor.clone()))?;
            if self.tick >= scope.remaining.deadline_tick {
                return Err(LimitError::DeadlineExceeded);
            }
            scope.remaining.checked_sub(usage)?;
        }
        for ancestor in &lineage {
            let scope = self
                .scopes
                .get_mut(ancestor)
                .expect("lineage validated above");
            scope.remaining = scope.remaining.checked_sub(usage)?;
        }
        self.reservations.insert(
            reservation_id.clone(),
            ReservationRecord {
                reservation_id,
                scope_id: scope_id.to_owned(),
                reserved: usage,
            },
        );
        Ok(())
    }

    /// Settles a reservation exactly once. Unused capacity is returned to the
    /// same scope, while actual committed usage remains consumed.
    pub fn charge(
        &mut self,
        reservation_id: &str,
        charge_id: impl Into<String>,
        actual: Usage,
    ) -> Result<(), LimitError> {
        let charge_id = charge_id.into();
        validate_logical_id(&charge_id)?;
        if self.charge_ids.contains(&charge_id) {
            return Ok(());
        }
        let reservation = self
            .reservations
            .remove(reservation_id)
            .ok_or_else(|| LimitError::UnknownReservation(reservation_id.to_owned()))?;
        if !reservation.reserved.contains(actual) {
            self.reservations
                .insert(reservation.reservation_id.clone(), reservation);
            return Err(LimitError::ReservationExceeded);
        }
        let unused = reservation.reserved.checked_sub(actual)?;
        for ancestor in self.scope_lineage(&reservation.scope_id)? {
            let scope = self
                .scopes
                .get_mut(&ancestor)
                .ok_or_else(|| LimitError::UnknownScope(ancestor.clone()))?;
            scope.remaining = scope.remaining.checked_add_usage(unused)?;
        }
        self.charge_ids.insert(charge_id);
        Ok(())
    }

    pub fn release(&mut self, reservation_id: &str) -> Result<(), LimitError> {
        let reservation = self
            .reservations
            .remove(reservation_id)
            .ok_or_else(|| LimitError::UnknownReservation(reservation_id.to_owned()))?;
        for ancestor in self.scope_lineage(&reservation.scope_id)? {
            let scope = self
                .scopes
                .get_mut(&ancestor)
                .ok_or_else(|| LimitError::UnknownScope(ancestor.clone()))?;
            scope.remaining = scope.remaining.checked_add_usage(reservation.reserved)?;
        }
        Ok(())
    }

    pub fn enter_parallel(&mut self, scope_id: &str) -> Result<(), LimitError> {
        let scope = self.scope_before_deadline_mut(scope_id)?;
        if scope.active_parallel >= scope.remaining.max_parallel {
            return Err(LimitError::StructuralLimit);
        }
        scope.active_parallel += 1;
        Ok(())
    }

    pub fn leave_parallel(&mut self, scope_id: &str) -> Result<(), LimitError> {
        let scope = self
            .scopes
            .get_mut(scope_id)
            .ok_or_else(|| LimitError::UnknownScope(scope_id.to_owned()))?;
        scope.active_parallel = scope.active_parallel.saturating_sub(1);
        Ok(())
    }

    pub fn next_loop_iteration(
        &mut self,
        scope_id: &str,
        loop_id: &str,
        maximum: u32,
    ) -> Result<u32, LimitError> {
        self.scope_before_deadline_mut(scope_id)?;
        if maximum == 0 {
            return Err(LimitError::StructuralLimit);
        }
        let key = format!("{scope_id}:{loop_id}");
        let current = self.loop_iterations.get(&key).copied().unwrap_or(0);
        if current >= maximum {
            return Err(LimitError::Exhausted);
        }
        let next = current.checked_add(1).ok_or(LimitError::Overflow)?;
        self.loop_iterations.insert(key, next);
        Ok(next)
    }

    pub fn advance_to(&mut self, tick: u64) -> Result<(), LimitError> {
        if tick < self.tick {
            return Err(LimitError::DeadlineExtension);
        }
        self.tick = tick;
        if self
            .scopes
            .values()
            .any(|scope| self.tick >= scope.remaining.deadline_tick)
        {
            return Err(LimitError::DeadlineExceeded);
        }
        Ok(())
    }

    #[must_use]
    pub fn remaining(&self, scope_id: &str) -> Option<BudgetEnvelope> {
        self.scopes.get(scope_id).map(|scope| scope.remaining)
    }

    #[must_use]
    pub fn checkpoint(&self) -> LimitCheckpoint {
        LimitCheckpoint {
            current_tick: self.tick,
            scopes: self
                .scopes
                .iter()
                .map(|(scope_id, scope)| BudgetScopeCheckpoint {
                    scope_id: scope_id.clone(),
                    parent_scope_id: scope.parent.clone(),
                    depth: scope.depth,
                    remaining: BudgetEnvelope {
                        deadline_tick: scope.remaining.deadline_tick.saturating_sub(self.tick),
                        ..scope.remaining
                    },
                    active_children: scope.active_children,
                    active_parallel: scope.active_parallel,
                })
                .collect(),
            reservations: self.reservations.values().cloned().collect(),
            charge_ids: self.charge_ids.iter().cloned().collect(),
            loop_iterations: self
                .loop_iterations
                .iter()
                .map(|(id, count)| (id.clone(), *count))
                .collect(),
        }
    }

    /// Restores remaining-duration deadlines relative to a new monotonic epoch.
    /// The stored durations may only shrink before this call; they can never be
    /// expanded by passing an earlier tick.
    pub fn restore(checkpoint: LimitCheckpoint, new_tick: u64) -> Result<Self, LimitError> {
        let mut scopes = BTreeMap::new();
        for saved in checkpoint.scopes {
            validate_logical_id(&saved.scope_id)?;
            let deadline_tick = new_tick
                .checked_add(saved.remaining.deadline_tick)
                .ok_or(LimitError::Overflow)?;
            scopes.insert(
                saved.scope_id,
                BudgetScope {
                    parent: saved.parent_scope_id,
                    depth: saved.depth,
                    remaining: BudgetEnvelope {
                        deadline_tick,
                        ..saved.remaining
                    },
                    active_children: saved.active_children,
                    active_parallel: saved.active_parallel,
                },
            );
        }
        for scope in scopes.values() {
            if let Some(parent) = &scope.parent
                && !scopes.contains_key(parent)
            {
                return Err(LimitError::UnknownScope(parent.clone()));
            }
            if scope.active_parallel > scope.remaining.max_parallel {
                return Err(LimitError::StructuralLimit);
            }
            if scope.remaining.deadline_tick <= new_tick {
                return Err(LimitError::DeadlineExceeded);
            }
        }
        if scopes
            .values()
            .filter(|scope| scope.parent.is_none())
            .count()
            != 1
        {
            return Err(LimitError::StructuralLimit);
        }
        for (scope_id, scope) in &scopes {
            let actual_children = scopes
                .values()
                .filter(|candidate| candidate.parent.as_deref() == Some(scope_id.as_str()))
                .count();
            if actual_children != scope.active_children as usize {
                return Err(LimitError::StructuralLimit);
            }
            if scope.active_children > scope.remaining.max_fan_out {
                return Err(LimitError::StructuralLimit);
            }
            if let Some(parent_id) = &scope.parent {
                let parent = scopes
                    .get(parent_id)
                    .ok_or_else(|| LimitError::UnknownScope(parent_id.clone()))?;
                if scope.depth != parent.depth.saturating_add(1)
                    || scope.depth > parent.remaining.max_depth
                {
                    return Err(LimitError::StructuralLimit);
                }
            } else if scope.depth != 0 {
                return Err(LimitError::StructuralLimit);
            }
        }
        let mut reservations = BTreeMap::new();
        for record in checkpoint.reservations {
            validate_logical_id(&record.reservation_id)?;
            if !scopes.contains_key(&record.scope_id)
                || record.reserved.is_zero()
                || reservations
                    .insert(record.reservation_id.clone(), record)
                    .is_some()
            {
                return Err(LimitError::InvalidReservation);
            }
        }
        let charge_count = checkpoint.charge_ids.len();
        let charge_ids: BTreeSet<_> = checkpoint.charge_ids.into_iter().collect();
        if charge_ids.len() != charge_count
            || charge_ids.iter().any(|id| {
                validate_logical_id(id).is_err() || reservations.contains_key(id.as_str())
            })
        {
            return Err(LimitError::InvalidReservation);
        }
        let loop_count = checkpoint.loop_iterations.len();
        let loop_iterations: BTreeMap<_, _> = checkpoint.loop_iterations.into_iter().collect();
        if loop_iterations.len() != loop_count
            || loop_iterations.iter().any(|(key, count)| {
                *count == 0
                    || key.split_once(':').is_none_or(|(scope_id, loop_id)| {
                        !scopes.contains_key(scope_id) || validate_logical_id(loop_id).is_err()
                    })
            })
        {
            return Err(LimitError::InvalidReservation);
        }
        Ok(Self {
            tick: new_tick,
            scopes,
            reservations,
            charge_ids,
            loop_iterations,
        })
    }

    fn scope_before_deadline_mut(
        &mut self,
        scope_id: &str,
    ) -> Result<&mut BudgetScope, LimitError> {
        let tick = self.tick;
        let scope = self
            .scopes
            .get_mut(scope_id)
            .ok_or_else(|| LimitError::UnknownScope(scope_id.to_owned()))?;
        if tick >= scope.remaining.deadline_tick {
            return Err(LimitError::DeadlineExceeded);
        }
        Ok(scope)
    }

    fn scope_lineage(&self, scope_id: &str) -> Result<Vec<String>, LimitError> {
        let mut lineage = Vec::new();
        let mut current = Some(scope_id.to_owned());
        let mut seen = BTreeSet::new();
        while let Some(id) = current {
            if !seen.insert(id.clone()) {
                return Err(LimitError::InvalidReservation);
            }
            let scope = self
                .scopes
                .get(&id)
                .ok_or_else(|| LimitError::UnknownScope(id.clone()))?;
            current = scope.parent.clone();
            lineage.push(id);
        }
        Ok(lineage)
    }
}

fn validate_logical_id(value: &str) -> Result<(), LimitError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        Err(LimitError::InvalidReservation)
    } else {
        Ok(())
    }
}
