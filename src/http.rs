//! Shared HTTP transport. `request` holds the resolved request type (S4), `client`
//! sends it (S6).

pub mod client;
pub mod request;

pub use client::{Transport, TransportError, TransportResponse, send};
