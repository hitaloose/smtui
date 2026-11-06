//! The real file system of the machine smtui runs on.

use std::io;
use std::path::Path;
use std::process::Command;

use super::fs::{Entry, EntryKind, Fs};

pub struct LocalFs;

/// `EXDEV` on Linux and macOS: rename across file systems.
const EXDEV: i32 = 18;

impl Fs for LocalFs {
    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        let mut out = Vec::new();
        for item in std::fs::read_dir(dir)? {
            let item = item?;
            let Ok(meta) = item.metadata() else { continue };
            let symlink = meta.file_type().is_symlink();
            // Follow links for the kind and size; a dangling link shows as a file.
            let target = if symlink { std::fs::metadata(item.path()).unwrap_or(meta.clone()) } else { meta.clone() };
            out.push(Entry {
                name: item.file_name(),
                kind: if target.is_dir() { EntryKind::Dir } else { EntryKind::File },
                size: target.len(),
                modified: meta.modified().ok(),
                symlink,
            });
        }
        Ok(out)
    }

    fn move_to(&self, from: &Path, to: &Path) -> io::Result<()> {
        ensure_absent(to)?;
        match std::fs::rename(from, to) {
            Err(e) if e.raw_os_error() == Some(EXDEV) => run(Command::new("mv").arg("--").arg(from).arg(to)),
            other => other,
        }
    }

    fn copy_to(&self, from: &Path, to: &Path) -> io::Result<()> {
        ensure_absent(to)?;
        // No shell involved: paths are passed as plain arguments.
        run(Command::new("cp").arg("-Rp").arg("--").arg(from).arg(to))
    }
}

fn ensure_absent(path: &Path) -> io::Result<()> {
    if path.symlink_metadata().is_ok() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} already exists", path.display())));
    }
    Ok(())
}

fn run(cmd: &mut Command) -> io::Result<()> {
    let out = cmd.output()?;
    if out.status.success() {
        Ok(())
    } else {
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(io::Error::other(if msg.is_empty() { format!("command failed: {}", out.status) } else { msg }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_and_moves_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("apps/api")).unwrap();
        std::fs::write(root.join("apps/api/main.rs"), "fn main() {}").unwrap();
        std::fs::create_dir(root.join("dest")).unwrap();

        LocalFs.copy_to(&root.join("apps"), &root.join("dest/apps")).unwrap();
        assert!(root.join("dest/apps/api/main.rs").exists());
        assert!(root.join("apps/api/main.rs").exists());

        assert!(LocalFs.copy_to(&root.join("apps"), &root.join("dest/apps")).is_err());

        LocalFs.move_to(&root.join("apps"), &root.join("moved")).unwrap();
        assert!(root.join("moved/api/main.rs").exists());
        assert!(!root.join("apps").exists());

        let names: Vec<_> = LocalFs.list(root).unwrap().into_iter().map(|e| e.name).collect();
        assert!(names.contains(&"moved".into()) && names.contains(&"dest".into()));
    }
}
