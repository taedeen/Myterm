//! MyTerm core foundation.
//!
//! Pure-Rust, dependency-free building blocks shared by every client
//! (Windows, Linux, Android, iOS). Anything that needs cryptography,
//! networking or storage lives in the `vault`, `ssh` and `ffi` crates and
//! builds on the types defined here.
//!
//! Module map (spec section in brackets):
//! - [`model`]      hosts, groups and inheritance                (3.1)
//! - [`keybar`]     key-bar profiles and shortcut key encoding     (3.5 A)
//! - [`prompt`]     hidden-input / password prompt detection       (3.3, 3.5 C)
//! - [`inject`]     "Type password" decision logic + secret buffer (3.5 C)
//! - [`scrub`]      secret-pattern filter for history              (3.3)
//! - [`history`]    per-device command history with sync merge     (8.2)
//! - [`completion`] suggestion engine                              (3.3)
//! - [`cdtrack`]    working-directory tracking for combined `cd`s  (3.3)
//! - [`sshkey`]     SSH public key / signature wire encoding       (3.4.1)
//! - [`signer`]     hardware signer interface + host key policy    (3.4.1)
//! - [`knownhosts`] trust-on-first-use host key checks             (3.4)
//! - [`sync`]       versioned record merge for settings sync       (8.3)
//! - [`session`]    output/input tracking + Type password on a channel
//! - [`audit`]      hash-chained audit log                         (6.3)
//! - [`codec`]      binary encoding for storage and sync payloads
//! - [`sha256`]     minimal SHA-256 used for fingerprints and audit

pub mod audit;
pub mod cdtrack;
pub mod codec;
pub mod completion;
pub mod history;
pub mod inject;
pub mod keybar;
pub mod knownhosts;
pub mod model;
pub mod prompt;
pub mod scrub;
pub mod session;
pub mod sha256;
pub mod signer;
pub mod sshkey;
pub mod sync;

/// Stable identifier for any record (UUID string in practice).
pub type Id = String;

/// Identifier of one installed MyTerm device (one per app install).
pub type DeviceId = String;
