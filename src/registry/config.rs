//! Registry configuration (URL, auth) sourced from env > manifest > built-in.

use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::pkg_cache;

/// Stored in `~/.jwc/credentials.json` (mode 0600 on unix; on windows the
/// file lives in the user profile and inherits ACLs). Tokens are sent as
/// `Authorization: Bearer <token>` to the matching registry host.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Credentials {
    /// host → bearer-token map.
    #[serde(default)]
    pub tokens: std::collections::BTreeMap<String, String>,
}

impl Credentials {
    pub fn path() -> PathBuf {
        pkg_cache::jwc_home().join("credentials.json")
    }

    pub fn load() -> Result<Self> {
        let path = Self::path();
        if !path.is_file() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path)?;
        let parsed: Self = serde_json::from_str(&raw).unwrap_or_default();
        Ok(parsed)
    }

    pub fn token_for_host(&self, host: &str) -> Option<&str> {
        self.tokens.get(host).map(|s| s.as_str())
    }
}
