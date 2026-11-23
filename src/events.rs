//! Run events and their delivery to the log and to notifiers.

use std::fmt;
use std::str::FromStr;

/// The kinds of event a run emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EventKind {
    RunCompleted,
    Failure,
    Renewed,
    Expiring,
}

impl EventKind {
    pub const ALL: [Self; 4] = [
        Self::RunCompleted,
        Self::Failure,
        Self::Renewed,
        Self::Expiring,
    ];

    /// The name of the kind as it appears in the `event` field and in configuration.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RunCompleted => "run_completed",
            Self::Failure => "failure",
            Self::Renewed => "renewed",
            Self::Expiring => "expiring",
        }
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for EventKind {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == text).ok_or_else(|| {
            format!("unknown event {text:?} (known events: run_completed, failure, renewed, expiring)")
        })
    }
}
