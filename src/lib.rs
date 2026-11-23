//! Butterbot: a run-once ACME certificate maintainer.
//!
//! The crate is organised by capability. Each module corresponds to one spec under
//! `openspec/`, plus the shared building blocks: [`error`], [`retry`], [`clock`],
//! [`logging`], [`http`] and [`azure`].

pub mod acme;
pub mod azure;
pub mod cli;
pub mod clock;
pub mod config;
pub mod dns;
pub mod error;
pub mod events;
pub mod http;
pub mod logging;
pub mod pipeline;
pub mod policy;
pub mod retry;
pub mod store;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;

pub use error::{Error, Result};
