//! Butterbot: a run-once ACME certificate maintainer.
//!
//! The crate is organised by capability. Each module corresponds to one spec under
//! `openspec/`, plus the shared [`error`] and [`retry`] building blocks.

pub mod acme;
pub mod cli;
pub mod config;
pub mod dns;
pub mod error;
pub mod events;
pub mod pipeline;
pub mod policy;
pub mod retry;
pub mod store;

pub use error::{Error, Result};
