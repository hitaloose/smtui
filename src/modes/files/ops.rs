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

/// `base.tar.gz`, then `base (2).tar.gz`, `base (3).tar.gz`… until unused.
pub fn archive_name(base: &str, taken: &HashSet<String>) -> String {
    (1..)
        .map(|i| if i == 1 { format!("{base}.tar.gz") } else { format!("{base} ({i}).tar.gz") })
        .find(|c| !taken.contains(c))
        .expect("an unused name always exists")
}

/// `New file.txt`, then `New file (2).txt`… until unused.
pub fn numbered_file_name(stem: &str, ext: &str, taken: &HashSet<String>) -> String {
    (1..)
        .map(|i| if i == 1 { format!("{stem}{ext}") } else { format!("{stem} ({i}){ext}") })
        .find(|c| !taken.contains(c))
        .expect("an unused name always exists")
}

/// Why `name` cannot be a new file or folder in the open one, if it cannot.
pub fn new_name_problem(name: &str) -> Option<&'static str> {
    if name.trim().is_empty() {
        Some("Type a name.")
    } else if name.contains('/') {
        Some("A name cannot contain /.")
    } else if name == "." || name == ".." {
        Some("Not a valid name.")
    } else if name.len() > 255 {
        Some("Too long.")
    } else {
        None
    }
}

/// Archive formats smtui can extract. Tarballs go through `tar` (which detects the
/// compression itself), zip files through `unzip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Tar,
    Zip,
}

/// Archive extensions, longest first, so `x.tar.gz` is not read as `x.tar` + `.gz`.
const ARCHIVE_EXTS: &[(&str, ArchiveKind)] = &[
    (".tar.gz", ArchiveKind::Tar),
    (".tar.bz2", ArchiveKind::Tar),
    (".tar.xz", ArchiveKind::Tar),
    (".tar.zst", ArchiveKind::Tar),
    (".tgz", ArchiveKind::Tar),
    (".tbz2", ArchiveKind::Tar),
    (".tbz", ArchiveKind::Tar),
    (".txz", ArchiveKind::Tar),
    (".tzst", ArchiveKind::Tar),
    (".tar", ArchiveKind::Tar),
    (".zip", ArchiveKind::Zip),
];

/// The archive format of a file name, if smtui can extract it.
pub fn archive_kind(name: &str) -> Option<ArchiveKind> {
    let lower = name.to_lowercase();
    ARCHIVE_EXTS.iter().find(|(ext, _)| lower.len() > ext.len() && lower.ends_with(ext)).map(|(_, k)| *k)
}

/// The name without its archive extension: `backup.tar.gz` → `backup`.
pub fn archive_stem(name: &str) -> &str {
    let lower = name.to_lowercase();
    match ARCHIVE_EXTS.iter().find(|(ext, _)| lower.len() > ext.len() && lower.ends_with(ext)) {
        Some((ext, _)) => &name[..name.len() - ext.len()],
        None => name,
    }
}

/// A member path is safe when it stays inside the destination: not absolute, no `..`.
pub fn safe_member(member: &str) -> bool {
    let member = member.trim_end_matches('/');
    !member.starts_with('/') && !member.split('/').any(|part| part == "..")
}

/// `base`, then `base (2)`, `base (3)`… until unused.
pub fn numbered_name(base: &str, taken: &HashSet<String>) -> String {
    (1..)
        .map(|i| if i == 1 { base.to_string() } else { format!("{base} ({i})") })
        .find(|c| !taken.contains(c))
        .expect("an unused name always exists")
}

/// Where extracted content goes, so an archive never scatters files or overwrites anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// The archive holds a single top-level entry whose name is free: move it out as is.
    Single(String),
    /// Everything goes into a new folder named after the archive.
    Folder(String),
}

/// `top`: the top-level entries the archive produced; `taken`: names already in the folder.
pub fn place_extracted(top: &[String], stem: &str, taken: &HashSet<String>) -> Placement {
    match top {
        [one] if !taken.contains(one) => Placement::Single(one.clone()),
        _ => Placement::Folder(numbered_name(if stem.is_empty() { "extracted" } else { stem }, taken)),
    }
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

fn remote_arg(target: &ScpTarget, path: &str) -> String {
    let host = if target.host.contains(':') && !target.host.starts_with('[') {
        format!("[{}]", target.host)
    } else {
        target.host.clone()
    };
    shell_quote(&format!("{}@{}:{}", target.user, host, path)).into_owned()
}

/// Placeholder used by "Copy upload command" for the user to replace.
pub const LOCAL_PATH_PLACEHOLDER: &str = "<LOCAL_PATH>";

/// Quotes a path on the user's machine, keeping a leading `~/` unquoted so it still expands.
fn local_arg(path: &str) -> String {
    if path == LOCAL_PATH_PLACEHOLDER {
        return path.to_string();
    }
    match path.strip_prefix("~/") {
        Some(rest) => format!("~/{}", shell_quote(rest)),
        None => shell_quote(path).into_owned(),
    }
}

/// Command to run on the user's machine that uploads `locals` into the folder `dest`.
/// `-r` is always used: from the server we cannot tell whether a local path is a folder.
pub fn scp_upload_command(target: &ScpTarget, locals: &[String], dest: &Path) -> String {
    let mut parts = vec!["scp".to_string(), "-r".into()];
    if target.port != 22 {
        parts.push(format!("-P {}", target.port));
    }
    parts.extend(locals.iter().map(|l| local_arg(l)));
    let dest = dest.to_string_lossy();
    let dest = if dest.ends_with('/') { dest.into_owned() } else { format!("{dest}/") };
    parts.push(remote_arg(target, &dest));
    parts.join(" ")
}

/// Paths pasted by a terminal when files are dropped on it, or `None` if the text is not
/// only paths. Handles `My\ File` escapes, single/double quotes, one path per line and
/// `file://` URIs.
pub fn parse_dropped_paths(text: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_token = false;
    let mut chars = text.trim().chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    cur.push(next);
                    in_token = true;
                }
            }
            '\'' | '"' => {
                in_token = true;
                for q in chars.by_ref() {
                    if q == c {
                        break;
                    }
                    cur.push(q);
                }
            }
            c if c.is_whitespace() => {
                if in_token {
                    tokens.push(std::mem::take(&mut cur));
                    in_token = false;
                }
            }
            c => {
                cur.push(c);
                in_token = true;
            }
        }
    }
    if in_token {
        tokens.push(cur);
    }
    let paths: Vec<String> = tokens
        .into_iter()
        .map(|t| match t.strip_prefix("file://") {
            // file:///Users/me/a%20b.txt (an optional host before the path is dropped)
            Some(rest) => percent_decode(&rest[rest.find('/').unwrap_or(0)..]),
            None => t,
        })
        .collect();
    let looks_local = |p: &String| p.starts_with('/') || p.starts_with("~/");
    (!paths.is_empty() && paths.iter().all(looks_local)).then_some(paths)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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

    #[test]
    fn new_folder_names() {
        let taken: HashSet<String> = ["New folder".to_string(), "New folder (2)".to_string()].into();
        assert_eq!(numbered_name("New folder", &taken), "New folder (3)");
        assert_eq!(numbered_name("site", &taken), "site");
        let files: HashSet<String> = ["New file.txt".to_string()].into();
        assert_eq!(numbered_file_name("New file", ".txt", &files), "New file (2).txt");
        assert_eq!(new_name_problem("site"), None);
        assert_eq!(new_name_problem("my notes.txt"), None);
        assert!(new_name_problem("  ").is_some());
        assert!(new_name_problem("a/b").is_some());
        assert!(new_name_problem("..").is_some());
    }

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
    fn archive_names_never_collide() {
        assert_eq!(archive_name("apps", &set(&[])), "apps.tar.gz");
        assert_eq!(archive_name("apps", &set(&["apps.tar.gz"])), "apps (2).tar.gz");
        assert_eq!(archive_name("apps", &set(&["apps.tar.gz", "apps (2).tar.gz"])), "apps (3).tar.gz");
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
    fn parses_dropped_paths() {
        // Terminal.app / iTerm2: backslash-escaped, space separated
        assert_eq!(
            parse_dropped_paths("/Users/me/My\\ File.txt /Users/me/b.txt "),
            Some(vec!["/Users/me/My File.txt".into(), "/Users/me/b.txt".into()])
        );
        // Quoted, and one per line
        assert_eq!(
            parse_dropped_paths("'/tmp/a b'\n\"/tmp/it's\""),
            Some(vec!["/tmp/a b".into(), "/tmp/it's".into()])
        );
        // file:// URIs with percent encoding
        assert_eq!(parse_dropped_paths("file:///home/me/a%20b.txt"), Some(vec!["/home/me/a b.txt".into()]));
        assert_eq!(parse_dropped_paths("~/notes.md"), Some(vec!["~/notes.md".into()]));
        // Ordinary pasted text is not a drop
        assert_eq!(parse_dropped_paths("hello world"), None);
        assert_eq!(parse_dropped_paths("/tmp/a and more"), None);
        assert_eq!(parse_dropped_paths("   "), None);
    }

    #[test]
    fn builds_upload_command() {
        let t = ScpTarget { user: "deploy".into(), host: "edge1".into(), port: 22 };
        assert_eq!(
            scp_upload_command(&t, &["/Users/me/My File.txt".into(), "~/a b".into()], Path::new("/home/deploy/apps")),
            "scp -r '/Users/me/My File.txt' ~/'a b' deploy@edge1:/home/deploy/apps/"
        );
        let t = ScpTarget { port: 2222, ..t };
        assert_eq!(
            scp_upload_command(&t, &[LOCAL_PATH_PLACEHOLDER.into()], Path::new("/srv/my app")),
            "scp -r -P 2222 <LOCAL_PATH> 'deploy@edge1:/srv/my app/'"
        );
        assert_eq!(
            scp_upload_command(&t, &[LOCAL_PATH_PLACEHOLDER.into()], Path::new("/")),
            "scp -r -P 2222 <LOCAL_PATH> deploy@edge1:/"
        );
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(human_size(420), "420 B");
        assert_eq!(human_size(3789), "3.7 KB");
        assert_eq!(human_size(2_516_582), "2.4 MB");
        assert_eq!(human_size(880_640), "860 KB");
    }

    #[test]
    fn archive_kinds() {
        assert_eq!(archive_kind("backup.tar.gz"), Some(ArchiveKind::Tar));
        assert_eq!(archive_kind("BACKUP.TGZ"), Some(ArchiveKind::Tar));
        assert_eq!(archive_kind("x.tar.zst"), Some(ArchiveKind::Tar));
        assert_eq!(archive_kind("site.zip"), Some(ArchiveKind::Zip));
        assert_eq!(archive_kind(".tar.gz"), None);
        assert_eq!(archive_kind("notes.gz"), None);
        assert_eq!(archive_kind("readme.txt"), None);
        assert_eq!(archive_stem("backup (2).tar.gz"), "backup (2)");
        assert_eq!(archive_stem("Site.ZIP"), "Site");
    }

    #[test]
    fn unsafe_members() {
        for ok in ["a.txt", "./a/b", "dir/", "a..b/c", "./"] {
            assert!(safe_member(ok), "{ok}");
        }
        for bad in ["/etc/passwd", "../x", "a/../../x", "a/.."] {
            assert!(!safe_member(bad), "{bad}");
        }
    }

    #[test]
    fn extraction_placement() {
        let taken: HashSet<String> = ["backup".into(), "app".into()].into();
        assert_eq!(place_extracted(&["site".into()], "backup", &taken), Placement::Single("site".into()));
        assert_eq!(place_extracted(&["app".into()], "backup", &taken), Placement::Folder("backup (2)".into()));
        assert_eq!(place_extracted(&["a".into(), "b".into()], "logs", &taken), Placement::Folder("logs".into()));
    }
}
