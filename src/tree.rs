//! Arena-backed file tree. The UI thread is the single owner and writer; scanner
//! workers only ever send messages (see `scan`), so no locking is needed here.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU32;

use crate::scan::{Entry, Msg};

pub type NodeId = u32;

pub const ROOT: NodeId = 0;
const NONE: NodeId = NodeId::MAX;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    #[default]
    Other,
}

pub mod flags {
    /// The directory's contents have been read.
    pub const SCANNED: u8 = 1 << 0;
    /// Reading or stat-ing this entry failed (usually permission denied).
    pub const ERROR: u8 = 1 << 1;
    /// A hardlink whose inode was already counted elsewhere; its size is zeroed.
    pub const HARDLINK_DUP: u8 = 1 << 2;
    /// A mount point on another filesystem that was not descended into (`-x`).
    pub const OTHER_FS: u8 = 1 << 3;
}

/// 32 bytes per entry; names live in a shared byte arena.
#[derive(Clone, Copy, Debug)]
pub struct Node {
    /// Disk usage in bytes (allocated blocks), including all descendants.
    pub disk: u64,
    name_off: u32,
    pub parent: NodeId,
    first_child: NodeId,
    n_children: u32,
    /// Number of descendant entries.
    pub items: u32,
    name_len: u16,
    pub kind: Kind,
    pub flags: u8,
}

impl Default for Node {
    fn default() -> Self {
        Node {
            disk: 0,
            name_off: 0,
            parent: NONE,
            first_child: 0,
            n_children: 0,
            items: 0,
            name_len: 0,
            kind: Kind::Other,
            flags: 0,
        }
    }
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.kind == Kind::Dir
    }

    pub fn has(&self, flag: u8) -> bool {
        self.flags & flag != 0
    }
}

pub struct Tree {
    nodes: Vec<Node>,
    names: Vec<u8>,
    root_path: PathBuf,
    /// Shared with scanners, which allocate node ids for the entries they find.
    pub ids: Arc<AtomicU32>,
    pub errors: u32,
}

impl Tree {
    pub fn new(root_path: PathBuf) -> Self {
        let root = Node {
            kind: Kind::Dir,
            ..Node::default()
        };
        Tree {
            nodes: vec![root],
            names: Vec::new(),
            root_path,
            ids: Arc::new(AtomicU32::new(ROOT + 1)),
            errors: 0,
        }
    }

    pub fn get(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        let p = self.get(id).parent;
        (p != NONE).then_some(p)
    }

    pub fn children(&self, id: NodeId) -> impl ExactSizeIterator<Item = NodeId> + use<> {
        let n = self.get(id);
        n.first_child..n.first_child + n.n_children
    }

    /// Children sorted largest first, ties broken by name.
    pub fn sorted_children(&self, id: NodeId) -> Vec<NodeId> {
        let mut v: Vec<NodeId> = self.children(id).collect();
        v.sort_unstable_by(|&a, &b| {
            self.get(b)
                .disk
                .cmp(&self.get(a).disk)
                .then_with(|| self.name(a).cmp(self.name(b)))
        });
        v
    }

    pub fn name(&self, id: NodeId) -> &OsStr {
        let n = self.get(id);
        let bytes = &self.names[n.name_off as usize..n.name_off as usize + n.name_len as usize];
        // SAFETY: the bytes were produced by `OsStr::as_encoded_bytes` in `push_name`.
        unsafe { OsStr::from_encoded_bytes_unchecked(bytes) }
    }

    pub fn path(&self, id: NodeId) -> PathBuf {
        let mut parts = Vec::new();
        let mut cur = id;
        while let Some(p) = self.parent(cur) {
            parts.push(cur);
            cur = p;
        }
        let mut path = self.root_path.clone();
        for &part in parts.iter().rev() {
            path.push(self.name(part));
        }
        path
    }

    pub fn apply(&mut self, msg: Msg) {
        match msg {
            Msg::Root { id, kind, disk, error } => {
                let n = &mut self.nodes[id as usize];
                n.kind = kind;
                if error {
                    n.flags |= flags::ERROR;
                    self.errors += 1;
                }
                self.add_up(id, disk as i64, 0);
            }
            Msg::Batch { dir, first, entries, error } => self.apply_batch(dir, first, entries, error),
            Msg::Done => {}
        }
    }

    fn apply_batch(&mut self, dir: NodeId, first: NodeId, entries: Vec<Entry>, error: bool) {
        let count = entries.len() as u32;
        if count > 0 {
            let needed = (first + count) as usize;
            if self.nodes.len() < needed {
                self.nodes.resize(needed, Node::default());
            }
        }
        let mut total = 0u64;
        for (i, e) in entries.into_iter().enumerate() {
            let (name_off, name_len) = self.push_name(&e.name);
            if e.flags & flags::ERROR != 0 {
                self.errors += 1;
            }
            total += e.disk;
            self.nodes[first as usize + i] = Node {
                disk: e.disk,
                name_off,
                parent: dir,
                first_child: 0,
                n_children: 0,
                items: 0,
                name_len,
                kind: e.kind,
                flags: e.flags,
            };
        }
        let d = &mut self.nodes[dir as usize];
        d.first_child = first;
        d.n_children = count;
        d.flags |= flags::SCANNED;
        if error {
            d.flags |= flags::ERROR;
            self.errors += 1;
        }
        self.add_up(dir, total as i64, count as i64);
    }

    /// Adds to the totals of `id` and all of its ancestors.
    fn add_up(&mut self, id: NodeId, disk: i64, items: i64) {
        let mut cur = id;
        while cur != NONE {
            let n = &mut self.nodes[cur as usize];
            n.disk = n.disk.saturating_add_signed(disk);
            n.items = (n.items as i64 + items).max(0) as u32;
            cur = n.parent;
        }
    }

    /// Forgets everything below `id` so it can be rescanned. The old descendants
    /// stay in the arena as unreachable garbage.
    pub fn reset(&mut self, id: NodeId) {
        let n = *self.get(id);
        self.add_up(id, -(n.disk as i64), -(n.items as i64));
        let n = &mut self.nodes[id as usize];
        n.first_child = 0;
        n.n_children = 0;
        n.flags &= !(flags::SCANNED | flags::ERROR);
    }

    fn push_name(&mut self, name: &OsStr) -> (u32, u16) {
        let bytes = name.as_encoded_bytes();
        let off = self.names.len();
        let len = bytes.len().min(u16::MAX as usize);
        self.names.extend_from_slice(&bytes[..len]);
        (u32::try_from(off).expect("name arena exceeded 4 GiB"), len as u16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: Kind, disk: u64) -> Entry {
        Entry { name: name.into(), kind, disk, flags: 0 }
    }

    #[test]
    fn batches_propagate_and_reset() {
        let mut t = Tree::new("/r".into());
        t.apply(Msg::Root { id: ROOT, kind: Kind::Dir, disk: 4, error: false });
        t.apply(Msg::Batch {
            dir: ROOT,
            first: 1,
            entries: vec![entry("a", Kind::Dir, 4), entry("f", Kind::File, 10)],
            error: false,
        });
        t.apply(Msg::Batch { dir: 1, first: 3, entries: vec![entry("g", Kind::File, 100)], error: false });

        assert_eq!(t.get(ROOT).disk, 118);
        assert_eq!(t.get(ROOT).items, 3);
        assert_eq!(t.get(1).disk, 104);
        assert_eq!(t.sorted_children(ROOT), vec![1, 2]);
        assert_eq!(t.path(3), PathBuf::from("/r/a/g"));

        t.reset(1);
        assert_eq!(t.get(ROOT).disk, 14);
        assert_eq!(t.get(ROOT).items, 2);
        assert_eq!(t.children(1).len(), 0);
    }
}
