//! # nexus-cloud-protocol (edge side)
//!
//! Typed Rust view of `proto/v1.json` — the WebSocket envelope and the
//! eight message kinds Phase 1 implements (see the cloud-console
//! `docs/WIRE_PROTOCOL.md` §4).
//!
//! ## Repo boundary
//!
//! This crate is the edge mirror of the cloud-console crate of the same
//! name. Both repos hold byte-identical copies of `proto/v1.json` and
//! `proto/generated/rust/v1.rs`; the cloud-console `cargo xtask
//! sync-cloud-protocol --core <path>` keeps the edge copy in lockstep.
//! Per REPO_BOUNDARY R1, neither repo imports a `nexus-*` crate from the
//! other — both regenerate from the same source schema independently.

#![forbid(unsafe_code)]

/// Wire-protocol version 1. Generated from `proto/v1.json`.
pub mod v1 {
    #![allow(clippy::pub_underscore_fields)]
    #![allow(clippy::struct_excessive_bools)]
    #![allow(clippy::large_enum_variant)]
    #![allow(clippy::doc_markdown)]
    #![allow(clippy::derive_partial_eq_without_eq)]
    #![allow(missing_docs)]

    include!("../../../proto/generated/rust/v1.rs");
}
