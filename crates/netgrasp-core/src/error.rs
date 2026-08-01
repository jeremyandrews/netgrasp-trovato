//! Error type for the Netgrasp core.
//!
//! The core is host-agnostic, so it never surfaces kernel host error codes
//! (negative `i32`s) directly. The plugin's port implementations translate host
//! failures into [`CoreError`]; the core's own pure logic produces the
//! validation variants.

use std::fmt;

/// A failure inside the Netgrasp core or one of its host ports.
///
/// The transient/permanent split is the same contract Argus established and for
/// the same reason: a queue worker's only way to ask for a retry is a WASM trap
/// (`G-QUEUE-RETRY-SIGNAL`), so exactly one predicate must decide whether to
/// trap. Netgrasp does its work from `tap_cron` rather than a queue worker, and
/// `tap_cron` must never trap (it shares one dispatch budget across every
/// plugin) — but the same split still decides whether a failed row is left
/// `dirty` for the next pass or marked terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreError {
    /// A database (db host) operation failed. Transient: the row stays `dirty`
    /// and the next cron pass re-attempts it.
    Store(String),
    /// An `item-api` host call failed. Transient for the same reason.
    Item(String),
    /// A row or Item was not found where the caller expected one. Permanent —
    /// re-reading gets the same answer; the caller repairs rather than retries.
    NotFound(String),
    /// Input that cannot be interpreted: a malformed Item payload, a device row
    /// with no MAC, a column name outside its writer's set. Permanent.
    Invalid(String),
}

impl CoreError {
    /// Whether the caller should leave the work for a later pass.
    ///
    /// `true` for infrastructure failures (a db hiccup, an `item-api` blip),
    /// `false` for anything a retry would re-derive identically.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(self, CoreError::Store(_) | CoreError::Item(_))
    }
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreError::Store(m) => write!(f, "store error: {m}"),
            CoreError::Item(m) => write!(f, "item host error: {m}"),
            CoreError::NotFound(m) => write!(f, "not found: {m}"),
            CoreError::Invalid(m) => write!(f, "invalid input: {m}"),
        }
    }
}

impl std::error::Error for CoreError {}

/// Convenience alias for fallible core operations.
pub type CoreResult<T> = Result<T, CoreError>;

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn infrastructure_failures_are_transient_and_input_failures_are_not() {
        assert!(CoreError::Store("db down".into()).is_transient());
        assert!(CoreError::Item("host blip".into()).is_transient());
        assert!(!CoreError::NotFound("no such item".into()).is_transient());
        assert!(!CoreError::Invalid("no mac".into()).is_transient());
    }

    #[test]
    fn display_names_the_class_before_the_detail() {
        assert_eq!(
            CoreError::Store("timeout".into()).to_string(),
            "store error: timeout"
        );
        assert_eq!(
            CoreError::Invalid("bad column".into()).to_string(),
            "invalid input: bad column"
        );
    }
}
