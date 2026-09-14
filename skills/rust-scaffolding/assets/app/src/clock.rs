//! Time as a dependency. Nothing can intercept `Utc::now()` from the outside,
//! so code that needs the current time takes a `Clock` and tests hand it a
//! `FixedClock`.
use chrono::{DateTime, Utc};

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// Production. Zero-sized, so `Arc<SystemClock>` costs one allocation at startup.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Tests. Hand-written on purpose: two lines beat a mockall mock here.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(pub DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}
