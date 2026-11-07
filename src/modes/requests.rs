//! Requests one mode sends to whichever mode can handle them (see [`super::Mode::handle_request`]).
//! They live here, not in the modes, so modes never depend on each other.

use std::path::PathBuf;

/// Open a new terminal tab whose shell starts in `cwd`, optionally typing a command into it.
#[derive(Debug, Clone)]
pub struct OpenTerminal {
    pub cwd: PathBuf,
    /// Typed into the shell (and run) once it is ready, so it lands in the shell history.
    pub command: Option<String>,
    /// Tab title; defaults to the folder name.
    pub label: Option<String>,
}

impl OpenTerminal {
    pub fn at(cwd: PathBuf) -> Self {
        Self { cwd, command: None, label: None }
    }
}

/// Show `path` in the file browser: an existing tab on that folder, or a new one.
#[derive(Debug, Clone)]
pub struct OpenFiles {
    pub path: PathBuf,
}
