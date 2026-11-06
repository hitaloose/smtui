//! Files mode: a file browser with tabs, multi-selection and two-step copy/move/paste.

pub mod fs;
pub mod local;
pub mod memory;
pub mod ops;
pub mod tab;
pub mod view;

use std::collections::{BTreeSet, HashSet};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Frame, layout::Rect};

use self::{
    fs::{Entry, Fs},
    ops::{Clip, ClipItem, ClipOp, PasteStep},
    tab::FilesTab,
    view::FilesTarget,
};
use super::{EventResult, KeyHint, MenuItem, Mode, ModeCtx, Shared, TabInfo, requests::OpenTerminal};
use crate::{clipboard, event::AppEvent, event::EventSender, ui::dialog::Confirm, ui::hit::HitMap};

const DOUBLE_CLICK: Duration = Duration::from_millis(400);

mod action {
    pub const OPEN: u32 = 1;
    pub const COPY: u32 = 2;
    pub const MOVE: u32 = 3;
    pub const PASTE_HERE: u32 = 4;
    pub const PASTE_INTO: u32 = 5;
    pub const SCP: u32 = 6;
    pub const TERMINAL: u32 = 7;
    pub const DELETE: u32 = 8;
    pub const DELETE_CONFIRMED: u32 = 9;
    pub const COMPRESS: u32 = 10;
    pub const TOGGLE_HIDDEN: u32 = 11;
    pub const UPLOAD: u32 = 12;
    pub const REFRESH: u32 = 13;
}

enum FilesEvent {
    Listed { tab_id: u64, request: u64, result: Result<Vec<Entry>, String> },
    Pasted(PasteReport),
    Deleted(DeleteReport),
    Compressed { tab_id: u64, dir: PathBuf, result: Result<OsString, String> },
}

#[derive(Clone)]
struct DeleteReport {
    dir: PathBuf,
    /// (name, is_dir) of the entries that were removed.
    done: Vec<(OsString, bool)>,
    errors: Vec<String>,
}

#[derive(Clone)]
struct PasteReport {
    op: ClipOp,
    src_dir: PathBuf,
    dest: PathBuf,
    origin_tab: u64,
    done: Vec<PasteStep>,
    skipped: usize,
    errors: Vec<String>,
}

pub struct FilesMode {
    fs: Arc<dyn Fs>,
    tx: EventSender,
    tabs: Vec<FilesTab>,
    active: usize,
    next_id: u64,
    next_request: u64,
    clip: Option<Clip>,
    show_hidden: bool,
    pasting: bool,
    hits: HitMap<FilesTarget>,
    list_body: Rect,
    last_click: Option<(Instant, usize)>,
    /// Folder that "Paste into folder" targets while the menu is open.
    menu_into: Option<PathBuf>,
    /// Folder that "Open in terminal" and "Copy upload command" target while the menu is open.
    menu_folder: Option<PathBuf>,
    /// Entries waiting for the delete confirmation: (folder, [(name, is_dir)]).
    pending_delete: Option<(PathBuf, Vec<(OsString, bool)>)>,
    deleting: bool,
    compressing: bool,
}

impl FilesMode {
    pub fn new(fs: Arc<dyn Fs>, start: PathBuf, shared: &Shared) -> Self {
        let mut mode = Self {
            fs,
            tx: shared.tx.clone(),
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            next_request: 1,
            clip: None,
            show_hidden: shared.config.show_hidden,
            pasting: false,
            hits: HitMap::default(),
            list_body: Rect::default(),
            last_click: None,
            menu_into: None,
            menu_folder: None,
            pending_delete: None,
            deleting: false,
            compressing: false,
        };
        mode.open_tab(start);
        mode
    }

    fn open_tab(&mut self, path: PathBuf) {
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(FilesTab::new(id, path));
        self.active = self.tabs.len() - 1;
        self.load(self.active);
    }

    fn tab(&mut self) -> &mut FilesTab {
        &mut self.tabs[self.active]
    }

    /// Starts listing the folder of tab `idx` on a worker thread.
    fn load(&mut self, idx: usize) {
        let request = self.next_request;
        self.next_request += 1;
        let tab = &mut self.tabs[idx];
        tab.request = request;
        tab.loading = true;
        let (fs, tx, path, tab_id) = (self.fs.clone(), self.tx.clone(), tab.path.clone(), tab.id);
        std::thread::spawn(move || {
            let result = fs.list(&path).map_err(|e| format!("Cannot open {}: {e}", path.display()));
            tx.send(FilesEvent::Listed { tab_id, request, result });
        });
    }

    fn reload_where(&mut self, pred: impl Fn(&FilesTab) -> bool) {
        let idxs: Vec<usize> = (0..self.tabs.len()).filter(|&i| pred(&self.tabs[i])).collect();
        for i in idxs {
            self.load(i);
        }
    }

    fn navigate(&mut self, path: PathBuf) {
        self.tab().navigate(path);
        self.last_click = None;
        self.load(self.active);
    }

    fn go_up(&mut self) {
        if let Some(parent) = self.tab().parent().map(PathBuf::from) {
            self.navigate(parent);
        }
    }

    fn open_entry(&mut self, idx: usize) {
        let tab = self.tab();
        if let Some(e) = tab.entries.get(idx).filter(|e| e.is_dir()) {
            let path = tab.path.join(&e.name);
            self.navigate(path);
        }
    }

    fn list_height(&self) -> usize {
        self.list_body.height.max(1) as usize
    }

    fn move_cursor(&mut self, delta: isize) {
        let h = self.list_height();
        let tab = self.tab();
        if tab.entries.is_empty() {
            return;
        }
        let last = tab.entries.len() as isize - 1;
        tab.cursor = (tab.cursor as isize + delta).clamp(0, last) as usize;
        tab.ensure_cursor_visible(h);
    }

    /// Keyboard actions with nothing selected act on the entry under the cursor.
    fn ensure_selection(&mut self) {
        let tab = self.tab();
        if tab.selection.is_empty() && !tab.entries.is_empty() {
            let c = tab.cursor;
            tab.select_only(c);
        }
    }

    // ---- Copy / move / paste ----

    fn mark(&mut self, op: ClipOp, ctx: &mut ModeCtx) {
        let tab = &self.tabs[self.active];
        let items: Vec<ClipItem> =
            tab.selected_entries().iter().map(|e| ClipItem { name: e.name.clone(), is_dir: e.is_dir() }).collect();
        if items.is_empty() {
            return;
        }
        let label = label_for(items.iter().map(|i| &i.name));
        self.clip = Some(Clip { op, dir: tab.path.clone(), items });
        let msg = match op {
            ClipOp::Copy => format!("Copied: {label}"),
            ClipOp::Move => format!("Marked to move: {label}"),
        };
        ctx.toast(msg, "Open the destination folder and use Paste.");
    }

    fn paste(&mut self, dest: PathBuf, ctx: &mut ModeCtx) {
        let Some(clip) = self.clip.clone() else { return };
        if self.pasting {
            ctx.toast("A paste is already running", "");
            return;
        }
        self.pasting = true;
        let (fs, tx, origin_tab) = (self.fs.clone(), self.tx.clone(), self.tabs[self.active].id);
        std::thread::spawn(move || {
            let mut report = PasteReport {
                op: clip.op,
                src_dir: clip.dir.clone(),
                dest: dest.clone(),
                origin_tab,
                done: Vec::new(),
                skipped: 0,
                errors: Vec::new(),
            };
            match fs.list(&dest) {
                Err(e) => report.errors.push(format!("{}: {e}", dest.display())),
                Ok(list) => {
                    let names: HashSet<String> = list.iter().map(|e| e.name.to_string_lossy().into_owned()).collect();
                    let plan = ops::plan_paste(&clip, &dest, &names);
                    report.skipped = plan.skipped;
                    for step in plan.steps {
                        let res = match clip.op {
                            ClipOp::Copy => fs.copy_to(&step.src, &step.dst),
                            ClipOp::Move => fs.move_to(&step.src, &step.dst),
                        };
                        match res {
                            Ok(()) => report.done.push(step),
                            Err(e) => report.errors.push(format!("{}: {e}", step.src.display())),
                        }
                    }
                }
            }
            tx.send(FilesEvent::Pasted(report));
        });
    }

    fn finish_paste(&mut self, r: PasteReport, ctx: &mut ModeCtx) {
        self.pasting = false;
        if r.op == ClipOp::Move && !r.done.is_empty() {
            // Tabs open inside a moved folder follow it.
            for step in r.done.iter().filter(|s| s.is_dir) {
                for tab in &mut self.tabs {
                    if let Some(new_path) = ops::rewrite_path(&tab.path, &step.src, &step.dst) {
                        tab.path = new_path;
                    }
                }
            }
            for tab in self.tabs.iter_mut().filter(|t| t.path == r.src_dir) {
                tab.selection.clear();
            }
            self.clip = None;
        }
        let new_names: BTreeSet<OsString> = r.done.iter().map(PasteStep::dst_name).collect();
        for tab in self.tabs.iter_mut().filter(|t| t.id == r.origin_tab && t.path == r.dest) {
            tab.select_after_load = Some(new_names.clone());
        }
        let (dest, src) = (r.dest.clone(), r.src_dir.clone());
        let moved: Vec<PathBuf> = r.done.iter().filter(|s| s.is_dir).map(|s| s.dst.clone()).collect();
        self.reload_where(|t| t.path == dest || t.path == src || moved.iter().any(|m| t.path.starts_with(m)));

        let mut detail = Vec::new();
        if r.skipped > 0 {
            let s = if r.skipped == 1 { "item skipped" } else { "items skipped" };
            detail.push(format!("{} {s} (same folder, or a folder into itself).", r.skipped));
        }
        if let Some(first) = r.errors.first() {
            let more = if r.errors.len() > 1 { format!(" (+{} more)", r.errors.len() - 1) } else { String::new() };
            detail.push(format!("{first}{more}"));
        }
        if r.done.is_empty() {
            if detail.is_empty() {
                detail.push("Same folder, or a folder into itself.".into());
            }
            ctx.toast("Nothing to paste", detail.join(" "));
            return;
        }
        let label = label_for(new_names.iter());
        let verb = if r.op == ClipOp::Move { "Moved" } else { "Pasted" };
        ctx.toast(format!("{verb}: {label}"), detail.join(" "));
    }

    fn copy_scp(&mut self, ctx: &mut ModeCtx) {
        let tab = &self.tabs[self.active];
        let items: Vec<(PathBuf, bool)> = tab.selected_entries().iter().map(|e| (tab.path.join(&e.name), e.is_dir())).collect();
        if items.is_empty() {
            return;
        }
        let target = ctx.shared.host.scp_target(&ctx.shared.config.scp);
        let cmd = ops::scp_command(&target, &items);
        let msg = match clipboard::copy(&cmd, ctx.shared.host.in_tmux) {
            clipboard::CopyOutcome::Copied => "scp command copied. Paste it in your local terminal.",
            clipboard::CopyOutcome::Sent => "scp command sent to your clipboard (OSC 52). Paste it in your local terminal.",
            clipboard::CopyOutcome::Failed => "Could not access the clipboard. Copy it manually:",
        };
        ctx.toast(msg, cmd);
    }

    // ---- Delete ----

    /// Asks for confirmation (typing DELETE) before removing the selection.
    fn ask_delete(&mut self, ctx: &mut ModeCtx) {
        if self.deleting {
            ctx.toast("A delete is already running", "");
            return;
        }
        let tab = &self.tabs[self.active];
        let targets: Vec<(OsString, bool)> = tab.selected_entries().iter().map(|e| (e.name.clone(), e.is_dir())).collect();
        if targets.is_empty() {
            return;
        }
        let dirs = targets.iter().filter(|(_, d)| *d).count();
        let what = match (targets.len(), dirs) {
            (1, 1) => "this folder and everything inside it".to_string(),
            (1, _) => "this file".to_string(),
            (n, 0) => format!("these {n} files"),
            (n, _) => format!("these {n} items (folders with everything inside them)"),
        };
        let items = targets
            .iter()
            .map(|(n, d)| format!("{}{}", n.to_string_lossy(), if *d { "/" } else { "" }))
            .collect();
        ctx.confirm(Confirm {
            title: format!("Delete {}?", label_for(targets.iter().map(|(n, _)| n))),
            message: format!("This permanently deletes {what} from {}. It cannot be undone.", tab.path.display()),
            items,
            confirm_label: "Delete".into(),
            danger: true,
            require_text: Some("DELETE".into()),
            action: action::DELETE_CONFIRMED,
        });
        self.pending_delete = Some((tab.path.clone(), targets));
    }

    fn delete(&mut self) {
        let Some((dir, targets)) = self.pending_delete.take() else { return };
        self.deleting = true;
        let (fs, tx) = (self.fs.clone(), self.tx.clone());
        std::thread::spawn(move || {
            let mut report = DeleteReport { dir: dir.clone(), done: Vec::new(), errors: Vec::new() };
            for (name, is_dir) in targets {
                let path = dir.join(&name);
                match fs.remove(&path) {
                    Ok(()) => report.done.push((name, is_dir)),
                    Err(e) => report.errors.push(format!("{}: {e}", path.display())),
                }
            }
            tx.send(FilesEvent::Deleted(report));
        });
    }

    fn finish_delete(&mut self, r: DeleteReport, ctx: &mut ModeCtx) {
        self.deleting = false;
        let removed: Vec<PathBuf> = r.done.iter().map(|(n, _)| r.dir.join(n)).collect();
        // Tabs that were inside a deleted folder go back to its parent.
        for tab in &mut self.tabs {
            if removed.iter().any(|p| tab.path.starts_with(p)) {
                tab.navigate(r.dir.clone());
            }
        }
        // Deleted items leave the copy/move area.
        if let Some(clip) = &mut self.clip {
            if removed.iter().any(|p| clip.dir.starts_with(p)) {
                self.clip = None;
            } else if clip.dir == r.dir {
                clip.items.retain(|i| !r.done.iter().any(|(n, _)| *n == i.name));
                if clip.items.is_empty() {
                    self.clip = None;
                }
            }
        }
        let dir = r.dir.clone();
        self.reload_where(|t| t.path == dir || t.loading);

        let detail = match r.errors.as_slice() {
            [] => String::new(),
            [one] => one.clone(),
            [first, rest @ ..] => format!("{first} (+{} more)", rest.len()),
        };
        if r.done.is_empty() {
            ctx.toast("Nothing was deleted", detail);
        } else {
            ctx.toast(format!("Deleted: {}", label_for(r.done.iter().map(|(n, _)| n))), detail);
        }
    }

    // ---- Compress ----

    fn compress(&mut self, ctx: &mut ModeCtx) {
        if self.compressing {
            ctx.toast("A compression is already running", "");
            return;
        }
        let tab = &self.tabs[self.active];
        let names: Vec<OsString> = tab.selected_entries().iter().map(|e| e.name.clone()).collect();
        if names.is_empty() {
            return;
        }
        let base = match names.as_slice() {
            [one] => one.to_string_lossy().into_owned(),
            _ => tab.title(),
        };
        let taken: HashSet<String> = tab.entries.iter().map(|e| e.name.to_string_lossy().into_owned()).collect();
        let archive = ops::archive_name(&base, &taken);
        self.compressing = true;
        ctx.toast(format!("Compressing to {archive}…"), "");
        let (fs, tx, dir, tab_id) = (self.fs.clone(), self.tx.clone(), tab.path.clone(), tab.id);
        std::thread::spawn(move || {
            let dest = dir.join(&archive);
            let result = fs
                .compress(&dir, &names, &dest)
                .map(|()| OsString::from(&archive))
                .map_err(|e| format!("{archive}: {e}"));
            tx.send(FilesEvent::Compressed { tab_id, dir, result });
        });
    }

    fn finish_compress(&mut self, tab_id: u64, dir: &PathBuf, result: &Result<OsString, String>, ctx: &mut ModeCtx) {
        self.compressing = false;
        match result {
            Ok(name) => {
                for tab in self.tabs.iter_mut().filter(|t| t.id == tab_id && &t.path == dir) {
                    tab.select_after_load = Some(BTreeSet::from([name.clone()]));
                }
                ctx.toast(format!("Created {}", name.to_string_lossy()), "");
            }
            Err(e) => ctx.toast("Could not create the archive", e.clone()),
        }
        let dir = dir.clone();
        self.reload_where(|t| t.path == dir);
    }

    // ---- Upload (scp from the user's machine) ----

    /// The single selected folder, or the open one.
    fn upload_destination(&self) -> PathBuf {
        let tab = &self.tabs[self.active];
        match tab.selected_entries().as_slice() {
            [one] if one.is_dir() => tab.path.join(&one.name),
            _ => tab.path.clone(),
        }
    }

    fn copy_upload(&mut self, locals: &[String], dest: PathBuf, dropped: bool, ctx: &mut ModeCtx) {
        let target = ctx.shared.host.scp_target(&ctx.shared.config.scp);
        let cmd = ops::scp_upload_command(&target, locals, &dest);
        let outcome = clipboard::copy(&cmd, ctx.shared.host.in_tmux);
        let what = if dropped {
            let n = locals.len();
            format!("Upload command for {n} {} copied", if n == 1 { "item" } else { "items" })
        } else {
            "Upload command copied".to_string()
        };
        let msg = match outcome {
            clipboard::CopyOutcome::Copied | clipboard::CopyOutcome::Sent if dropped => {
                format!("{what}. Run it in a terminal on your computer.")
            }
            clipboard::CopyOutcome::Copied | clipboard::CopyOutcome::Sent => {
                format!("{what}. Replace {} and run it on your computer.", ops::LOCAL_PATH_PLACEHOLDER)
            }
            clipboard::CopyOutcome::Failed => "Could not access the clipboard. Copy it manually:".to_string(),
        };
        ctx.toast(msg, cmd);
    }

    fn toggle_hidden(&mut self, ctx: &mut ModeCtx) {
        self.show_hidden = !self.show_hidden;
        self.reload_where(|_| true);
        ctx.toast(if self.show_hidden { "Showing hidden files" } else { "Hiding hidden files" }, "Press . to toggle.");
    }

    // ---- Context menu ----

    /// `on_item`: the menu was opened on an entry (the selection), not on empty space.
    fn open_menu(&mut self, x: u16, y: u16, on_item: bool, ctx: &mut ModeCtx) {
        let tab = &self.tabs[self.active];
        let selected = tab.selected_entries();
        let n = selected.len();
        let suffix = if n > 1 { format!(" {n} items") } else { String::new() };
        let single_dir = (n == 1 && selected[0].is_dir()).then(|| tab.path.join(&selected[0].name));
        let paste_hint = self.clip.as_ref().map_or(String::new(), |c| {
            let n = c.items.len();
            let op = if c.op == ClipOp::Move { "move" } else { "copy" };
            format!("{n} {} · {op}", if n == 1 { "item" } else { "items" })
        });
        let can_paste = self.clip.is_some() && !self.pasting;

        let mut items = Vec::new();
        self.menu_into = None;
        if on_item && n > 0 {
            if single_dir.is_some() {
                items.push(MenuItem::action("Open", action::OPEN));
            }
            items.push(MenuItem::action(format!("Copy{suffix}"), action::COPY));
            items.push(MenuItem::action(format!("Move{suffix}"), action::MOVE));
            let paste = match &single_dir {
                Some(dir) => {
                    self.menu_into = Some(dir.clone());
                    MenuItem::action("Paste into folder", action::PASTE_INTO)
                }
                None => MenuItem::action("Paste here", action::PASTE_HERE),
            };
            items.push(paste.with_hint(paste_hint).enabled(can_paste));
            items.push(MenuItem::Separator);
            items.push(MenuItem::action("Compress to .tar.gz", action::COMPRESS).with_hint("z").enabled(!self.compressing));
            items.push(MenuItem::action("Copy scp command", action::SCP));
            items.push(MenuItem::action(
                if single_dir.is_some() { "Copy upload command" } else { "Copy upload command here" },
                action::UPLOAD,
            ).with_hint("u"));
            items.push(match &single_dir {
                Some(_) => MenuItem::action("Open in terminal", action::TERMINAL),
                None => MenuItem::action("Open terminal here", action::TERMINAL),
            });
            items.push(MenuItem::Separator);
            items.push(
                MenuItem::action(format!("Delete{suffix}"), action::DELETE).with_hint("Del").danger().enabled(!self.deleting),
            );
        } else {
            items.push(MenuItem::action("Paste here", action::PASTE_HERE).with_hint(paste_hint).enabled(can_paste));
            items.push(MenuItem::Separator);
            items.push(MenuItem::action("Copy upload command here", action::UPLOAD).with_hint("u"));
            items.push(MenuItem::action("Open terminal here", action::TERMINAL));
        }
        self.menu_folder = Some(match (&single_dir, on_item) {
            (Some(dir), true) => dir.clone(),
            _ => self.tabs[self.active].path.clone(),
        });
        ctx.open_menu(x, y, items);
    }

    /// Menu key: acts on the selection, or on the cursor entry when nothing is selected.
    fn open_menu_from_keyboard(&mut self, ctx: &mut ModeCtx) {
        let body = self.list_body;
        self.ensure_selection();
        let tab = self.tab();
        let on_item = !tab.selection.is_empty();
        let row = tab.cursor.saturating_sub(tab.scroll) as u16;
        self.open_menu(body.x + 4, body.y + row + 1, on_item, ctx);
    }

    fn handle_list_click(&mut self, ev: MouseEvent, target: Option<FilesTarget>, ctx: &mut ModeCtx) {
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
        match (ev.kind, target) {
            (MouseEventKind::Down(MouseButton::Left), Some(FilesTarget::Up)) => self.go_up(),
            (MouseEventKind::Down(MouseButton::Left), Some(FilesTarget::Refresh)) => self.load(self.active),
            (MouseEventKind::Down(MouseButton::Left), Some(FilesTarget::Crumb(p))) => {
                if p != self.tabs[self.active].path {
                    self.navigate(p);
                }
            }
            (MouseEventKind::Down(MouseButton::Left), Some(FilesTarget::Row(i))) => {
                let now = Instant::now();
                let double = !ctrl && !shift && self.last_click.is_some_and(|(t, r)| r == i && now - t < DOUBLE_CLICK);
                let tab = self.tab();
                if ctrl {
                    tab.toggle(i);
                } else if shift {
                    let anchor = tab.cursor;
                    tab.select_range(anchor, i);
                } else {
                    tab.select_only(i);
                }
                tab.cursor = i;
                if double {
                    self.last_click = None;
                    self.open_entry(i);
                } else {
                    self.last_click = Some((now, i));
                }
            }
            (MouseEventKind::Down(MouseButton::Right), Some(FilesTarget::Row(i))) => {
                let tab = self.tab();
                if !tab.is_selected(i) {
                    tab.select_only(i);
                }
                tab.cursor = i;
                self.open_menu(ev.column, ev.row, true, ctx);
            }
            (MouseEventKind::Down(button), None) if self.list_body.contains((ev.column, ev.row).into()) => {
                if button == MouseButton::Right {
                    self.open_menu(ev.column, ev.row, false, ctx);
                } else {
                    self.tab().selection.clear();
                }
            }
            _ => {}
        }
    }
}

fn label_for<'a>(names: impl Iterator<Item = &'a OsString>) -> String {
    let names: Vec<_> = names.collect();
    match names.as_slice() {
        [one] => one.to_string_lossy().into_owned(),
        many => format!("{} items", many.len()),
    }
}

impl Mode for FilesMode {
    fn name(&self) -> &str {
        "Files"
    }

    fn icon(&self) -> &str {
        "▤"
    }

    fn tabs(&self) -> Vec<TabInfo> {
        self.tabs.iter().map(|t| TabInfo { title: t.title() }).collect()
    }

    fn active_tab(&self) -> usize {
        self.active
    }

    fn select_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.active = idx;
            self.last_click = None;
        }
    }

    fn new_tab(&mut self, _ctx: &mut ModeCtx) {
        let path = self.tabs[self.active].path.clone();
        self.open_tab(path);
    }

    fn close_tab(&mut self, idx: usize, _ctx: &mut ModeCtx) {
        if self.tabs.len() > 1 && idx < self.tabs.len() {
            self.tabs.remove(idx);
            if self.active > idx || self.active >= self.tabs.len() {
                self.active = self.active.saturating_sub(1);
            }
        }
    }

    fn toolbar_height(&self) -> u16 {
        3
    }

    fn render_toolbar(&mut self, f: &mut Frame, area: Rect, shared: &Shared) {
        // The toolbar is drawn first each frame, so it resets the hit map.
        self.hits.clear();
        view::render_toolbar(f, area, &shared.theme, &self.tabs[self.active], &mut self.hits);
    }

    fn render(&mut self, f: &mut Frame, area: Rect, shared: &Shared) {
        let tab = &mut self.tabs[self.active];
        self.list_body = view::render_list(f, area, &shared.theme, tab, self.clip.as_ref(), &mut self.hits);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &mut ModeCtx) -> EventResult {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.list_height() as isize;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') if !ctrl => self.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') if !ctrl => self.move_cursor(1),
            KeyCode::PageUp => self.move_cursor(-page),
            KeyCode::PageDown => self.move_cursor(page),
            KeyCode::Home | KeyCode::Char('g') => self.move_cursor(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.move_cursor(isize::MAX / 2),
            KeyCode::Char(' ') => {
                let tab = self.tab();
                let c = tab.cursor;
                tab.toggle(c);
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                let c = self.tabs[self.active].cursor;
                self.open_entry(c);
            }
            KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => self.go_up(),
            KeyCode::Esc => self.tab().selection.clear(),
            KeyCode::Char('a') if ctrl => {
                let tab = self.tab();
                tab.selection = tab.entries.iter().map(|e| e.name.clone()).collect();
            }
            KeyCode::Menu | KeyCode::Char('o') => self.open_menu_from_keyboard(ctx),
            KeyCode::F(10) if key.modifiers.contains(KeyModifiers::SHIFT) => self.open_menu_from_keyboard(ctx),
            KeyCode::Char('c') if !ctrl => {
                self.ensure_selection();
                self.mark(ClipOp::Copy, ctx);
            }
            KeyCode::Char('x') | KeyCode::Char('m') if !ctrl => {
                self.ensure_selection();
                self.mark(ClipOp::Move, ctx);
            }
            KeyCode::Char('v') | KeyCode::Char('p') if !ctrl => {
                let dest = self.tabs[self.active].path.clone();
                self.paste(dest, ctx);
            }
            KeyCode::Char('s') if !ctrl => {
                self.ensure_selection();
                self.copy_scp(ctx);
            }
            KeyCode::Char('r') if !ctrl => self.load(self.active),
            KeyCode::Char('t') if !ctrl => {
                // The folder under the cursor, or the current folder.
                let tab = &self.tabs[self.active];
                let cwd = match tab.entries.get(tab.cursor) {
                    Some(e) if e.is_dir() && tab.selection.contains(&e.name) => tab.path.join(&e.name),
                    _ => tab.path.clone(),
                };
                ctx.request(OpenTerminal { cwd });
            }
            KeyCode::Char('.') => self.toggle_hidden(ctx),
            KeyCode::Char('u') if !ctrl => {
                let dest = self.upload_destination();
                self.copy_upload(&[ops::LOCAL_PATH_PLACEHOLDER.to_string()], dest, false, ctx);
            }
            KeyCode::Char('z') if !ctrl => {
                self.ensure_selection();
                self.compress(ctx);
            }
            KeyCode::Delete | KeyCode::Char('d') if !ctrl => {
                self.ensure_selection();
                self.ask_delete(ctx);
            }
            _ => return EventResult::Ignored,
        }
        EventResult::Consumed
    }

    /// Files dropped on the terminal arrive as pasted local paths: turn them into an upload command.
    fn handle_paste(&mut self, text: &str, ctx: &mut ModeCtx) -> EventResult {
        match ops::parse_dropped_paths(text) {
            Some(paths) => {
                let dest = self.upload_destination();
                self.copy_upload(&paths, dest, true, ctx);
                EventResult::Consumed
            }
            None => {
                ctx.toast("Pasted text ignored", "Drop files from your computer here to get an upload command.");
                EventResult::Consumed
            }
        }
    }

    fn handle_mouse(&mut self, ev: MouseEvent, ctx: &mut ModeCtx) -> EventResult {
        let over_list = self.list_body.contains((ev.column, ev.row).into());
        match ev.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if over_list => {
                let tab = self.tab();
                let max = tab.entries.len().saturating_sub(1);
                tab.scroll = if ev.kind == MouseEventKind::ScrollUp {
                    tab.scroll.saturating_sub(3)
                } else {
                    (tab.scroll + 3).min(max)
                };
                // Keep the cursor on screen so the next render does not scroll back.
                let h = self.list_body.height as usize;
                let tab = self.tab();
                tab.cursor = tab.cursor.clamp(tab.scroll, (tab.scroll + h).saturating_sub(1).min(max));
            }
            MouseEventKind::Down(_) => {
                let target = self.hits.hit(ev.column, ev.row);
                self.handle_list_click(ev, target, ctx);
            }
            _ => return EventResult::Ignored,
        }
        EventResult::Consumed
    }

    fn handle_app_event(&mut self, ev: &AppEvent, ctx: &mut ModeCtx) -> EventResult {
        let Some(ev) = ev.get::<FilesEvent>() else { return EventResult::Ignored };
        match ev {
            FilesEvent::Listed { tab_id, request, result } => {
                let show_hidden = self.show_hidden;
                if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == *tab_id && t.request == *request) {
                    match result {
                        Ok(entries) => {
                            let mut entries: Vec<Entry> = entries
                                .iter()
                                .filter(|e| show_hidden || !e.name.to_string_lossy().starts_with('.'))
                                .cloned()
                                .collect();
                            ops::sort_entries(&mut entries);
                            tab.apply_listing(entries);
                        }
                        Err(msg) => {
                            tab.loading = false;
                            tab.entries.clear();
                            tab.error = Some(msg.clone());
                        }
                    }
                }
            }
            FilesEvent::Deleted(report) => self.finish_delete(report.clone(), ctx),
            FilesEvent::Compressed { tab_id, dir, result } => self.finish_compress(*tab_id, dir, result, ctx),
            FilesEvent::Pasted(report) => {
                self.finish_paste(report.clone(), ctx);
            }
        }
        EventResult::Consumed
    }

    fn on_action(&mut self, action: u32, ctx: &mut ModeCtx) {
        match action {
            action::OPEN => {
                if let Some(dir) = self.menu_into.take() {
                    self.navigate(dir);
                }
            }
            action::COPY => self.mark(ClipOp::Copy, ctx),
            action::MOVE => self.mark(ClipOp::Move, ctx),
            action::PASTE_HERE => {
                let dest = self.tabs[self.active].path.clone();
                self.paste(dest, ctx);
            }
            action::PASTE_INTO => {
                if let Some(dir) = self.menu_into.take() {
                    self.paste(dir, ctx);
                }
            }
            action::SCP => self.copy_scp(ctx),
            action::DELETE => self.ask_delete(ctx),
            action::COMPRESS => self.compress(ctx),
            action::TOGGLE_HIDDEN => self.toggle_hidden(ctx),
            action::REFRESH => self.load(self.active),
            action::UPLOAD => {
                let dest = self.menu_folder.clone().unwrap_or_else(|| self.upload_destination());
                self.copy_upload(&[ops::LOCAL_PATH_PLACEHOLDER.to_string()], dest, false, ctx);
            }
            action::DELETE_CONFIRMED => self.delete(),
            action::TERMINAL => {
                if let Some(cwd) = self.menu_folder.clone() {
                    ctx.request(OpenTerminal { cwd });
                }
            }
            _ => {}
        }
    }

    fn status(&self, shared: &Shared) -> String {
        let tab = &self.tabs[self.active];
        let selected = tab.selected_entries();
        let sel = match selected.as_slice() {
            [] => String::new(),
            [one] if one.is_dir() => format!(" · {}", one.name.to_string_lossy()),
            [one] => format!(" · {} · {}", one.name.to_string_lossy(), ops::human_size(one.size)),
            many => format!(" · {} selected", many.len()),
        };
        let clip = self.clip.as_ref().map_or(String::new(), |c| {
            format!(" · {} to {}", c.items.len(), if c.op == ClipOp::Move { "move" } else { "copy" })
        });
        let busy = if self.pasting {
            " · pasting…"
        } else if self.deleting {
            " · deleting…"
        } else if self.compressing {
            " · compressing…"
        } else {
            ""
        };
        format!("{}{sel}{clip}{busy}", shared.host.label())
    }

    fn hints(&self, shared: &Shared) -> Vec<KeyHint> {
        vec![KeyHint::new(crate::keys::display(&shared.config.leader), "Shortcuts").opens_shortcuts()]
    }

    fn shortcut_items(&self) -> Vec<MenuItem> {
        let label = if self.show_hidden { "Hide hidden files" } else { "Show hidden files" };
        vec![
            MenuItem::action("Refresh", action::REFRESH).with_hint("r"),
            MenuItem::action(label, action::TOGGLE_HIDDEN).with_hint("."),
        ]
    }
}
