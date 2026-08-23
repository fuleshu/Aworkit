//! Monotonic bounded deadline helpers.

use std::time::{Duration, Instant};

/// A deadline derived only from a monotonic clock.
#[derive(Clone, Copy, Debug)]
pub struct MonotonicDeadline {
    started: Instant,
    duration: Duration,
}

impl MonotonicDeadline {
    pub fn after(duration: Duration) -> Result<Self, DeadlineError> {
        if duration.is_zero() {
            return Err(DeadlineError::Zero);
        }
        Ok(Self {
            started: Instant::now(),
            duration,
        })
    }

    #[must_use]
    pub fn expired(self) -> bool {
        self.started.elapsed() >= self.duration
    }

    #[must_use]
    pub fn remaining(self) -> Duration {
        self.duration.saturating_sub(self.started.elapsed())
    }

    #[must_use]
    pub fn elapsed(self) -> Duration {
        self.started.elapsed()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeadlineError {
    Zero,
}

impl std::fmt::Display for DeadlineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("deadline duration must be greater than zero")
    }
}

impl std::error::Error for DeadlineError {}
