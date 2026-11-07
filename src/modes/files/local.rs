//! The real file system of the machine smtui runs on.

use std::io;
use std::path::Path;
use std::process::Command;

use std::collections::HashSet;
use std::ffi::OsString;

use super::fs::{Entry, EntryKind, Fs};
use super::ops::{self, ArchiveKind, Placement};

pub struct LocalFs;

/// `EXDEV` on Linux and macOS: rename across file systems.
const EXDEV: i32 = 18;

impl Fs for LocalFs {
    fn extract(&self, archive: &Path) -> io::Result<OsString> {
        Self::extract_impl(archive)
    }

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
    /// 1. List the members and refuse absolute paths or `..` (tar and unzip also guard
    ///    against these, this makes it explicit and independent of their version).
    /// 2. Extract into a new, empty hidden folder next to the archive, without restoring
    ///    the archive's owners, so nothing outside that folder can be touched.
    /// 3. Move the result into place under a free name; nothing existing is overwritten.
    fn extract_impl(archive: &Path) -> io::Result<OsString> {
        let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let kind = ops::archive_kind(&name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, format!("{name} is not a supported archive")))?;
        let dir = archive.parent().ok_or_else(|| io::Error::other("the archive has no parent folder"))?;

        let listing = match kind {
            ArchiveKind::Tar => output(Command::new("tar").arg("-tf").arg(archive)),
            ArchiveKind::Zip => output(Command::new("unzip").arg("-Z1").arg(archive)),
        }
        .map_err(|e| missing_tool(e, kind))?;
        let members: Vec<&str> = listing.lines().filter(|l| !l.is_empty()).collect();
        if members.is_empty() {
            return Err(io::Error::other("the archive is empty"));
        }
        if let Some(bad) = members.iter().find(|m| !ops::safe_member(m)) {
            return Err(io::Error::other(format!("refusing to extract: unsafe path in the archive ({bad})")));
        }

        let tmp = (0..100)
            .map(|i| dir.join(format!(".smtui-extract-{}-{i}", std::process::id())))
            .find(|p| std::fs::create_dir(p).is_ok())
            .ok_or_else(|| io::Error::other(format!("cannot create a temporary folder in {}", dir.display())))?;
        let result = (|| {
            match kind {
                ArchiveKind::Tar => run(Command::new("tar").arg("-xf").arg(archive).arg("-C").arg(&tmp).arg("--no-same-owner")),
                ArchiveKind::Zip => run(Command::new("unzip").arg("-q").arg("-n").arg(archive).arg("-d").arg(&tmp)),
            }
            .map_err(|e| missing_tool(e, kind))?;
            let top: Vec<String> =
                std::fs::read_dir(&tmp)?.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            if top.is_empty() {
                return Err(io::Error::other("the archive had nothing to extract"));
            }
            let taken: HashSet<String> =
                std::fs::read_dir(dir)?.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            match ops::place_extracted(&top, ops::archive_stem(&name), &taken) {
                Placement::Single(entry) => {
                    std::fs::rename(tmp.join(&entry), dir.join(&entry))?;
                    std::fs::remove_dir(&tmp)?;
                    Ok(OsString::from(entry))
                }
                Placement::Folder(folder) => {
                    std::fs::rename(&tmp, dir.join(&folder))?;
                    Ok(OsString::from(folder))
                }
            }
        })();
        if result.is_err() && tmp.exists() {
            let _ = std::fs::remove_dir_all(&tmp); // never leave a half-extracted folder behind
        }
        result
    }

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

/// Runs `cmd` and returns its standard output.
fn output(cmd: &mut Command) -> io::Result<String> {
    let out = cmd.output()?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(io::Error::other(if msg.is_empty() { format!("command failed: {}", out.status) } else { msg }))
    }
}

/// A clearer error when `tar` or `unzip` is not installed.
fn missing_tool(e: io::Error, kind: ArchiveKind) -> io::Error {
    if e.kind() == io::ErrorKind::NotFound {
        let tool = if kind == ArchiveKind::Zip { "unzip" } else { "tar" };
        io::Error::new(io::ErrorKind::NotFound, format!("{tool} is not installed on this server"))
    } else {
        e
    }
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

    fn tarball(root: &Path, name: &str, cwd: &Path, members: &[&str]) -> std::path::PathBuf {
        let dest = root.join(name);
        let ok = Command::new("tar").arg("-czf").arg(&dest).arg("-C").arg(cwd).args(members).status().unwrap();
        assert!(ok.success());
        dest
    }

    #[test]
    fn extracts_without_overwriting() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let src = root.join("src");
        std::fs::create_dir_all(src.join("site/css")).unwrap();
        std::fs::write(src.join("site/index.html"), "new").unwrap();
        std::fs::write(src.join("a.txt"), "a").unwrap();
        std::fs::write(src.join("b.txt"), "b").unwrap();
        let out = root.join("out");
        std::fs::create_dir(&out).unwrap();

        // One top-level folder: extracted as is.
        let single = tarball(&out, "site.tar.gz", &src, &["./site"]);
        assert_eq!(LocalFs.extract(&single).unwrap(), "site");
        assert_eq!(std::fs::read_to_string(out.join("site/index.html")).unwrap(), "new");

        // Again: `site` exists, so it goes to a new folder and the old one is untouched.
        std::fs::write(out.join("site/index.html"), "mine").unwrap();
        assert_eq!(LocalFs.extract(&single).unwrap(), "site (2)");
        assert_eq!(std::fs::read_to_string(out.join("site/index.html")).unwrap(), "mine");
        assert_eq!(std::fs::read_to_string(out.join("site (2)/site/index.html")).unwrap(), "new");

        // Several top-level entries: a folder named after the archive.
        let loose = tarball(&out, "loose.tgz", &src, &["a.txt", "b.txt"]);
        assert_eq!(LocalFs.extract(&loose).unwrap(), "loose");
        assert!(out.join("loose/a.txt").exists() && out.join("loose/b.txt").exists());

        // No temporary folders left behind.
        assert!(!std::fs::read_dir(&out).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".smtui-extract")));
    }

    #[test]
    fn refuses_unsafe_archives() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join("inner")).unwrap();
        std::fs::write(root.join("evil.txt"), "x").unwrap();
        // A member named `../evil.txt`, built from inside `inner`; `-P` keeps the `../`
        // (GNU tar and bsdtar both strip it otherwise).
        let archive = root.join("evil.tar.gz");
        let ok = Command::new("tar").arg("-czPf").arg(&archive).arg("-C").arg(root.join("inner")).arg("../evil.txt").status().unwrap();
        assert!(ok.success());
        let err = LocalFs.extract(&archive).unwrap_err();
        assert!(err.to_string().contains("unsafe path"), "{err}");
        assert!(!std::fs::read_dir(root).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".smtui-extract")));
    }
}
