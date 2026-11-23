//! DNS validation: publishing challenge records through a DNS provider,
//! checking their propagation and removing them afterwards.

pub mod azure;
pub mod cloudflare;
pub mod propagation;
