//! Arena-backed file tree. The UI thread is the single owner and writer; scanner
//! workers only ever send messages (see `scan`), so no locking is needed here.
//!
//! Several scans may be in flight at once (initial scan, rescans, live
//! refreshes). Stale messages are dropped by two rules: nothing is applied to a
//! `DEAD` node, and a directory's batch is applied only once (`SCANNED`).

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::scan::{Listing, Msg};

pub type NodeId = u32;

pub const ROOT: NodeId = 0;
const NONE: u32 = u32::MAX;

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
    /// Not part of the tree: removed, replaced by a rescan, or never created.
    pub const DEAD: u8 = 1 << 4;
    /// Matched an exclude pattern: listed, but not read or counted.
    pub const EXCLUDED: u8 = 1 << 5;
    /// Directories with any of these flags are not scanned.
    pub const NO_DESCEND: u8 = ERROR | OTHER_FS | EXCLUDED;
}

/// 32 bytes per entry; names live in a shared byte arena and child lists in a
/// side table that only directories use.
#[derive(Clone, Copy, Debug)]
pub struct Node {
    /// Disk usage in bytes (allocated blocks), including all descendants.
    pub disk: u64,
    name_off: u32,
    pub parent: NodeId,
    /// Index into `Tree::dirs`, or `NONE`.
    dir: u32,
    /// Number of descendant entries.
    pub items: u32,
    name_len: u16,
    pub kind: Kind,
    pub flags: u8,
}

impl Default for Node {
    /// A placeholder for an id that no applied message has created.
    fn default() -> Self {
        Node {
            disk: 0,
            name_off: 0,
            parent: NONE,
            dir: NONE,
            items: 0,
            name_len: 0,
            kind: Kind::Other,
            flags: flags::DEAD,
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
    dirs: Vec<Vec<NodeId>>,
    free_dirs: Vec<u32>,
    root: NodeId,
    root_path: PathBuf,
    /// Shared with scanners, which allocate node ids for the entries they find.
    pub ids: Arc<AtomicU32>,
    pub errors: u32,
    /// Bumped on every change, so views can tell when to recompute.
    pub version: u64,
    /// New directories found by live refreshes that still need a full scan.
    pending_scans: Vec<NodeId>,
}

impl Tree {
    pub fn new(root_path: PathBuf) -> Self {
        let root = Node {
            kind: Kind::Dir,
            flags: 0,
            ..Node::default()
        };
        Tree {
            nodes: vec![root],
            names: Vec::new(),
            dirs: Vec::new(),
            free_dirs: Vec::new(),
            root: ROOT,
            root_path,
            ids: Arc::new(AtomicU32::new(ROOT + 1)),
            errors: 0,
            version: 0,
            pending_scans: Vec::new(),
        }
    }

    pub fn get(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    /// Size of the arena, including dead nodes.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    /// Ids past the end of the arena belong to batches that were dropped as
    /// stale, so they count as dead too.
    pub fn is_live(&self, id: NodeId) -> bool {
        self.nodes
            .get(id as usize)
            .is_some_and(|n| !n.has(flags::DEAD))
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        let p = self.get(id).parent;
        (p != NONE).then_some(p)
    }

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        match self.get(id).dir {
            NONE => &[],
            d => &self.dirs[d as usize],
        }
    }

    /// Children sorted largest first, ties broken by name.
    pub fn sorted_children(&self, id: NodeId) -> Vec<NodeId> {
        let mut v = self.children(id).to_vec();
        v.sort_unstable_by(|&a, &b| self.cmp_size(a, b));
        v
    }

    /// The `k` largest children, sorted, without sorting the whole list.
    pub fn largest_children(&self, id: NodeId, k: usize) -> Vec<NodeId> {
        let mut v = self.children(id).to_vec();
        if v.len() > k && k > 0 {
            v.select_nth_unstable_by(k - 1, |&a, &b| self.cmp_size(a, b));
            v.truncate(k);
        }
        v.sort_unstable_by(|&a, &b| self.cmp_size(a, b));
        v
    }

    fn cmp_size(&self, a: NodeId, b: NodeId) -> std::cmp::Ordering {
        self.get(b)
            .disk
            .cmp(&self.get(a).disk)
            .then_with(|| self.name(a).cmp(self.name(b)))
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
        while cur != self.root {
            parts.push(cur);
            match self.parent(cur) {
                Some(p) => cur = p,
                None => break,
            }
        }
        let mut path = self.root_path.clone();
        for &part in parts.iter().rev() {
            path.push(self.name(part));
        }
        path
    }

    /// The deepest live node on the way to `path`, if `path` is under the root.
    pub fn lookup(&self, path: &Path) -> Option<NodeId> {
        let rest = path.strip_prefix(&self.root_path).ok()?;
        let mut cur = self.root;
        for part in rest.components() {
            match self
                .children(cur)
                .iter()
                .find(|&&c| self.name(c) == part.as_os_str())
            {
                Some(&c) if self.is_live(c) => cur = c,
                _ => break,
            }
        }
        Some(cur)
    }

    pub fn take_pending_scans(&mut self) -> Vec<NodeId> {
        std::mem::take(&mut self.pending_scans)
    }

    pub fn apply(&mut self, msg: Msg) {
        self.version += 1;
        match msg {
            Msg::Root {
                id,
                kind,
                disk,
                error,
            } => {
                if !self.is_live(id) {
                    return;
                }
                let n = &mut self.nodes[id as usize];
                n.kind = kind;
                if error {
                    n.flags |= flags::ERROR;
                    self.errors += 1;
                }
                self.add_up(id, disk as i64, 0);
            }
            Msg::Batch {
                dir,
                first,
                listing,
                error,
            } => {
                if !self.is_live(dir) || self.get(dir).has(flags::SCANNED) {
                    return;
                }
                let new = self.create_entries(dir, first, &listing);
                self.finish_dir(dir, new, error);
            }
            Msg::Refresh {
                dir,
                listing,
                error,
            } => self.apply_refresh(dir, listing, error),
            Msg::Done => {}
        }
    }

    /// Creates nodes `first..` for the listing under `dir` and adds their
    /// sizes to `dir` and its ancestors. Returns the new ids.
    fn create_entries(&mut self, dir: NodeId, first: NodeId, listing: &Listing) -> Vec<NodeId> {
        let count = listing.len() as u32;
        let needed = (first + count) as usize;
        if self.nodes.len() < needed {
            self.nodes.resize(needed, Node::default());
        }
        // All the names in one copy; each node then points into it.
        let base = self.names.len();
        self.names.extend_from_slice(listing.names());
        u32::try_from(self.names.len()).expect("name arena exceeded 4 GiB");
        let mut total = 0u64;
        for (i, e) in listing.entries.iter().enumerate() {
            let range = listing.name_range(i);
            let name_off = (base + range.start) as u32;
            let name_len = range.len().min(u16::MAX as usize) as u16;
            if e.flags & flags::ERROR != 0 {
                self.errors += 1;
            }
            total += e.disk;
            self.nodes[first as usize + i] = Node {
                disk: e.disk,
                name_off,
                parent: dir,
                dir: NONE,
                items: 0,
                name_len,
                kind: e.kind,
                flags: e.flags,
            };
        }
        self.add_up(dir, total as i64, count as i64);
        (first..first + count).collect()
    }

    /// Appends `children` to `dir` and marks it scanned.
    fn finish_dir(&mut self, dir: NodeId, children: Vec<NodeId>, error: bool) {
        match self.get(dir).dir {
            NONE => {
                let slot = self.alloc_dir(children);
                self.nodes[dir as usize].dir = slot;
            }
            d => self.dirs[d as usize].extend(children),
        }
        let d = &mut self.nodes[dir as usize];
        d.flags |= flags::SCANNED;
        if error {
            d.flags |= flags::ERROR;
            self.errors += 1;
        }
    }

    /// Re-reads one directory in place: keeps the subtrees of entries that are
    /// still there, updates file sizes, drops removed entries, and queues new
    /// directories for scanning.
    fn apply_refresh(&mut self, dir: NodeId, listing: Listing, error: bool) {
        if !self.is_live(dir) || !self.get(dir).has(flags::SCANNED) {
            return;
        }
        let mut old: HashMap<OsString, NodeId> = self
            .children(dir)
            .iter()
            .map(|&c| (self.name(c).to_os_string(), c))
            .collect();
        let mut kept = Vec::with_capacity(listing.len());
        let mut fresh = Listing::default();
        for (i, e) in listing.entries.iter().enumerate() {
            let name = listing.name(i);
            match old.remove(name) {
                Some(id) if self.get(id).kind == e.kind => {
                    let n = *self.get(id);
                    if !n.is_dir() && !n.has(flags::HARDLINK_DUP) && n.disk != e.disk {
                        self.add_up(id, e.disk as i64 - n.disk as i64, 0);
                    }
                    kept.push(id);
                }
                Some(id) => {
                    self.kill(id);
                    fresh.push(name, e.kind, e.disk, e.flags);
                }
                None => fresh.push(name, e.kind, e.disk, e.flags),
            }
        }
        for (_, id) in old {
            self.kill(id);
        }

        let first = self.ids.fetch_add(fresh.len() as u32, Ordering::Relaxed);
        let mut new_dirs = Vec::new();
        // Known limitation: a new hardlink to a file counted elsewhere is
        // counted again, since refreshes don't share the scan's inode set.
        for (i, e) in fresh.entries.iter().enumerate() {
            if e.kind == Kind::Dir && e.flags & flags::NO_DESCEND == 0 {
                new_dirs.push(first + i as NodeId);
            }
        }
        let created = self.create_entries(dir, first, &fresh);
        kept.extend(created);
        let slot = self.get(dir).dir;
        self.dirs[slot as usize] = kept;
        let d = &mut self.nodes[dir as usize];
        if error && !d.has(flags::ERROR) {
            d.flags |= flags::ERROR;
            self.errors += 1;
        }
        self.pending_scans.extend(new_dirs);
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

    /// Removes `id` from the totals and marks it and its subtree dead. The
    /// caller is responsible for dropping it from its parent's child list.
    fn kill(&mut self, id: NodeId) {
        let n = *self.get(id);
        if let Some(p) = self.parent(id) {
            self.add_up(p, -(n.disk as i64), -(n.items as i64 + 1));
        }
        self.kill_subtree(id);
    }

    fn kill_subtree(&mut self, id: NodeId) {
        let mut stack = vec![id];
        while let Some(id) = stack.pop() {
            let n = &mut self.nodes[id as usize];
            if n.flags & flags::ERROR != 0 {
                self.errors = self.errors.saturating_sub(1);
            }
            n.flags |= flags::DEAD;
            let slot = std::mem::replace(&mut n.dir, NONE);
            if slot != NONE {
                stack.extend(std::mem::take(&mut self.dirs[slot as usize]));
                self.free_dirs.push(slot);
            }
        }
    }

    /// Forgets everything below `id` so it can be rescanned.
    pub fn reset(&mut self, id: NodeId) {
        self.version += 1;
        let n = *self.get(id);
        self.add_up(id, -(n.disk as i64), -(n.items as i64));
        if n.has(flags::ERROR) {
            self.errors = self.errors.saturating_sub(1);
        }
        if n.dir != NONE {
            for c in std::mem::take(&mut self.dirs[n.dir as usize]) {
                self.kill_subtree(c);
            }
        }
        self.nodes[id as usize].flags &= !(flags::SCANNED | flags::ERROR);
    }

    /// Makes the root's parent directory the new root, with the old root as
    /// its only child so far. Returns the new root and the old root's name,
    /// which the scan of the new root must skip.
    pub fn grow_up(&mut self) -> Option<(NodeId, OsString)> {
        self.version += 1;
        let parent_path = self.root_path.parent()?.to_path_buf();
        let name = self.root_path.file_name()?.to_os_string();
        let old = self.root;
        let new = self.ids.fetch_add(1, Ordering::Relaxed);
        if self.nodes.len() <= new as usize {
            self.nodes.resize(new as usize + 1, Node::default());
        }
        let (name_off, name_len) = self.push_name(&name);
        let o = self.nodes[old as usize];
        let slot = self.alloc_dir(vec![old]);
        self.nodes[new as usize] = Node {
            disk: o.disk,
            items: o.items + 1,
            kind: Kind::Dir,
            dir: slot,
            flags: 0,
            ..Node::default()
        };
        let o = &mut self.nodes[old as usize];
        o.parent = new;
        o.name_off = name_off;
        o.name_len = name_len;
        self.root = new;
        self.root_path = parent_path;
        Some((new, name))
    }

    fn alloc_dir(&mut self, children: Vec<NodeId>) -> u32 {
        match self.free_dirs.pop() {
            Some(slot) => {
                self.dirs[slot as usize] = children;
                slot
            }
            None => {
                self.dirs.push(children);
                (self.dirs.len() - 1) as u32
            }
        }
    }

    fn push_name(&mut self, name: &OsStr) -> (u32, u16) {
        let bytes = name.as_encoded_bytes();
        let off = self.names.len();
        let len = bytes.len().min(u16::MAX as usize);
        self.names.extend_from_slice(&bytes[..len]);
        (
            u32::try_from(off).expect("name arena exceeded 4 GiB"),
            len as u16,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Tree {
        let mut t = Tree::new("/r/s".into());
        t.ids.store(4, Ordering::Relaxed);
        t.apply(Msg::Root {
            id: ROOT,
            kind: Kind::Dir,
            disk: 4,
            error: false,
        });
        t.apply(Msg::Batch {
            dir: ROOT,
            first: 1,
            listing: Listing::of(&[("a", Kind::Dir, 4), ("f", Kind::File, 10)]),
            error: false,
        });
        t.apply(Msg::Batch {
            dir: 1,
            first: 3,
            listing: Listing::of(&[("g", Kind::File, 100)]),
            error: false,
        });
        t
    }

    #[test]
    fn batches_propagate_and_reset() {
        let mut t = sample();
        assert_eq!(t.get(ROOT).disk, 118);
        assert_eq!(t.get(ROOT).items, 3);
        assert_eq!(t.get(1).disk, 104);
        assert_eq!(t.sorted_children(ROOT), vec![1, 2]);
        assert_eq!(t.largest_children(ROOT, 1), vec![1]);
        assert_eq!(t.path(3), PathBuf::from("/r/s/a/g"));
        assert_eq!(t.lookup(Path::new("/r/s/a/g")), Some(3));
        assert_eq!(t.lookup(Path::new("/r/s/a/missing/x")), Some(1));
        assert_eq!(t.lookup(Path::new("/elsewhere")), None);

        // A batch for a dir that was never created (its parent's batch was
        // dropped) is ignored.
        t.apply(Msg::Batch {
            dir: 500,
            first: 501,
            listing: Listing::of(&[("x", Kind::File, 7)]),
            error: false,
        });
        assert_eq!(t.get(ROOT).disk, 118);

        // A duplicate batch for an already-scanned dir is ignored.
        t.apply(Msg::Batch {
            dir: 1,
            first: 9,
            listing: Listing::of(&[("dup", Kind::File, 7)]),
            error: false,
        });
        assert_eq!(t.get(ROOT).disk, 118);

        t.reset(1);
        assert_eq!(t.get(ROOT).disk, 14);
        assert_eq!(t.get(ROOT).items, 2);
        assert!(t.children(1).is_empty());
        assert!(!t.is_live(3));
        // Stale messages for the dead subtree are dropped.
        t.apply(Msg::Root {
            id: 3,
            kind: Kind::File,
            disk: 50,
            error: false,
        });
        assert_eq!(t.get(ROOT).disk, 14);
    }

    #[test]
    fn refresh_keeps_subtrees() {
        let mut t = sample();
        // "f" grew, "a" is unchanged (its subtree must survive), "n" is a new dir.
        t.apply(Msg::Refresh {
            dir: ROOT,
            listing: Listing::of(&[
                ("a", Kind::Dir, 4),
                ("f", Kind::File, 30),
                ("n", Kind::Dir, 4),
            ]),
            error: false,
        });
        assert_eq!(t.get(ROOT).disk, 4 + 104 + 30 + 4);
        assert_eq!(t.get(ROOT).items, 4);
        assert_eq!(t.get(1).disk, 104);
        let n = t.lookup(Path::new("/r/s/n")).unwrap();
        assert_eq!(t.take_pending_scans(), vec![n]);

        // "a" removed: its subtree dies and leaves the totals.
        t.apply(Msg::Refresh {
            dir: ROOT,
            listing: Listing::of(&[("f", Kind::File, 30)]),
            error: false,
        });
        assert_eq!(t.get(ROOT).disk, 34);
        assert_eq!(t.get(ROOT).items, 1);
        assert!(!t.is_live(1) && !t.is_live(3));
    }

    #[test]
    fn grow_up_adopts_old_root() {
        let mut t = sample();
        let (new, skip) = t.grow_up().unwrap();
        assert_eq!(skip, OsString::from("s"));
        assert_eq!(t.root(), new);
        assert_eq!(t.path(3), PathBuf::from("/r/s/a/g"));
        assert_eq!(t.get(new).disk, 118);
        assert_eq!(t.get(new).items, 4);

        // The parent's scan reports its own size and siblings, skipping "s".
        t.apply(Msg::Root {
            id: new,
            kind: Kind::Dir,
            disk: 4,
            error: false,
        });
        let first = t.ids.fetch_add(1, Ordering::Relaxed);
        t.apply(Msg::Batch {
            dir: new,
            first,
            listing: Listing::of(&[("sib", Kind::File, 8)]),
            error: false,
        });
        assert_eq!(t.get(new).disk, 130);
        assert_eq!(t.children(new).len(), 2);
    }
}
