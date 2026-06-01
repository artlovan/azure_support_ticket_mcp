//! Configuration model.
//!
//! Loads `~/.azure-support-ticket-mcp/config.toml` (or the path given via
//! `--config`), applies env-var overrides prefixed `AZURE_SUPPORT_TICKET_MCP_*`,
//! and resolves any `~` paths to the user's home directory.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

pub const APP_DIR_NAME: &str = ".azure-support-ticket-mcp";
pub const ENV_PREFIX: &str = "AZURE_SUPPORT_TICKET_MCP_";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub auth: Auth,
    pub cache: Cache,
    pub drafts: Drafts,
    pub seed: Seed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    pub cloud: String,
    pub log_level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Auth {
    /// "env" or "az_cli". The chain still falls back unless explicitly disabled.
    pub prefer: String,
    pub allow_az_cli_fallback: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Cache {
    pub path: PathBuf,
    pub services_ttl_hours: u32,
    pub classifications_ttl_hours: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Drafts {
    /// "memory" or "sqlite".
    pub store: String,
    pub sqlite_path: PathBuf,
    pub ttl_days: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Seed {
    pub auto_download: bool,
    pub release_url_template: String,
}

impl Default for Config {
    fn default() -> Self {
        let app_dir = default_app_dir();
        Self {
            general: General {
                cloud: "AzurePublicCloud".into(),
                log_level: "info".into(),
            },
            auth: Auth {
                prefer: "env".into(),
                allow_az_cli_fallback: true,
            },
            cache: Cache {
                path: app_dir.join("cache.sqlite"),
                services_ttl_hours: 24,
                classifications_ttl_hours: 24 * 7,
            },
            drafts: Drafts {
                store: "memory".into(),
                sqlite_path: app_dir.join("drafts.sqlite"),
                ttl_days: 7,
            },
            seed: Seed {
                auto_download: false, // MVP: embedded only by default
                release_url_template:
                    "https://github.com/OWNER/REPO/releases/download/v{version}/support_services_seed.json"
                        .into(),
            },
        }
    }
}

impl Default for General {
    fn default() -> Self {
        Config::default().general
    }
}
impl Default for Auth {
    fn default() -> Self {
        Config::default().auth
    }
}
impl Default for Cache {
    fn default() -> Self {
        Config::default().cache
    }
}
impl Default for Drafts {
    fn default() -> Self {
        Config::default().drafts
    }
}
impl Default for Seed {
    fn default() -> Self {
        Config::default().seed
    }
}

impl Config {
    /// Load config from disk, then apply env-var overrides.
    pub fn load(explicit: Option<&Path>) -> AppResult<Self> {
        let path = explicit
            .map(PathBuf::from)
            .unwrap_or_else(default_config_path);

        let mut cfg = if path.exists() {
            let body = std::fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?;
            toml::from_str::<Config>(&body)?
        } else {
            Config::default()
        };

        cfg.apply_env_overrides();
        cfg.expand_paths();
        cfg.validate()?;
        Ok(cfg)
    }

    fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var(format!("{ENV_PREFIX}CLOUD")) {
            self.general.cloud = v;
        }
        if let Ok(v) = std::env::var(format!("{ENV_PREFIX}LOG_LEVEL")) {
            self.general.log_level = v;
        }
        if let Ok(v) = std::env::var(format!("{ENV_PREFIX}AUTH_PREFER")) {
            self.auth.prefer = v;
        }
        if let Ok(v) = std::env::var(format!("{ENV_PREFIX}AUTH_ALLOW_AZ_CLI_FALLBACK")) {
            self.auth.allow_az_cli_fallback = parse_bool(&v);
        }
        if let Ok(v) = std::env::var(format!("{ENV_PREFIX}CACHE_PATH")) {
            self.cache.path = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var(format!("{ENV_PREFIX}DRAFTS_STORE")) {
            self.drafts.store = v;
        }
        if let Ok(v) = std::env::var(format!("{ENV_PREFIX}DRAFTS_SQLITE_PATH")) {
            self.drafts.sqlite_path = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var(format!("{ENV_PREFIX}DRAFTS_TTL_DAYS")) {
            if let Ok(n) = v.parse() {
                self.drafts.ttl_days = n;
            }
        }
        if let Ok(v) = std::env::var(format!("{ENV_PREFIX}SEED_AUTO_DOWNLOAD")) {
            self.seed.auto_download = parse_bool(&v);
        }
    }

    fn expand_paths(&mut self) {
        self.cache.path = expand_tilde(&self.cache.path);
        self.drafts.sqlite_path = expand_tilde(&self.drafts.sqlite_path);
    }

    fn validate(&self) -> AppResult<()> {
        match self.auth.prefer.as_str() {
            "env" | "az_cli" => {}
            other => {
                return Err(AppError::Config(format!(
                    "auth.prefer must be 'env' or 'az_cli', got '{other}'"
                )))
            }
        }
        match self.drafts.store.as_str() {
            "memory" | "sqlite" => {}
            other => {
                return Err(AppError::Config(format!(
                    "drafts.store must be 'memory' or 'sqlite', got '{other}'"
                )))
            }
        }
        Ok(())
    }

    /// Directory containing cache/drafts/config. Convenience for callers.
    pub fn app_dir(&self) -> PathBuf {
        self.cache
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(default_app_dir)
    }
}

fn parse_bool(v: &str) -> bool {
    matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

fn default_app_dir() -> PathBuf {
    if let Ok(v) = std::env::var(format!("{ENV_PREFIX}HOME")) {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_DIR_NAME)
}

fn default_config_path() -> PathBuf {
    default_app_dir().join("config.toml")
}

fn expand_tilde(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    p.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        let cfg = Config::default();
        cfg.validate().unwrap();
        assert_eq!(cfg.general.cloud, "AzurePublicCloud");
        assert_eq!(cfg.drafts.ttl_days, 7);
    }

    #[test]
    fn rejects_bad_auth_prefer() {
        let mut cfg = Config::default();
        cfg.auth.prefer = "garbage".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn expand_tilde_replaces_home() {
        let p = PathBuf::from("~/foo");
        let e = expand_tilde(&p);
        assert_ne!(e, p);
        assert!(e.ends_with("foo"));
    }
}
