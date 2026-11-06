//! User configuration loaded from `~/.config/smtui/config.toml`. Every key is optional.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Accent color as `#rrggbb`.
    pub accent: String,
    /// Prefix key for global shortcuts while a terminal has focus, e.g. `ctrl-]`.
    pub leader: String,
    /// Show dotfiles in the Files mode.
    pub show_hidden: bool,
    /// Force (`true`) or disable (`false`) 24-bit color. Auto-detected when unset.
    pub truecolor: Option<bool>,
    /// Overrides for the `scp` command copied from the Files mode.
    pub scp: ScpConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScpConfig {
    /// Host (or `~/.ssh/config` alias) the user reaches this server with.
    pub host: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            accent: "#5eead4".into(),
            leader: "ctrl-]".into(),
            show_hidden: true,
            truecolor: None,
            scp: ScpConfig::default(),
        }
    }
}

impl Config {
    pub fn default_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| dirs::home_dir().map(|h| h.join(".config")))?;
        Some(base.join("smtui").join("config.toml"))
    }

    /// Loads the config from `path` (or the default location). A missing file yields the defaults.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let path = match path {
            Some(p) => p.to_path_buf(),
            None => match Self::default_path() {
                Some(p) if p.exists() => p,
                _ => return Ok(Self::default()),
            },
        };
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read config file {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("invalid config file {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_partial_config() {
        let c: Config = toml::from_str("accent = \"#fbbf24\"\n[scp]\nhost = \"edge1\"\n").unwrap();
        assert_eq!(c.accent, "#fbbf24");
        assert_eq!(c.leader, "ctrl-]");
        assert_eq!(c.scp.host.as_deref(), Some("edge1"));
    }
}
