//! Parallel directory walker. Workers read one directory each and send its
//! entries as a `Msg::Batch`; they never touch the tree directly.
//!
//! Ordering: a directory's batch is sent before its subdirectories are queued,
//! so the receiver always sees a parent's batch (which creates the child nodes)
//! before any batch for those children.

#[cfg(target_os = "linux")]
mod linux;
mod platform;

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
#[cfg(not(target_os = "linux"))]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crossbeam_channel::Sender;
use rayon::{Scope, ThreadPool};
#[cfg(target_os = "linux")]
use rustix::fd::OwnedFd;
/// Elsewhere, directories are always opened by path and nothing is held.
#[cfg(not(target_os = "linux"))]
type OwnedFd = ();

use crate::exclude::Excludes;
use crate::tree::{Kind, NodeId, flags};

pub enum Msg {
    /// The scan root itself: its kind and own size.
    Root {
        id: NodeId,
        kind: Kind,
        disk: u64,
        error: bool,
    },
    /// The entries of directory `dir`, which get ids `first..first + listing.len()`.
    Batch {
        dir: NodeId,
        first: NodeId,
        listing: Listing,
        error: bool,
    },
    /// A fresh listing of an already-scanned directory (see `refresh`).
    Refresh {
        dir: NodeId,
        listing: Listing,
        error: bool,
    },
    /// A scan started with `spawn` has read every directory under its root.
    Done,
}

/// One directory's entries. Names are packed into a single buffer, so a
/// listing costs two allocations however many entries it has.
#[derive(Default)]
pub struct Listing {
    pub entries: Vec<Entry>,
    names: Vec<u8>,
}

pub struct Entry {
    /// Where this entry's name ends in `Listing::names`; it starts where the
    /// previous one ends.
    name_end: u32,
    pub kind: Kind,
    pub disk: u64,
    pub flags: u8,
}

impl Listing {
    pub fn push(&mut self, name: &OsStr, kind: Kind, disk: u64, flags: u8) {
        self.names.extend_from_slice(name.as_encoded_bytes());
        self.entries.push(Entry {
            name_end: self.names.len() as u32,
            kind,
            disk,
            flags,
        });
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Where entry `i`'s name lies in `names()`.
    pub fn name_range(&self, i: usize) -> std::ops::Range<usize> {
        let start = if i == 0 {
            0
        } else {
            self.entries[i - 1].name_end as usize
        };
        start..self.entries[i].name_end as usize
    }

    pub fn name(&self, i: usize) -> &OsStr {
        // SAFETY: the bytes came from `OsStr::as_encoded_bytes` in `push`.
        unsafe { OsStr::from_encoded_bytes_unchecked(&self.names[self.name_range(i)]) }
    }

    /// Every name, back to back, in entry order.
    pub fn names(&self) -> &[u8] {
        &self.names
    }

    /// A listing of `(name, kind, size)` entries, for tests.
    #[cfg(test)]
    pub fn of(items: &[(&str, Kind, u64)]) -> Listing {
        let mut l = Listing::default();
        for &(name, kind, disk) in items {
            l.push(OsStr::new(name), kind, disk, 0);
        }
        l
    }

    /// The listing without the entry called `name`.
    fn without(self, name: &OsStr) -> Listing {
        let mut out = Listing::default();
        for (i, e) in self.entries.iter().enumerate() {
            if self.name(i) != name {
                out.push(self.name(i), e.kind, e.disk, e.flags);
            }
        }
        out
    }
}

#[derive(Clone, Default)]
pub struct Options {
    /// Don't descend into directories on other filesystems.
    pub one_file_system: bool,
    /// Entries that are listed but neither read nor counted.
    pub excludes: Arc<Excludes>,
}

pub struct Request {
    pub path: PathBuf,
    /// The existing node to scan into.
    pub id: NodeId,
    /// A top-level entry to leave out, because the tree already has it.
    pub skip: Option<OsString>,
    /// Report the root's own size with `Msg::Root`. Off when the node's size
    /// is already known, as for directories found by a refresh.
    pub send_root: bool,
}

pub fn pool() -> Arc<ThreadPool> {
    // Scanning is mostly waiting on stat calls, so oversubscribe the cores.
    let threads = thread::available_parallelism()
        .map_or(4, |n| n.get() * 2)
        .clamp(4, 64);
    Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("dvt-scan-{i}"))
            .build()
            .expect("failed to build scanner thread pool"),
    )
}

struct Ctx {
    tx: Sender<Msg>,
    ids: Arc<AtomicU32>,
    opts: Options,
    root_dev: Option<u64>,
    /// (dev, ino) of multiply-linked files already counted.
    seen_links: Mutex<HashSet<(u64, u64)>>,
    /// Directory descriptors currently held open for children to open
    /// themselves relative to (see `Parent`).
    held_fds: AtomicUsize,
}

/// Most systems allow at least 1024 open files per process.
const MAX_HELD_FDS: usize = 512;

/// Scans `req.path` recursively into node `req.id` on the pool, then sends
/// `Msg::Done`.
pub fn spawn(req: Request, ids: Arc<AtomicU32>, opts: Options, pool: &ThreadPool, tx: Sender<Msg>) {
    pool.spawn(move || {
        let meta = fs::symlink_metadata(&req.path);
        let (kind, disk, root_dev) = match &meta {
            Ok(m) => (
                kind_of(m.file_type()),
                platform::disk_usage(m),
                platform::device(m),
            ),
            Err(_) => (Kind::Other, 0, None),
        };
        if req.send_root {
            let _ = tx.send(Msg::Root {
                id: req.id,
                kind,
                disk,
                error: meta.is_err(),
            });
        }
        if kind == Kind::Dir {
            let ctx = Ctx {
                tx: tx.clone(),
                ids,
                opts,
                root_dev,
                seen_links: Mutex::new(HashSet::new()),
                held_fds: AtomicUsize::new(0),
            };
            rayon::scope(|s| walk(s, req.path, None, req.id, req.skip.as_deref(), &ctx));
        }
        let _ = tx.send(Msg::Done);
    });
}

/// Re-reads the single directory `path` (node `id`) without recursing and
/// sends the listing as `Msg::Refresh`.
pub fn refresh(path: PathBuf, id: NodeId, opts: Options, pool: &ThreadPool, tx: Sender<Msg>) {
    pool.spawn(move || {
        let root_dev = fs::symlink_metadata(&path)
            .ok()
            .and_then(|m| platform::device(&m));
        let ctx = Ctx {
            tx,
            ids: Arc::default(),
            opts,
            root_dev,
            seen_links: Mutex::default(),
            held_fds: AtomicUsize::new(0),
        };
        let (listing, error, _) = read_dir(&path, None, &ctx);
        let _ = ctx.tx.send(Msg::Refresh {
            dir: id,
            listing,
            error,
        });
    });
}

/// A directory kept open so its subdirectories can be opened relative to it,
/// sparing the kernel a walk down the full path for each one. It closes when
/// the last child that needs it has opened itself.
pub(super) struct Parent<'c> {
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fd: OwnedFd,
    ctx: &'c Ctx,
}

impl Drop for Parent<'_> {
    fn drop(&mut self) {
        self.ctx.held_fds.fetch_sub(1, Ordering::Relaxed);
    }
}

fn walk<'s>(
    s: &Scope<'s>,
    path: PathBuf,
    parent: Option<Arc<Parent<'s>>>,
    id: NodeId,
    skip: Option<&OsStr>,
    ctx: &'s Ctx,
) {
    let (mut listing, error, fd) = read_dir(&path, parent.as_deref(), ctx);
    drop(parent);
    if let Some(skip) = skip {
        listing = listing.without(skip);
    }
    let first = ctx.ids.fetch_add(listing.len() as u32, Ordering::Relaxed);
    let subdirs: Vec<(PathBuf, NodeId)> = listing
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.kind == Kind::Dir && e.flags & flags::NO_DESCEND == 0)
        .map(|(i, _)| (path.join(listing.name(i)), first + i as NodeId))
        .collect();
    let batch = Msg::Batch {
        dir: id,
        first,
        listing,
        error,
    };
    if ctx.tx.send(batch).is_err() {
        return; // receiver gone, the app is quitting
    }
    // Hold this directory open for its children, within the budget.
    let held = match fd {
        Some(fd)
            if !subdirs.is_empty()
                && ctx.held_fds.fetch_add(1, Ordering::Relaxed) < MAX_HELD_FDS =>
        {
            Some(Arc::new(Parent { fd, ctx }))
        }
        Some(_) if !subdirs.is_empty() => {
            ctx.held_fds.fetch_sub(1, Ordering::Relaxed);
            None
        }
        _ => None,
    };
    for (child, child_id) in subdirs {
        let held = held.clone();
        s.spawn(move |s| walk(s, child, held, child_id, None, ctx));
    }
}

/// What the readers learn about an entry from `stat`.
struct Stat {
    kind: Kind,
    disk: u64,
    dev: u64,
    ino: u64,
    nlink: u64,
}

/// Applies the scan rules shared by every reader (`-x` and hardlink dedup)
/// and returns the entry's kind, counted size and flags.
fn classify(ctx: &Ctx, st: Stat) -> (Kind, u64, u8) {
    if st.kind == Kind::Dir {
        if ctx.opts.one_file_system && Some(st.dev) != ctx.root_dev {
            return (st.kind, st.disk, flags::OTHER_FS);
        }
    } else if st.nlink > 1 && !ctx.seen_links.lock().unwrap().insert((st.dev, st.ino)) {
        return (st.kind, 0, flags::HARDLINK_DUP);
    }
    (st.kind, st.disk, 0)
}

#[cfg(target_os = "linux")]
use linux::read_dir;

/// The portable reader, on top of `std::fs`.
#[cfg(not(target_os = "linux"))]
fn read_dir(path: &Path, _parent: Option<&Parent>, ctx: &Ctx) -> (Listing, bool, Option<OwnedFd>) {
    let mut listing = Listing::default();
    let Ok(rd) = fs::read_dir(path) else {
        return (listing, true, None);
    };
    let mut error = false;
    for de in rd {
        let Ok(de) = de else {
            error = true;
            continue;
        };
        let name = de.file_name();
        // The file type usually comes free with the listing; an excluded
        // entry needs nothing more.
        if !ctx.opts.excludes.is_empty() {
            let is_dir = de.file_type().is_ok_and(|t| t.is_dir());
            if ctx.opts.excludes.matches(path, &name, is_dir) {
                let kind = if is_dir { Kind::Dir } else { Kind::File };
                listing.push(&name, kind, 0, flags::EXCLUDED);
                continue;
            }
        }
        // `DirEntry::metadata` does not follow symlinks.
        let Ok(meta) = de.metadata() else {
            listing.push(&name, Kind::Other, 0, flags::ERROR);
            continue;
        };
        let (dev, ino, nlink) = platform::identity(&meta);
        let stat = Stat {
            kind: kind_of(meta.file_type()),
            disk: platform::disk_usage(&meta),
            dev,
            ino,
            nlink,
        };
        let (kind, disk, fl) = classify(ctx, stat);
        listing.push(&name, kind, disk, fl);
    }
    (listing, error, None)
}

fn kind_of(ft: fs::FileType) -> Kind {
    if ft.is_dir() {
        Kind::Dir
    } else if ft.is_file() {
        Kind::File
    } else if ft.is_symlink() {
        Kind::Symlink
    } else {
        Kind::Other
    }
}
