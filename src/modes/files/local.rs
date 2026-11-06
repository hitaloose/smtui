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

    fn remove(&self, path: &Path) -> io::Result<()> {
        Self::remove_impl(path)
    }

    fn compress(&self, dir: &Path, names: &[std::ffi::OsString], dest: &Path) -> io::Result<()> {
        ensure_absent(dest)?;
        // No shell; `./name` keeps names starting with `-` from being read as options.
        let mut cmd = Command::new("tar");
        cmd.arg("-czf").arg(dest).arg("-C").arg(dir);
        for name in names {
            cmd.arg(Path::new(".").join(name));
        }
        let result = run(&mut cmd);
        if result.is_err() {
            let _ = std::fs::remove_file(dest); // do not leave a partial archive behind
        }
        result
    }
}

impl LocalFs {
    fn remove_impl(path: &Path) -> io::Result<()> {
        // symlink_metadata does not follow links, so a link to a folder is removed as a file.
        if path.symlink_metadata()?.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) }
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

        LocalFs.remove(&root.join("moved")).unwrap();
        assert!(!root.join("moved").exists());
    }

    #[test]
    fn compresses_files_and_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("apps/api")).unwrap();
        std::fs::write(root.join("apps/api/main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("-odd name.txt"), "x").unwrap();
        let dest = root.join("out.tar.gz");
        LocalFs.compress(root, &["apps".into(), "-odd name.txt".into()], &dest).unwrap();
        let listing = Command::new("tar").arg("-tzf").arg(&dest).output().unwrap();
        let listing = String::from_utf8_lossy(&listing.stdout);
        assert!(listing.contains("apps/api/main.rs"), "{listing}");
        assert!(listing.contains("-odd name.txt"), "{listing}");
        assert!(LocalFs.compress(root, &["apps".into()], &dest).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn removing_a_symlink_keeps_its_target() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join("real")).unwrap();
        std::fs::write(root.join("real/keep.txt"), "x").unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        LocalFs.remove(&root.join("link")).unwrap();
        assert!(!root.join("link").exists());
        assert!(root.join("real/keep.txt").exists());
    }
}
