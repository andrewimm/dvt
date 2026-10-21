//! Watches the directory being viewed for changes (inotify on Linux, FSEvents
//! on macOS, via the `notify` crate) and reports the paths that changed.
//!
//! Adding a recursive inotify watch walks the whole subtree, so it runs on a
//! dedicated thread; if the view moves on while that is happening, only the
//! latest request is honoured.

use std::path::PathBuf;
use std::thread;

use crossbeam_channel::{Receiver, Sender};
use notify::event::{EventKind, ModifyKind};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher as _};

/// FSEvents watches are always recursive, and cost nothing per directory.
pub const RECURSIVE_IS_FREE: bool = cfg!(target_os = "macos");

pub enum Change {
    /// An entry was created, removed, renamed or written.
    Path(PathBuf),
    /// Events were dropped (the kernel's queue overflowed), so everything
    /// below this directory, or the whole watch for `None`, may be stale.
    Rescan(Option<PathBuf>),
}

pub struct Watcher {
    cmds: Sender<Option<(PathBuf, bool)>>,
    pub events: Receiver<Change>,
}

impl Watcher {
    pub fn new() -> Option<Self> {
        let (ev_tx, events) = crossbeam_channel::unbounded();
        let mut watcher = RecommendedWatcher::new(
            move |res: notify::Result<notify::Event>| {
                let Ok(ev) = res else { return };
                if ev.need_rescan() {
                    let _ = ev_tx.send(Change::Rescan(ev.paths.into_iter().next()));
                    return;
                }
                // Opens and closes (including our own reads) and metadata-only
                // changes don't affect sizes.
                let relevant = matches!(
                    ev.kind,
                    EventKind::Any
                        | EventKind::Create(_)
                        | EventKind::Remove(_)
                        | EventKind::Modify(
                            ModifyKind::Any | ModifyKind::Data(_) | ModifyKind::Name(_)
                        )
                );
                if relevant {
                    for p in ev.paths {
                        let _ = ev_tx.send(Change::Path(p));
                    }
                }
            },
            Config::default().with_follow_symlinks(false),
        )
        .ok()?;

        let (cmds, cmd_rx) = crossbeam_channel::unbounded::<Option<(PathBuf, bool)>>();
        thread::Builder::new()
            .name("dvt-watch".into())
            .spawn(move || {
                let mut current: Option<PathBuf> = None;
                while let Ok(mut cmd) = cmd_rx.recv() {
                    // Skip to the most recent request.
                    while let Ok(next) = cmd_rx.try_recv() {
                        cmd = next;
                    }
                    if let Some(old) = current.take() {
                        let _ = watcher.unwatch(&old);
                    }
                    if let Some((path, recursive)) = cmd {
                        let mode = if recursive {
                            RecursiveMode::Recursive
                        } else {
                            RecursiveMode::NonRecursive
                        };
                        if watcher.watch(&path, mode).is_ok() {
                            current = Some(path);
                        }
                    }
                }
            })
            .ok()?;
        Some(Watcher { cmds, events })
    }

    /// Watches `path` (replacing any previous watch), or nothing for `None`.
    pub fn watch(&self, target: Option<(PathBuf, bool)>) {
        let _ = self.cmds.send(target);
    }
}
