//! Nested bounded usage reservation and charging; a worker can only consume limits.

use thiserror::Error;

/// A frozen resource envelope for a run or nested scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Budget {
    pub turns: u32,
    pub attempts: u32,
    pub deadline_tick: u64,
}
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
        if self.tick >= self.remaining.deadline_tick {
            return Err(LimitError::DeadlineExceeded);
        }
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
        self.tick += 1;
        if self.tick > self.remaining.deadline_tick {
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
