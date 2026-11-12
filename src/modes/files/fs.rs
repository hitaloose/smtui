//! File system abstraction used by the Files mode. Calls are blocking and always run on
//! a worker thread, never on the UI loop.

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: OsString,
    /// Symlinks report the kind of their target, so links to folders can be opened.
    pub kind: EntryKind,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub symlink: bool,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Dir
    }
}

pub trait Fs: Send + Sync {
    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>>;
    /// Moves `from` to `to`; fails if `to` exists.
    fn move_to(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Copies `from` (recursively for folders) to `to`; fails if `to` exists.
    fn copy_to(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Creates the empty folder `path`; fails if it exists.
    fn create_dir(&self, path: &Path) -> io::Result<()>;
    /// Creates the empty file `path`; fails if it exists (never truncates).
    fn create_file(&self, path: &Path) -> io::Result<()>;
    /// Deletes `path` permanently: folders with their content; symlinks themselves, never their target.
    fn remove(&self, path: &Path) -> io::Result<()>;
    /// Creates the gzip-compressed tarball `dest` from the entries `names` of `dir`; fails if `dest` exists.
    fn compress(&self, dir: &Path, names: &[std::ffi::OsString], dest: &Path) -> io::Result<()>;
    /// Extracts `archive` into its folder without overwriting anything (see
    /// [`super::ops::place_extracted`]) and returns the name of the entry it created.
    fn extract(&self, archive: &Path) -> io::Result<OsString>;
}
