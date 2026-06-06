//! Async tokio TCP transport for Ember+ (wraps `ember-proto`).

pub mod connection;

pub use connection::{ConnError, Connection, DEFAULT_PORT};
