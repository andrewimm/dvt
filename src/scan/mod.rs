//! Parallel directory walker. Workers read one directory each and send its
//! entries as a `Msg::Batch`; they never touch the tree directly.
//!
//! Ordering: a directory's batch is sent before its subdirectories are queued,
//! so the receiver always sees a parent's batch (which creates the child nodes)
//! before any batch for those children.

mod platform;

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crossbeam_channel::Sender;
use rayon::{Scope, ThreadPool};

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
    /// The entries of directory `dir`, which get ids `first..first + entries.len()`.
    Batch {
        dir: NodeId,
        first: NodeId,
        entries: Vec<Entry>,
        error: bool,
    },
    /// A fresh listing of an already-scanned directory (see `refresh`).
    Refresh {
        dir: NodeId,
        entries: Vec<Entry>,
        error: bool,
    },
    /// A scan started with `spawn` has read every directory under its root.
    Done,
}

pub struct Entry {
    pub name: OsString,
    pub kind: Kind,
    pub disk: u64,
    pub flags: u8,
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
}

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
            };
            rayon::scope(|s| walk(s, req.path, req.id, req.skip.as_deref(), &ctx));
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
        };
        let (entries, error) = read_dir(&path, &ctx);
        let _ = ctx.tx.send(Msg::Refresh {
            dir: id,
            entries,
            error,
        });
    });
}

fn walk<'s>(s: &Scope<'s>, path: PathBuf, id: NodeId, skip: Option<&OsStr>, ctx: &'s Ctx) {
    let (mut entries, error) = read_dir(&path, ctx);
    if let Some(skip) = skip {
        entries.retain(|e| e.name != skip);
    }
    let first = ctx.ids.fetch_add(entries.len() as u32, Ordering::Relaxed);
    let subdirs: Vec<(PathBuf, NodeId)> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.kind == Kind::Dir && e.flags & flags::NO_DESCEND == 0)
        .map(|(i, e)| (path.join(&e.name), first + i as NodeId))
        .collect();
    if ctx
        .tx
        .send(Msg::Batch {
            dir: id,
            first,
            entries,
            error,
        })
        .is_err()
    {
        return; // receiver gone, the app is quitting
    }
    for (child, child_id) in subdirs {
        s.spawn(move |s| walk(s, child, child_id, None, ctx));
    }
}

fn read_dir(path: &Path, ctx: &Ctx) -> (Vec<Entry>, bool) {
    let Ok(rd) = fs::read_dir(path) else {
        return (Vec::new(), true);
    };
    let mut error = false;
    let mut entries = Vec::new();
    for de in rd {
        let Ok(de) = de else {
            error = true;
            continue;
        };
        let name = de.file_name();
        // Checked before stat-ing: the file type usually comes free with the
        // directory listing, and an excluded entry needs nothing more.
        if !ctx.opts.excludes.is_empty() {
            let is_dir = de.file_type().is_ok_and(|t| t.is_dir());
            if ctx.opts.excludes.matches(path, &name, is_dir) {
                let kind = if is_dir { Kind::Dir } else { Kind::File };
                entries.push(Entry {
                    name,
                    kind,
                    disk: 0,
                    flags: flags::EXCLUDED,
                });
                continue;
            }
        }
        // `DirEntry::metadata` does not follow symlinks (fstatat on unix).
        let Ok(meta) = de.metadata() else {
            entries.push(Entry {
                name,
                kind: Kind::Other,
                disk: 0,
                flags: flags::ERROR,
            });
            continue;
        };
        let kind = kind_of(meta.file_type());
        let mut disk = platform::disk_usage(&meta);
        let mut fl = 0;
        if kind == Kind::Dir {
            if ctx.opts.one_file_system && platform::device(&meta) != ctx.root_dev {
                fl |= flags::OTHER_FS;
            }
        } else if let Some(key) = platform::hardlink_key(&meta)
            && !ctx.seen_links.lock().unwrap().insert(key)
        {
            disk = 0;
            fl |= flags::HARDLINK_DUP;
        }
        entries.push(Entry {
            name,
            kind,
            disk,
            flags: fl,
        });
    }
    (entries, error)
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
