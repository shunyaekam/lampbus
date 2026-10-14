//! Pure rendering for lamps. No I/O, no clocks, no threads: every function maps inputs to outputs, so everything
//! here is deterministic and cheap to test.
//!
//! - [`color`]: colour parsing and conversion.
//! - [`anim`]: the declarative animation format (`spec/animation-format.md`), validated and sampled by time.
//! - [`pipeline`]: calibration, smoothing and temporal dithering, which turn an ideal colour into the integers a
//!   lamp can actually show without banding.

pub mod anim;
pub mod color;
pub mod pipeline;

use std::fmt;

/// A validation or parsing error with a human-readable message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub(crate) fn err<T>(msg: impl Into<String>) -> Result<T, Error> {
    Err(Error(msg.into()))
}
