//! The error reported for an unusable configuration.

use std::fmt;

/// Everything that is wrong with the configuration, collected in one report.
///
/// A configuration error ends the program with the configuration-error exit
/// code before any certificate is processed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    /// One entry per problem. Never empty.
    pub problems: Vec<String>,
}

impl ConfigError {
    pub fn new(problems: Vec<String>) -> Self {
        debug_assert!(
            !problems.is_empty(),
            "a configuration error has at least one problem"
        );
        Self { problems }
    }

    pub fn single(problem: impl Into<String>) -> Self {
        Self::new(vec![problem.into()])
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.problems.as_slice() {
            [only] => write!(f, "invalid configuration: {only}"),
            problems => {
                write!(f, "invalid configuration ({} problems):", problems.len())?;
                for problem in problems {
                    write!(f, "\n  - {problem}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ConfigError {}
