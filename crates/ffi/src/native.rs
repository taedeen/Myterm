//! Types for Rust callers of the API that aren't the Flutter apps (the Linux
//! command line). Kept out of `api` so the Dart binding generator skips them.

use std::sync::Arc;

use flutter_rust_bridge::DartFnFuture;

use crate::api::SessionEventDto;

/// Session events, delivered by callback.
pub type EventFn = Arc<dyn Fn(SessionEventDto) + Send + Sync>;
/// Hardware-key signing: (key id, data) -> DER signature; empty = refused.
pub type SignFn = Arc<dyn Fn(String, Vec<u8>) -> DartFnFuture<Vec<u8>> + Send + Sync>;

/// For callers without hardware keys: refuses every signing request.
pub fn no_hardware_signer() -> SignFn {
    Arc::new(|_key: String, _data: Vec<u8>| -> DartFnFuture<Vec<u8>> { Box::pin(async { Vec::new() }) })
}
