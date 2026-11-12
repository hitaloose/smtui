//! Requests a pane sends to the app (see [`super::Effect::Request`]). They live here, not in
//! the panes, so panes never depend on each other.

use std::path::PathBuf;

/// Open a new tab (Files + terminal) in `cwd`, with the focus on its terminal.
#[derive(Debug, Clone)]
pub struct OpenTab {
    pub cwd: PathBuf,
}

/// Show `path` in the Files pane of the current tab (the terminal is already there).
#[derive(Debug, Clone)]
pub struct ShowInFiles {
    pub path: PathBuf,
}
