//! Small preferences smtui remembers between runs, in `~/.local/state/smtui/state.toml`
//! (unlike `config.toml`, which only the user edits). Failures are logged, never fatal.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Last "show hidden files" choice in the Files mode.
    pub show_hidden: Option<bool>,
}

/// Where the state lives; `None` (e.g. in `--demo`) keeps everything in memory.
#[derive(Debug, Clone, Default)]
pub struct StateStore {
    path: Option<PathBuf>,
}

impl StateStore {
    pub fn default_location() -> Self {
        Self { path: crate::run::state_dir().map(|d| d.join("state.toml")) }
    }

    /// Never reads or writes anything.
    pub fn in_memory() -> Self {
        Self { path: None }
    }

    #[cfg(test)]
    pub fn at(path: &Path) -> Self {
        Self { path: Some(path.to_path_buf()) }
    }

    /// False in `--demo`: nothing should be read from or written to the user's files.
    pub fn is_persistent(&self) -> bool {
        self.path.is_some()
    }

    pub fn load(&self) -> State {
        let Some(path) = &self.path else { return State::default() };
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!(path = %path.display(), error = %e, "ignoring an unreadable state file");
                State::default()
            }),
            Err(_) => State::default(),
        }
    }

    /// Reads the current state, applies `change` and writes it back.
    pub fn update(&self, change: impl FnOnce(&mut State)) {
        let Some(path) = &self.path else { return };
        let mut state = self.load();
        change(&mut state);
        if let Err(e) = write(path, &state) {
            tracing::warn!(path = %path.display(), error = %e, "could not save the state");
        }
    }
}

fn write(path: &Path, state: &State) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Write a temporary file and rename it, so a crash never leaves half a file.
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, toml::to_string(state)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_hidden_files_choice() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::at(&dir.path().join("sub/state.toml"));
        assert_eq!(store.load().show_hidden, None);
        store.update(|s| s.show_hidden = Some(false));
        assert_eq!(store.load().show_hidden, Some(false));
        store.update(|s| s.show_hidden = Some(true));
        assert_eq!(store.load().show_hidden, Some(true));
    }

    #[test]
    fn broken_or_in_memory_state_is_harmless() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        std::fs::write(&path, "show_hidden = \"yes\"").unwrap();
        assert_eq!(StateStore::at(&path).load(), State::default());
        let mem = StateStore::in_memory();
        mem.update(|s| s.show_hidden = Some(false));
        assert_eq!(mem.load(), State::default());
    }
}
