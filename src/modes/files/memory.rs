//! In-memory file system with the prototype's sample data (`--demo` and tests).

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use super::fs::{Entry, EntryKind, Fs};

pub struct MemoryFs {
    dirs: Mutex<BTreeMap<PathBuf, Vec<Entry>>>,
}

fn not_found(p: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("{} not found", p.display()))
}

impl MemoryFs {
    pub fn sample() -> Self {
        let now = SystemTime::now();
        let ago = |days: u64| Some(now - Duration::from_secs(days * 86_400));
        let d = |n: &str, days| Entry { name: n.into(), kind: EntryKind::Dir, size: 0, modified: ago(days), symlink: false };
        let f = |n: &str, size, days| Entry { name: n.into(), kind: EntryKind::File, size, modified: ago(days), symlink: false };
        let mut dirs = BTreeMap::new();
        let mut put = |p: &str, v: Vec<Entry>| {
            dirs.insert(PathBuf::from(p), v);
        };
        put("/", vec![d("home", 55), d("etc", 4), d("var", 7), d("srv", 55)]);
        put("/home", vec![d("deploy", 0)]);
        put(
            "/home/deploy",
            vec![
                d("apps", 1),
                d("logs", 0),
                d("backups", 5),
                f(".bashrc", 3_789, 55),
                f("deploy.sh", 1_229, 9),
                f("notes.txt", 420, 3),
            ],
        );
        put("/home/deploy/apps", vec![d("api", 1), d("worker", 1), f("docker-compose.yml", 1_843, 1)]);
        put("/home/deploy/apps/api", vec![f("server.js", 12_400, 1)]);
        put("/home/deploy/apps/worker", vec![f("worker.js", 6_100, 1)]);
        put("/home/deploy/logs", vec![f("app.log", 2_516_582, 0), f("nginx.log", 880_640, 0), f("deploy.log", 14_336, 1)]);
        put("/home/deploy/backups", vec![]);
        put("/etc", vec![d("nginx", 7), d("ssh", 55), f("hosts", 221, 55)]);
        put("/etc/nginx", vec![]);
        put("/etc/ssh", vec![]);
        put("/var", vec![d("log", 0), d("lib", 7)]);
        put("/var/log", vec![]);
        put("/var/lib", vec![]);
        put("/srv", vec![]);
        Self { dirs: Mutex::new(dirs) }
    }

    fn transfer(&self, from: &Path, to: &Path, remove_source: bool) -> io::Result<()> {
        let mut dirs = self.dirs.lock().expect("memory fs lock");
        let (Some(src_parent), Some(src_name)) = (from.parent(), from.file_name()) else { return Err(not_found(from)) };
        let (Some(dst_parent), Some(dst_name)) = (to.parent(), to.file_name()) else { return Err(not_found(to)) };
        let entry = dirs
            .get(src_parent)
            .and_then(|v| v.iter().find(|e| e.name == src_name))
            .cloned()
            .ok_or_else(|| not_found(from))?;
        let dest_list = dirs.get(dst_parent).ok_or_else(|| not_found(dst_parent))?;
        if dest_list.iter().any(|e| e.name == dst_name) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} already exists", to.display())));
        }
        if entry.is_dir() {
            let keys: Vec<PathBuf> = dirs.keys().filter(|k| k.starts_with(from)).cloned().collect();
            for k in keys {
                let new_key = to.join(k.strip_prefix(from).expect("prefix checked"));
                let content = if remove_source { dirs.remove(&k) } else { dirs.get(&k).cloned() };
                dirs.insert(new_key, content.unwrap_or_default());
            }
        }
        dirs.get_mut(dst_parent).expect("checked").push(Entry { name: dst_name.into(), ..entry });
        if remove_source {
            dirs.get_mut(src_parent).expect("checked").retain(|e| e.name != src_name);
        }
        Ok(())
    }
}

impl Fs for MemoryFs {
    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        self.dirs.lock().expect("memory fs lock").get(dir).cloned().ok_or_else(|| not_found(dir))
    }

    fn move_to(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.transfer(from, to, true)
    }

    fn copy_to(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.transfer(from, to, false)
    }

    fn compress(&self, dir: &Path, names: &[std::ffi::OsString], dest: &Path) -> io::Result<()> {
        let mut dirs = self.dirs.lock().expect("memory fs lock");
        let (Some(parent), Some(dest_name)) = (dest.parent(), dest.file_name()) else { return Err(not_found(dest)) };
        let src = dirs.get(dir).ok_or_else(|| not_found(dir))?;
        // A rough stand-in for the compressed size: a third of the file sizes.
        let size = src.iter().filter(|e| names.contains(&e.name)).map(|e| e.size.max(4096)).sum::<u64>() / 3;
        let list = dirs.get_mut(parent).ok_or_else(|| not_found(parent))?;
        if list.iter().any(|e| e.name == dest_name) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} already exists", dest.display())));
        }
        list.push(Entry { name: dest_name.into(), kind: EntryKind::File, size, modified: Some(SystemTime::now()), symlink: false });
        Ok(())
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        let mut dirs = self.dirs.lock().expect("memory fs lock");
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else { return Err(not_found(path)) };
        let list = dirs.get_mut(parent).ok_or_else(|| not_found(path))?;
        let before = list.len();
        list.retain(|e| e.name != name);
        if list.len() == before {
            return Err(not_found(path));
        }
        dirs.retain(|k, _| !k.starts_with(path));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_folder_with_content() {
        let fs = MemoryFs::sample();
        fs.move_to(Path::new("/home/deploy/apps"), Path::new("/srv/apps")).unwrap();
        assert!(fs.list(Path::new("/srv/apps/api")).is_ok());
        assert!(fs.list(Path::new("/home/deploy/apps")).is_err());
        fs.copy_to(Path::new("/srv/apps"), Path::new("/srv/apps (copy)")).unwrap();
        assert_eq!(fs.list(Path::new("/srv/apps (copy)/api")).unwrap().len(), 1);
        assert!(fs.copy_to(Path::new("/srv/apps"), Path::new("/srv/apps")).is_err());
    }
}
