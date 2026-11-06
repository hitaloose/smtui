//! Pure logic of the Files mode: sorting, copy/move planning, path rewriting and the
//! `scp` command. Nothing here touches the file system, so it is all unit tested.

use std::borrow::Cow;
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Datelike, Local};

use super::fs::{Entry, EntryKind};
use crate::host::ScpTarget;

/// Folders first, then by name (case-insensitive, ties broken by the raw name).
pub fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        let dir = |e: &Entry| e.kind != EntryKind::Dir;
        dir(a).cmp(&dir(b)).then_with(|| {
            let (an, bn) = (a.name.to_string_lossy(), b.name.to_string_lossy());
            an.to_lowercase().cmp(&bn.to_lowercase()).then_with(|| an.cmp(&bn))
        })
    });
}

/// `name (copy)`, `name (copy 2)`… The suffix goes before the extension of files.
pub fn unique_name(name: &str, is_dir: bool, taken: &HashSet<String>) -> String {
    let dot = if is_dir { None } else { name.rfind('.').filter(|&i| i > 0) };
    let (base, ext) = match dot {
        Some(i) => name.split_at(i),
        None => (name, ""),
    };
    (1..)
        .map(|i| if i == 1 { format!("{base} (copy){ext}") } else { format!("{base} (copy {i}){ext}") })
        .find(|c| !taken.contains(c))
        .expect("an unused name always exists")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipOp {
    Copy,
    Move,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipItem {
    pub name: OsString,
    pub is_dir: bool,
}

/// Items marked with Copy or Move, waiting for Paste.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clip {
    pub op: ClipOp,
    pub dir: PathBuf,
    pub items: Vec<ClipItem>,
}

impl Clip {
    pub fn contains(&self, dir: &Path, name: &OsString) -> bool {
        self.dir == dir && self.items.iter().any(|i| &i.name == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteStep {
    pub src: PathBuf,
    pub dst: PathBuf,
    pub is_dir: bool,
}

impl PasteStep {
    pub fn dst_name(&self) -> OsString {
        self.dst.file_name().map(OsString::from).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PastePlan {
    pub steps: Vec<PasteStep>,
    /// Moves into the same folder and folders pasted into themselves.
    pub skipped: usize,
}

pub fn plan_paste(clip: &Clip, dest: &Path, dest_names: &HashSet<String>) -> PastePlan {
    let mut taken = dest_names.clone();
    let mut plan = PastePlan::default();
    for item in &clip.items {
        let src = clip.dir.join(&item.name);
        if clip.op == ClipOp::Move && clip.dir == dest {
            plan.skipped += 1;
            continue;
        }
        if item.is_dir && dest.starts_with(&src) {
            plan.skipped += 1;
            continue;
        }
        let name = item.name.to_string_lossy();
        let final_name: OsString = if taken.contains(name.as_ref()) {
            unique_name(&name, item.is_dir, &taken).into()
        } else {
            item.name.clone()
        };
        taken.insert(final_name.to_string_lossy().into_owned());
        plan.steps.push(PasteStep { dst: dest.join(&final_name), src, is_dir: item.is_dir });
    }
    plan
}

/// New location of `path` after `src` was moved to `dst`, if `path` was inside it.
pub fn rewrite_path(path: &Path, src: &Path, dst: &Path) -> Option<PathBuf> {
    let rest = path.strip_prefix(src).ok()?;
    Some(if rest.as_os_str().is_empty() { dst.to_path_buf() } else { dst.join(rest) })
}

/// Quotes for POSIX shells unless the string only has safe characters.
pub fn shell_quote(s: &str) -> Cow<'_, str> {
    let safe = !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if safe { Cow::Borrowed(s) } else { Cow::Owned(format!("'{}'", s.replace('\'', "'\\''"))) }
}

/// Command to download `items` (path, is_dir) to the current local directory.
pub fn scp_command(target: &ScpTarget, items: &[(PathBuf, bool)]) -> String {
    let host = if target.host.contains(':') && !target.host.starts_with('[') {
        format!("[{}]", target.host)
    } else {
        target.host.clone()
    };
    let mut parts = vec!["scp".to_string()];
    if items.iter().any(|(_, is_dir)| *is_dir) {
        parts.push("-r".into());
    }
    if target.port != 22 {
        parts.push(format!("-P {}", target.port));
    }
    for (path, _) in items {
        let arg = format!("{}@{}:{}", target.user, host, path.to_string_lossy());
        parts.push(shell_quote(&arg).into_owned());
    }
    parts.push(".".into());
    parts.join(" ")
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64 / 1024.0;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if v < 10.0 { format!("{v:.1} {}", UNITS[unit]) } else { format!("{v:.0} {}", UNITS[unit]) }
}

/// `Nov  6 14:03` this year, `Nov  6  2024` otherwise (like `ls -l`).
pub fn format_modified(time: Option<SystemTime>, now: DateTime<Local>) -> String {
    let Some(time) = time else { return String::new() };
    let t: DateTime<Local> = time.into();
    if t.year() == now.year() { t.format("%b %e %H:%M").to_string() } else { t.format("%b %e  %Y").to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn entry(name: &str, kind: EntryKind) -> Entry {
        Entry { name: name.into(), kind, size: 0, modified: None, symlink: false }
    }

    fn clip(op: ClipOp, dir: &str, items: &[(&str, bool)]) -> Clip {
        Clip {
            op,
            dir: dir.into(),
            items: items.iter().map(|(n, d)| ClipItem { name: n.into(), is_dir: *d }).collect(),
        }
    }

    #[test]
    fn sorts_folders_first_then_alphabetically() {
        let mut v = vec![
            entry("notes.txt", EntryKind::File),
            entry("logs", EntryKind::Dir),
            entry(".bashrc", EntryKind::File),
            entry("Apps", EntryKind::Dir),
        ];
        sort_entries(&mut v);
        let names: Vec<_> = v.iter().map(|e| e.name.to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["Apps", "logs", ".bashrc", "notes.txt"]);
    }

    #[test]
    fn unique_name_matches_prototype() {
        assert_eq!(unique_name("notes.txt", false, &set(&["notes.txt"])), "notes (copy).txt");
        assert_eq!(
            unique_name("notes.txt", false, &set(&["notes.txt", "notes (copy).txt"])),
            "notes (copy 2).txt"
        );
        assert_eq!(unique_name("apps.d", true, &set(&["apps.d"])), "apps.d (copy)");
        assert_eq!(unique_name(".bashrc", false, &set(&[".bashrc"])), ".bashrc (copy)");
    }

    #[test]
    fn plans_copy_with_collisions() {
        let c = clip(ClipOp::Copy, "/home/deploy", &[("notes.txt", false), ("apps", true)]);
        let plan = plan_paste(&c, Path::new("/home/deploy"), &set(&["notes.txt", "apps", "logs"]));
        assert_eq!(plan.skipped, 0);
        assert_eq!(plan.steps[0].dst, PathBuf::from("/home/deploy/notes (copy).txt"));
        assert_eq!(plan.steps[1].dst, PathBuf::from("/home/deploy/apps (copy)"));
    }

    #[test]
    fn two_items_with_the_same_new_name_do_not_collide() {
        let mut c = clip(ClipOp::Copy, "/a", &[("x", false)]);
        c.items.push(ClipItem { name: "x".into(), is_dir: false });
        let plan = plan_paste(&c, Path::new("/a"), &set(&["x"]));
        assert_eq!(plan.steps[0].dst, PathBuf::from("/a/x (copy)"));
        assert_eq!(plan.steps[1].dst, PathBuf::from("/a/x (copy 2)"));
    }

    #[test]
    fn skips_move_into_same_folder_and_folder_into_itself() {
        let same = clip(ClipOp::Move, "/home/deploy", &[("notes.txt", false)]);
        assert_eq!(plan_paste(&same, Path::new("/home/deploy"), &set(&["notes.txt"])).skipped, 1);

        let into_self = clip(ClipOp::Copy, "/home/deploy", &[("apps", true), ("notes.txt", false)]);
        let plan = plan_paste(&into_self, Path::new("/home/deploy/apps/api"), &set(&[]));
        assert_eq!(plan.skipped, 1);
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan_paste(&into_self, Path::new("/home/deploy/apps"), &set(&[])).skipped, 1);

        // A sibling whose name starts with the folder name is not "inside" it.
        let plan = plan_paste(&into_self, Path::new("/home/deploy/apps2"), &set(&[]));
        assert_eq!(plan.skipped, 0);
    }

    #[test]
    fn rewrites_paths_inside_moved_folder() {
        let (src, dst) = (Path::new("/home/deploy/apps"), Path::new("/srv/apps"));
        assert_eq!(rewrite_path(Path::new("/home/deploy/apps"), src, dst), Some("/srv/apps".into()));
        assert_eq!(rewrite_path(Path::new("/home/deploy/apps/api"), src, dst), Some("/srv/apps/api".into()));
        assert_eq!(rewrite_path(Path::new("/home/deploy/apps2"), src, dst), None);
    }

    #[test]
    fn quotes_like_the_prototype() {
        assert_eq!(shell_quote("deploy@edge1:/home/deploy/notes.txt"), "deploy@edge1:/home/deploy/notes.txt");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn builds_scp_command() {
        let t = ScpTarget { user: "deploy".into(), host: "edge1".into(), port: 22 };
        let items = vec![("/home/deploy/apps".into(), true), ("/home/deploy/notes.txt".into(), false)];
        assert_eq!(
            scp_command(&t, &items),
            "scp -r deploy@edge1:/home/deploy/apps deploy@edge1:/home/deploy/notes.txt ."
        );
        let t = ScpTarget { user: "deploy".into(), host: "fe80::1".into(), port: 2222 };
        assert_eq!(
            scp_command(&t, &[("/tmp/my file".into(), false)]),
            "scp -P 2222 'deploy@[fe80::1]:/tmp/my file' ."
        );
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(human_size(420), "420 B");
        assert_eq!(human_size(3789), "3.7 KB");
        assert_eq!(human_size(2_516_582), "2.4 MB");
        assert_eq!(human_size(880_640), "860 KB");
    }
}
