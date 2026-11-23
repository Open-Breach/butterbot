//! The shared error type.
//!
//! Every failure of a provider or of the ACME server is mapped to one of five
//! classes. The class, not the origin, decides what happens next: only
//! [`Error::Transient`] is retried (see [`crate::retry`]), and
//! [`Error::RateLimited`] may be waited out once by the caller. The mapping from
//! a service's answers to a class lives next to each provider.

use std::time::Duration;

/// Result type used throughout the crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A classified failure. The payload is the human-readable reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// A failure that may go away by itself: a network error, a server error,
    /// a temporary failure of a DNS or store service. Retried.
    #[error("{0}")]
    Transient(String),

    /// A failure that repeating the request cannot fix: a permission error,
    /// a rejected name, invalid input. Never retried.
    #[error("{0}")]
    Permanent(String),

    /// The service refused the request because of a rate limit.
    /// `retry_after` is the wait the service stated, if it stated one.
    #[error("{message}")]
    RateLimited {
        message: String,
        retry_after: Option<Duration>,
    },

    /// The ACME server rejected a challenge. Never retried within a run.
    #[error("{0}")]
    ValidationFailed(String),

    /// A step's time limit was reached.
    #[error("{0}")]
    Timeout(String),
}

impl Error {
    pub fn transient(message: impl Into<String>) -> Self {
        Self::Transient(message.into())
    }

    pub fn permanent(message: impl Into<String>) -> Self {
        Self::Permanent(message.into())
    }

    pub fn rate_limited(message: impl Into<String>, retry_after: Option<Duration>) -> Self {
        Self::RateLimited {
            message: message.into(),
            retry_after,
        }
    }

    pub fn validation_failed(message: impl Into<String>) -> Self {
        Self::ValidationFailed(message.into())
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::Timeout(message.into())
    }

    /// Whether the failed operation may be attempted again.
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_))
    }

    /// The name of the error's class, for log records.
    pub fn class(&self) -> &'static str {
        match self {
            Self::Transient(_) => "transient",
            Self::Permanent(_) => "permanent",
            Self::RateLimited { .. } => "rate_limited",
            Self::ValidationFailed(_) => "validation_failed",
            Self::Timeout(_) => "timeout",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_of_each() -> [Error; 5] {
        [
            Error::transient("connection reset"),
            Error::permanent("forbidden"),
            Error::rate_limited("too many requests", Some(Duration::from_secs(30))),
            Error::validation_failed("incorrect TXT record"),
            Error::timeout("no answer after 2m"),
        ]
    }

    #[test]
    fn only_the_transient_class_is_transient() {
        let transient: Vec<_> = one_of_each().iter().map(Error::is_transient).collect();
        assert_eq!(transient, [true, false, false, false, false]);
    }

    #[test]
    fn class_names_are_distinct_and_stable() {
        let classes: Vec<_> = one_of_each().iter().map(Error::class).collect();
        assert_eq!(
            classes,
            [
                "transient",
                "permanent",
                "rate_limited",
                "validation_failed",
                "timeout"
            ]
        );
    }

    #[test]
    fn display_is_the_reason() {
        let shown: Vec<_> = one_of_each().iter().map(ToString::to_string).collect();
        assert_eq!(
            shown,
            [
                "connection reset",
                "forbidden",
                "too many requests",
                "incorrect TXT record",
                "no answer after 2m"
            ]
        );
    }

    #[test]
    fn rate_limit_keeps_the_stated_wait() {
        let Error::RateLimited { retry_after, .. } =
            Error::rate_limited("slow down", Some(Duration::from_secs(30)))
        else {
            panic!("expected a rate-limit error");
        };
        assert_eq!(retry_after, Some(Duration::from_secs(30)));
    }
}
