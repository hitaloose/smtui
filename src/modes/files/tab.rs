//! State of one Files tab: its folder, listing, selection and keyboard cursor.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::fs::Entry;

pub struct FilesTab {
    pub id: u64,
    pub path: PathBuf,
    pub entries: Vec<Entry>,
    /// Names (inside `path`) of the selected entries.
    pub selection: BTreeSet<OsString>,
    pub cursor: usize,
    pub scroll: usize,
    pub loading: bool,
    pub error: Option<String>,
    /// Id of the listing request whose answer this tab is waiting for.
    pub request: u64,
    /// After the next listing: put the cursor here (e.g. the folder we came up from).
    pub focus_after_load: Option<OsString>,
    /// After the next listing: select these names (e.g. freshly pasted items).
    pub select_after_load: Option<BTreeSet<OsString>>,
}

impl FilesTab {
    pub fn new(id: u64, path: PathBuf) -> Self {
        Self {
            id,
            path,
            entries: Vec::new(),
            selection: BTreeSet::new(),
            cursor: 0,
            scroll: 0,
            loading: true,
            error: None,
            request: 0,
            focus_after_load: None,
            select_after_load: None,
        }
    }

    pub fn title(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "root".into())
    }

    pub fn is_selected(&self, idx: usize) -> bool {
        self.entries.get(idx).is_some_and(|e| self.selection.contains(&e.name))
    }

    pub fn selected_entries(&self) -> Vec<&Entry> {
        self.entries.iter().filter(|e| self.selection.contains(&e.name)).collect()
    }

    pub fn select_only(&mut self, idx: usize) {
        self.selection.clear();
        if let Some(e) = self.entries.get(idx) {
            self.selection.insert(e.name.clone());
        }
    }

    pub fn toggle(&mut self, idx: usize) {
        if let Some(e) = self.entries.get(idx)
            && !self.selection.remove(&e.name)
        {
            self.selection.insert(e.name.clone());
        }
    }

    pub fn select_range(&mut self, from: usize, to: usize) {
        let (a, b) = if from <= to { (from, to) } else { (to, from) };
        self.selection.clear();
        for e in self.entries.iter().skip(a).take(b - a + 1) {
            self.selection.insert(e.name.clone());
        }
    }

    /// Switches folder; the caller starts the listing.
    pub fn navigate(&mut self, path: PathBuf) {
        let came_from = self.path.strip_prefix(&path).ok().and_then(|rest| rest.iter().next()).map(OsString::from);
        self.path = path;
        self.entries.clear();
        self.selection.clear();
        self.cursor = 0;
        self.scroll = 0;
        self.error = None;
        self.loading = true;
        self.focus_after_load = came_from;
    }

    pub fn parent(&self) -> Option<&Path> {
        self.path.parent()
    }

    /// Applies a finished listing (already sorted and filtered).
    pub fn apply_listing(&mut self, entries: Vec<Entry>) {
        self.entries = entries;
        self.loading = false;
        self.error = None;
        let names: BTreeSet<OsString> = self.entries.iter().map(|e| e.name.clone()).collect();
        if let Some(sel) = self.select_after_load.take() {
            self.selection = sel;
        }
        self.selection.retain(|n| names.contains(n));
        let focus = self.focus_after_load.take().or_else(|| self.selection.iter().next().cloned());
        if let Some(i) = focus.and_then(|n| self.entries.iter().position(|e| e.name == n)) {
            self.cursor = i;
        }
        self.cursor = self.cursor.min(self.entries.len().saturating_sub(1));
    }

    /// Keeps the cursor inside the visible window of `height` rows.
    pub fn ensure_cursor_visible(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + height {
            self.scroll = self.cursor + 1 - height;
        }
        self.scroll = self.scroll.min(self.entries.len().saturating_sub(height));
    }
}
