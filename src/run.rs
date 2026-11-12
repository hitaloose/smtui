//! "Run…" on files: what the dialog needs (suggestions, warnings, the final command line)
//! and the per-type history of what the user ran. The command itself runs in a new
//! terminal tab, typed into an interactive shell, so nothing ever runs hidden.

use std::path::{Path, PathBuf};

use crate::modes::files::ops::shell_quote;

/// Keep this many history lines on disk.
const HISTORY_LIMIT: usize = 200;

/// smtui's state folder (`~/.local/state/smtui`), also used for the log.
pub fn state_dir() -> Option<PathBuf> {
    Some(dirs::state_dir().or_else(|| dirs::home_dir().map(|h| h.join(".local").join("state")))?.join("smtui"))
}

/// What the Files mode asks the app to show in the Run dialog.
#[derive(Debug, Clone)]
pub struct RunPrompt {
    /// Folder the terminal tab starts in.
    pub cwd: PathBuf,
    /// Absolute paths of the selected entries (never empty).
    pub paths: Vec<PathBuf>,
    /// A single selected file with the executable bit, so it can run with no command.
    pub executable: bool,
    /// History key: the extension, or `<dir>` / `<exec>` / `<file>`.
    pub key: String,
    pub suggestions: Vec<String>,
    pub notes: Vec<Note>,
    pub as_root: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub text: String,
    pub danger: bool,
}

impl Note {
    fn info(text: impl Into<String>) -> Self {
        Self { text: text.into(), danger: false }
    }
    fn danger(text: impl Into<String>) -> Self {
        Self { text: text.into(), danger: true }
    }
}

/// Facts about one selected entry, read from the real file system (missing in `--demo`).
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub executable: bool,
    pub world_writable: bool,
    /// Owner uid when it is neither the current user nor root.
    pub other_owner: Option<u32>,
    pub symlink_target: Option<PathBuf>,
}

pub fn inspect(path: &Path) -> Facts {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let mut facts = Facts::default();
    if let Ok(link) = std::fs::symlink_metadata(path)
        && link.file_type().is_symlink()
    {
        facts.symlink_target = std::fs::canonicalize(path).ok().or_else(|| std::fs::read_link(path).ok());
    }
    if let Ok(meta) = std::fs::metadata(path) {
        let mode = meta.permissions().mode();
        facts.executable = meta.is_file() && mode & 0o111 != 0;
        facts.world_writable = mode & 0o002 != 0 && mode & 0o1000 == 0; // sticky folders like /tmp are fine
        let me = current_uid();
        if meta.uid() != me && meta.uid() != 0 {
            facts.other_owner = Some(meta.uid());
        }
    }
    facts
}

pub fn current_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Names that cannot be typed safely into a shell: control characters (an ESC would be
/// read as a key by the line editor) or bytes that are not UTF-8.
pub fn unsafe_name(path: &Path) -> bool {
    match path.to_str() {
        Some(s) => s.chars().any(char::is_control),
        None => true,
    }
}

/// Builds the prompt for the selected entries: `(path, is_dir, facts)`.
pub fn prompt(cwd: PathBuf, entries: &[(PathBuf, bool, Facts)], as_root: bool) -> RunPrompt {
    let single = entries.len() == 1;
    let (first, first_dir, first_facts) = &entries[0];
    let name = first.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let executable = single && first_facts.executable;
    let key = history_key(&name, *first_dir, first_facts.executable);
    let suggestions = suggestions(&name, *first_dir, executable);

    let mut notes = Vec::new();
    if single && let Some(target) = &first_facts.symlink_target {
        notes.push(Note::info(format!("Symlink to {}", target.display())));
    }
    if executable && let Some(uid) = first_facts.other_owner {
        notes.push(Note::danger(format!("Owned by another user (uid {uid}).")));
    }
    let writable: Vec<&PathBuf> = entries.iter().filter(|(_, d, f)| !d && f.world_writable).map(|(p, ..)| p).collect();
    match writable.as_slice() {
        [] => {}
        [_] if single => notes.push(Note::danger("Anyone on this server can modify this file.")),
        many => notes.push(Note::danger(format!("{} files can be modified by anyone on this server.", many.len()))),
    }
    RunPrompt { cwd, paths: entries.iter().map(|(p, ..)| p.clone()).collect(), executable, key, suggestions, notes, as_root }
}

/// Lower-case extension, keeping double ones like `tar.gz`.
pub fn history_key(name: &str, is_dir: bool, executable: bool) -> String {
    if is_dir {
        return "<dir>".into();
    }
    let lower = name.to_lowercase();
    let trimmed = lower.trim_start_matches('.');
    for double in ["tar.gz", "tar.bz2", "tar.xz", "tar.zst"] {
        if trimmed.ends_with(&format!(".{double}")) {
            return double.into();
        }
    }
    match trimmed.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() => ext.into(),
        _ if executable => "<exec>".into(),
        _ => "<file>".into(),
    }
}

/// Commands offered for a file type. They are only filled in, never run on their own.
pub fn suggestions(name: &str, is_dir: bool, executable: bool) -> Vec<String> {
    let key = history_key(name, is_dir, executable);
    let list: &[&str] = match key.as_str() {
        "<dir>" => &["ls -la", "du -sh", "tree -L 2"],
        "log" | "out" => &["less +F", "tail -f", "less"],
        "sh" | "bash" => &["bash", "less"],
        "py" => &["python3", "less"],
        "js" | "mjs" | "cjs" => &["node", "less"],
        "rb" => &["ruby", "less"],
        "json" => &["jq .", "less"],
        "yml" | "yaml" | "toml" | "conf" | "ini" | "env" => &["vim", "less", "cat"],
        "tar.gz" | "tgz" | "tar.bz2" | "tar.xz" | "tar.zst" | "tar" => &["tar -tf", "tar -xf"],
        "zip" => &["unzip -l", "unzip"],
        "gz" => &["zless", "zcat"],
        "sql" => &["less", "psql -f", "mysql <"],
        _ => &["vim", "less", "cat", "tail -f"],
    };
    let mut out: Vec<String> = Vec::new();
    if executable {
        out.push(String::new()); // run the file itself
    }
    out.extend(list.iter().map(|s| s.to_string()));
    out
}

/// `prefix 'path' … args`, with every path quoted.
pub fn build_command(prefix: &str, paths: &[PathBuf], args: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !prefix.trim().is_empty() {
        parts.push(prefix.trim().to_string());
    }
    parts.extend(paths.iter().map(|p| shell_quote(&p.to_string_lossy()).into_owned()));
    if !args.trim().is_empty() {
        parts.push(args.trim().to_string());
    }
    parts.join(" ")
}

/// Why the command cannot run yet, if it cannot.
pub fn blocked_reason(prompt: &RunPrompt, prefix: &str) -> Option<&'static str> {
    if !prefix.trim().is_empty() {
        return None;
    }
    if prompt.paths.len() > 1 {
        Some("Type a command to run with these files.")
    } else if !prompt.executable {
        Some("This file is not executable: type a command, e.g. vim or bash.")
    } else {
        None
    }
}

/// Commands that destroy or overwrite data get a second confirmation. This is a speed
/// bump against slips, not a sandbox: the user can run anything in a terminal anyway.
pub fn looks_destructive(cmd: &str) -> bool {
    // `2>&1` only duplicates a descriptor; drop it before splitting on `&`.
    let cmd = cmd.replace(">&", " ");
    let words: Vec<&str> = cmd.split(|c: char| c.is_whitespace() || ";|&()".contains(c)).filter(|w| !w.is_empty()).collect();
    let recursive = words.iter().any(|w| *w == "-R" || *w == "--recursive" || (w.starts_with('-') && !w.starts_with("--") && w.contains('R')));
    for (i, w) in words.iter().enumerate() {
        let base = w.rsplit('/').next().unwrap_or(w);
        match base {
            "rm" | "rmdir" | "dd" | "shred" | "truncate" | "wipefs" | "unlink" => return true,
            b if b.starts_with("mkfs") => return true,
            "chmod" | "chown" | "chgrp" if recursive => return true,
            _ => {}
        }
        // `>` overwrites its target; `>>`, `2>&1` and `> /dev/null` do not destroy anything.
        if let Some(pos) = w.find('>') {
            let rest = &w[pos + 1..];
            let target = if rest.is_empty() { words.get(i + 1).copied().unwrap_or("") } else { rest };
            if !rest.starts_with('>') && !rest.starts_with('&') && target != "/dev/null" {
                return true;
            }
        }
    }
    false
}

/// Past runs, one `key<TAB>command<TAB>args` line each, newest last.
#[derive(Debug, Default)]
pub struct History {
    path: Option<PathBuf>,
    entries: Vec<(String, String, String)>,
}

impl History {
    pub fn load_default() -> Self {
        let path = state_dir().map(|d| d.join("run_history.tsv"));
        let text = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        Self { path, entries: parse_history(&text) }
    }

    /// (command, args) pairs for `key`, newest first.
    pub fn for_key(&self, key: &str) -> Vec<(String, String)> {
        self.entries.iter().rev().filter(|(k, ..)| k == key).map(|(_, c, a)| (c.clone(), a.clone())).collect()
    }

    pub fn record(&mut self, key: &str, command: &str, args: &str) {
        let clean = |s: &str| s.chars().filter(|c| !c.is_control()).collect::<String>().trim().to_string();
        let entry = (clean(key), clean(command), clean(args));
        self.entries.retain(|e| *e != entry);
        self.entries.push(entry);
        let excess = self.entries.len().saturating_sub(HISTORY_LIMIT);
        self.entries.drain(..excess);
        if let Err(e) = self.save() {
            tracing::warn!(error = %e, "could not save the run history");
        }
    }

    fn save(&self) -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let Some(path) = &self.path else { return Ok(()) };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Args may hold tokens or passwords: only the user can read the file.
        let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
        for (k, c, a) in &self.entries {
            writeln!(file, "{k}\t{c}\t{a}")?;
        }
        Ok(())
    }
}

fn parse_history(text: &str) -> Vec<(String, String, String)> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let key = parts.next()?.to_string();
            let cmd = parts.next()?.to_string();
            let args = parts.next().unwrap_or("").to_string();
            (!key.is_empty()).then_some((key, cmd, args))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_by_type() {
        assert_eq!(history_key("app.LOG", false, false), "log");
        assert_eq!(history_key("backup.tar.gz", false, false), "tar.gz");
        assert_eq!(history_key(".bashrc", false, false), "<file>");
        assert_eq!(history_key("deploy", false, true), "<exec>");
        assert_eq!(history_key("deploy.sh", false, true), "sh");
        assert_eq!(history_key("src", true, false), "<dir>");
    }

    #[test]
    fn executable_files_can_run_alone() {
        let s = suggestions("deploy", false, true);
        assert_eq!(s[0], "");
        assert!(!suggestions("notes.txt", false, false).contains(&String::new()));
    }

    #[test]
    fn command_quotes_every_path() {
        let paths = vec![PathBuf::from("/srv/my notes.txt"), PathBuf::from("/srv/it's;rm -rf ~")];
        assert_eq!(
            build_command(" vim ", &paths, " -O "),
            "vim '/srv/my notes.txt' '/srv/it'\\''s;rm -rf ~' -O"
        );
        assert_eq!(build_command("", &[PathBuf::from("/srv/run.sh")], ""), "/srv/run.sh");
    }

    #[test]
    fn destructive_commands() {
        for cmd in ["rm -rf '/srv/x'", "sudo rm '/a'", "/bin/rm a", "dd if=/dev/zero of=x", "mkfs.ext4 /dev/sdb", "chmod -R 777 /srv", "cat a > b", "echo x>b", "a; shred b"] {
            assert!(looks_destructive(cmd), "{cmd}");
        }
        for cmd in ["vim '/srv/rm.txt'", "less +F app.log", "cat a >> b", "make 2>&1", "x 2> /dev/null", "chmod 644 a", "tail -f '/var/log/x'", "ls -la"] {
            assert!(!looks_destructive(cmd), "{cmd}");
        }
    }

    #[test]
    fn blocked_without_command() {
        let p = |n: usize, exec: bool| RunPrompt {
            cwd: "/".into(),
            paths: vec![PathBuf::from("/a"); n],
            executable: exec,
            key: String::new(),
            suggestions: vec![],
            notes: vec![],
            as_root: false,
        };
        assert!(blocked_reason(&p(1, true), "").is_none());
        assert!(blocked_reason(&p(1, false), " ").is_some());
        assert!(blocked_reason(&p(2, false), "").is_some());
        assert!(blocked_reason(&p(2, false), "cat").is_none());
    }

    #[test]
    fn unsafe_names() {
        assert!(unsafe_name(Path::new("/srv/evil\x1b[31m.txt")));
        assert!(unsafe_name(Path::new("/srv/line\nbreak")));
        assert!(!unsafe_name(Path::new("/srv/ok name (2).txt")));
    }

    #[test]
    fn history_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.tsv");
        let mut h = History { path: Some(path.clone()), entries: Vec::new() };
        h.record("log", "less +F", "");
        h.record("log", "tail -f", "-n 100");
        h.record("log", "less +F", ""); // moves to the front
        h.record("py", "python3", "--help");
        let loaded = History { path: None, entries: parse_history(&std::fs::read_to_string(&path).unwrap()) };
        assert_eq!(
            loaded.for_key("log"),
            vec![("less +F".to_string(), String::new()), ("tail -f".to_string(), "-n 100".to_string())]
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
