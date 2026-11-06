//! Requests one mode sends to whichever mode can handle them (see [`super::Mode::handle_request`]).
//! They live here, not in the modes, so modes never depend on each other.

use std::path::PathBuf;

/// Open a new terminal tab whose shell starts in `cwd`.
#[derive(Debug, Clone)]
pub struct OpenTerminal {
    pub cwd: PathBuf,
}
