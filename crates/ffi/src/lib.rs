//! MyTerm API for the Flutter apps.
//!
//! `flutter_rust_bridge_codegen generate` reads `api` and produces
//! `frb_generated.rs` here and the Dart bindings in
//! `apps/mobile/lib/src/rust/`. Both are generated in CI and git-ignored.

pub mod api;
pub mod native;
mod dpapi;
#[cfg(feature = "flutter")]
mod frb_generated;
