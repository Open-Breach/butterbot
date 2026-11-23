//! Butterbot: a run-once ACME certificate maintainer.
//!
//! The crate is organised by capability. Each module corresponds to one spec under
//! `openspec/`.

pub mod acme;
pub mod cli;
pub mod config;
pub mod dns;
pub mod events;
pub mod pipeline;
pub mod policy;
pub mod store;
