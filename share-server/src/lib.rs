//! Native HTTP host for the shared transcript service.
//!
//! This crate does I/O and nothing else. Every decision — who may do what,
//! which key, which precondition — comes from
//! [`txcript_share_core::decide`], the same function the Cloudflare Worker
//! calls through wasm. There is no authorization branch here, and adding one
//! would put the rules back in two places.
//!
//! The shape of a request is therefore always the same: identify the caller,
//! gather facts with at most one HEAD, decide, execute.

#[cfg(feature = "cloudflare_access")]
pub mod access;
pub mod config;
pub mod http;
pub mod store;

pub use config::{Config, ConfigError};
pub use http::{State, router};
pub use store::Store;
