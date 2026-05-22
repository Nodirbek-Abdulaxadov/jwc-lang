//! Package registry client.
//!
//! Talks to a Cargo-shaped HTTP registry:
//! - `GET {base}/api/v1/crates/{name}` → JSON list of versions
//! - `GET {base}/api/v1/crates/{name}/{version}/download` → tarball
//!
//! Auth tokens are read from `~/.jwc/credentials.json` when available and
//! sent as `Authorization: Bearer <token>` headers. The `jwc login` command
//! that writes that file is out of scope for the v1 cut.

pub mod client;
pub mod config;
