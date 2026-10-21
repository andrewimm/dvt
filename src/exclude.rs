//! Exclude patterns: entries that are neither read nor counted.
//!
//! - A pattern without `/` matches an entry's name at any depth
//!   (`node_modules`, `*.iso`).
//! - A pattern with `/` matches the entry's full path (`/proc`,
//!   `/home/*/.cache`); `*` crosses `/` here.
//! - A trailing `/` restricts a pattern to directories (`target/`).
//!
//! Matching is case-sensitive, like `du --exclude` and `.gitignore`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::glob;

struct Pattern {
    glob: Vec<u8>,
    dir_only: bool,
}

#[derive(Default)]
pub struct Excludes {
    names: Vec<Pattern>,
    paths: Vec<Pattern>,
}

impl Excludes {
    pub fn new<S: AsRef<str>>(patterns: impl IntoIterator<Item = S>) -> Self {
        let mut ex = Excludes::default();
        for p in patterns {
            let p = p.as_ref().trim();
            // A pattern that is only slashes is "/", the filesystem root,
            // which can't be an entry of anything.
            let (body, dir_only) = match p.strip_suffix('/') {
                Some(body) if !body.is_empty() => (body, true),
                _ => (p, false),
            };
            if body.is_empty() || body == "/" {
                continue;
            }
            let pat = Pattern {
                glob: body.as_bytes().to_vec(),
                dir_only,
            };
            if body.contains('/') {
                ex.paths.push(pat);
            } else {
                ex.names.push(pat);
            }
        }
        ex
    }

    pub fn len(&self) -> usize {
        self.names.len() + self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the entry `name` in directory `parent` is excluded.
    pub fn matches(&self, parent: &Path, name: &OsStr, is_dir: bool) -> bool {
        let applies = |p: &Pattern| is_dir || !p.dir_only;
        let name_bytes = name.as_encoded_bytes();
        if self
            .names
            .iter()
            .any(|p| applies(p) && glob::matches(&p.glob, name_bytes, false))
        {
            return true;
        }
        if self.paths.is_empty() {
            return false;
        }
        let path = parent.join(name);
        let path_bytes = path.as_os_str().as_encoded_bytes();
        self.paths
            .iter()
            .any(|p| applies(p) && glob::matches(&p.glob, path_bytes, false))
    }
}

/// `$XDG_CONFIG_HOME/dvt/exclude`, falling back to `~/.config/dvt/exclude`.
pub fn config_file() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(dir.join("dvt").join("exclude"))
}

/// The patterns in an exclude file: one per line, blank lines and `#`
/// comments ignored. A missing file has no patterns.
pub fn read_file(path: &Path) -> std::io::Result<Vec<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(String::from)
            .collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ex(patterns: &[&str], parent: &str, name: &str, is_dir: bool) -> bool {
        Excludes::new(patterns).matches(Path::new(parent), OsStr::new(name), is_dir)
    }

    #[test]
    fn names() {
        assert!(ex(&["node_modules"], "/a/b", "node_modules", true));
        assert!(ex(&["*.iso"], "/a", "ubuntu.iso", false));
        assert!(!ex(&["*.iso"], "/a", "ubuntu.ISO", false));
        assert!(!ex(&["node_modules"], "/a", "node_modules_old", true));
    }

    #[test]
    fn dir_only() {
        assert!(ex(&["target/"], "/a", "target", true));
        assert!(!ex(&["target/"], "/a", "target", false));
    }

    #[test]
    fn paths() {
        assert!(ex(&["/proc"], "/", "proc", true));
        assert!(!ex(&["/proc"], "/home", "proc", true));
        assert!(ex(&["/home/*/.cache"], "/home/andrew", ".cache", true));
        assert!(ex(&["*/target/debug"], "/x/proj/target", "debug", true));
    }

    #[test]
    fn ignores_empty_and_root() {
        let e = Excludes::new(["", "  ", "/", "//"]);
        assert_eq!(e.len(), 0);
    }

    #[test]
    fn file_parsing() {
        let dir = std::env::temp_dir().join(format!("dvt-exclude-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("exclude");
        std::fs::write(&file, "# caches\nnode_modules\n\n  target/  \n#*.iso\n").unwrap();
        assert_eq!(read_file(&file).unwrap(), vec!["node_modules", "target/"]);
        assert!(read_file(&dir.join("missing")).unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
