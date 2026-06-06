//! Ember+ protocol implementation: S101 framing, BER, and the Glow schema.
//!
//! This crate is pure (no sockets, no async). It turns bytes into Glow trees and
//! Glow operations back into bytes. The async transport lives in `ember-net`.

pub mod glow;
pub mod s101;
