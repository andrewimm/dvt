use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use rayon::ThreadPool;

use crate::scan::{self, Msg, Request};
use crate::search::{self as find, Matcher};
use crate::tree::{NodeId, ROOT, Tree, flags};
use crate::watch::Watcher;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Treemap,
    List,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TileItem {
    Node(NodeId),
    /// Everything from `children[first..]` merged into one tile.
    Others { first: usize, count: usize, disk: u64 },
}

pub struct Tile {
    pub item: TileItem,
    pub rect: Rect,
}

#[derive(Clone, Copy)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// The `/` search overlay. Results are the top-most matching entries under
/// `under`, largest first.
pub struct Search {
    pub open: bool,
    pub query: String,
    pub under: NodeId,
    pub results: Vec<NodeId>,
    pub total: u64,
    pub sel: usize,
    pub offset: usize,
    pub took: Duration,
    pub area: Rect,
    stale: bool,
    last_run: Instant,
}

/// How often to re-run an open search while scans change the tree.
const SEARCH_RERUN: Duration = Duration::from_millis(500);

pub struct App {
    pub tree: Tree,
    pub cwd: NodeId,
    pub view: View,
    pub search: Search,
    /// Children of `cwd`, largest first. Refreshed before every frame.
    pub children: Vec<NodeId>,
    /// The selected child, remembered per directory.
    sel: HashMap<NodeId, NodeId>,
    pub show_help: bool,
    pub quit: bool,
    message: Option<(String, Instant)>,

    /// Full scans in flight, and when the current busy period started.
    scans: usize,
    scan_started: Option<Instant>,
    pub last_scan: Option<Duration>,
    opts: scan::Options,
    pool: Arc<ThreadPool>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,

    /// Draw directories' contents inside their treemap tiles.
    pub nested: bool,
    /// Live updates: `watched` is the directory being watched and whether the
    /// watch is recursive.
    pub live: bool,
    watcher: Option<Watcher>,
    pub watched: Option<(NodeId, bool)>,
    changed: HashSet<PathBuf>,
    last_change: Instant,
    last_refresh: HashMap<NodeId, Instant>,

    /// Written by the views while rendering, read by navigation and the mouse.
    pub tiles: Vec<Tile>,
    pub list_offset: usize,
    pub list_area: Rect,
}

const MESSAGE_TTL: Duration = Duration::from_secs(3);
/// Directories with more entries than this below them are watched
/// non-recursively, since a recursive watch costs one inotify watch per directory.
const RECURSIVE_WATCH_LIMIT: u32 = 200_000;
/// Wait for changes to settle before re-reading.
const CHANGE_DEBOUNCE: Duration = Duration::from_millis(300);
/// Re-read any one directory at most this often.
const REFRESH_INTERVAL: Duration = Duration::from_secs(1);

impl App {
    pub fn new(root: PathBuf, opts: scan::Options, live: bool) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut app = App {
            tree: Tree::new(root),
            cwd: ROOT,
            view: View::Treemap,
            search: Search {
                open: false,
                query: String::new(),
                under: ROOT,
                results: Vec::new(),
                total: 0,
                sel: 0,
                offset: 0,
                took: Duration::ZERO,
                area: Rect::default(),
                stale: false,
                last_run: Instant::now(),
            },
            children: Vec::new(),
            sel: HashMap::new(),
            show_help: false,
            quit: false,
            message: None,
            scans: 0,
            scan_started: None,
            last_scan: None,
            opts,
            pool: scan::pool(),
            tx,
            rx,
            nested: false,
            live,
            watcher: if live { Watcher::new() } else { None },
            watched: None,
            changed: HashSet::new(),
            last_change: Instant::now(),
            last_refresh: HashMap::new(),
            tiles: Vec::new(),
            list_offset: 0,
            list_area: Rect::default(),
        };
        app.start_scan(ROOT, None, true);
        app
    }

    fn start_scan(&mut self, id: NodeId, skip: Option<std::ffi::OsString>, send_root: bool) {
        let req = Request { path: self.tree.path(id), id, skip, send_root };
        scan::spawn(req, self.tree.ids.clone(), self.opts, &self.pool, self.tx.clone());
        self.scans += 1;
        self.scan_started.get_or_insert_with(Instant::now);
    }

    pub fn scan_elapsed(&self) -> Option<Duration> {
        self.scan_started.map(|t| t.elapsed())
    }

    /// Applies pending scanner messages for up to `budget`. Returns whether
    /// anything changed.
    pub fn drain(&mut self, budget: Duration) -> bool {
        let start = Instant::now();
        let mut changed = false;
        while let Ok(msg) = self.rx.try_recv() {
            changed = true;
            if matches!(msg, Msg::Done) {
                self.scans -= 1;
                if self.scans == 0 {
                    self.last_scan = self.scan_started.take().map(|t| t.elapsed());
                }
            }
            self.tree.apply(msg);
            if start.elapsed() >= budget {
                break;
            }
        }
        for id in self.tree.take_pending_scans() {
            self.start_scan(id, None, false);
        }
        // The directory being viewed may have been deleted.
        while !self.tree.is_live(self.cwd) {
            let Some(p) = self.tree.parent(self.cwd) else { break };
            self.set_cwd(p);
        }
        self.process_changes();
        if changed && self.search.open {
            self.search.stale = true;
        }
        changed
    }

    // Search

    fn open_search(&mut self) {
        self.search.open = true;
        self.search.under = self.cwd;
        self.run_search(false);
    }

    /// Re-runs the search. With `keep_selection`, the same entry stays
    /// selected if it is still a result; otherwise the largest is selected.
    fn run_search(&mut self, keep_selection: bool) {
        let s = &mut self.search;
        if !self.tree.is_live(s.under) {
            s.under = self.cwd;
        }
        let prev = s.results.get(s.sel).copied().filter(|_| keep_selection);
        let start = Instant::now();
        match Matcher::new(&s.query) {
            Some(m) => {
                let r = self.pool.install(|| find::find(&self.tree, s.under, &m));
                s.results = r.ids;
                s.total = r.total;
            }
            None => {
                s.results.clear();
                s.total = 0;
            }
        }
        s.took = start.elapsed();
        s.sel = prev.and_then(|id| s.results.iter().position(|&r| r == id)).unwrap_or(0);
        s.stale = false;
        s.last_run = Instant::now();
    }

    fn search_key(&mut self, k: KeyEvent) {
        let page = self.search.area.height.saturating_sub(2).max(1) as isize;
        match k.code {
            KeyCode::Esc => self.search.open = false,
            KeyCode::Enter => self.jump_to_result(),
            KeyCode::Up => self.move_result(-1),
            KeyCode::Down => self.move_result(1),
            KeyCode::PageUp => self.move_result(-page),
            KeyCode::PageDown => self.move_result(page),
            KeyCode::Home => self.search.sel = 0,
            KeyCode::End => self.move_result(isize::MAX / 2),
            KeyCode::Backspace => {
                if self.search.query.pop().is_some() {
                    self.run_search(false);
                }
            }
            KeyCode::Char(c) if !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                self.search.query.push(c);
                self.run_search(false);
            }
            _ => {}
        }
    }

    fn move_result(&mut self, delta: isize) {
        let last = self.search.results.len().saturating_sub(1) as isize;
        self.search.sel = (self.search.sel as isize).saturating_add(delta).clamp(0, last) as usize;
    }

    /// Closes the search and shows the selected result in its directory.
    fn jump_to_result(&mut self) {
        let Some(&id) = self.search.results.get(self.search.sel) else { return };
        if !self.tree.is_live(id) {
            self.run_search(false);
            return;
        }
        let Some(parent) = self.tree.parent(id) else { return };
        self.search.open = false;
        self.set_cwd(parent);
        self.sel.insert(parent, id);
    }

    fn search_mouse(&mut self, m: MouseEvent) -> bool {
        let area = self.search.area;
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let body_top = area.y + 1;
                if !area.contains(Position::new(m.column, m.row)) || m.row < body_top {
                    return false;
                }
                let i = self.search.offset + (m.row - body_top) as usize;
                if i >= self.search.results.len() {
                    return false;
                }
                if i == self.search.sel {
                    self.jump_to_result();
                } else {
                    self.search.sel = i;
                }
                true
            }
            MouseEventKind::ScrollUp => {
                self.move_result(-3);
                true
            }
            MouseEventKind::ScrollDown => {
                self.move_result(3);
                true
            }
            MouseEventKind::Down(MouseButton::Right) => {
                self.search.open = false;
                true
            }
            _ => false,
        }
    }

    /// Collects filesystem events and, once they settle, re-reads the
    /// directories that contain them.
    fn process_changes(&mut self) {
        let Some(w) = &self.watcher else { return };
        let mut got = false;
        while let Ok(path) = w.events.try_recv() {
            self.changed.insert(path);
            got = true;
        }
        if got {
            self.last_change = Instant::now();
        }
        if self.changed.is_empty() || self.last_change.elapsed() < CHANGE_DEBOUNCE {
            return;
        }
        let mut dirs = HashSet::new();
        for path in std::mem::take(&mut self.changed) {
            // A change to an entry means its parent directory's listing changed.
            let Some(parent) = path.parent() else { continue };
            if let Some(id) = self.tree.lookup(parent) {
                dirs.insert((id, path));
            }
        }
        let mut refreshed = HashSet::new();
        for (id, path) in dirs {
            let n = self.tree.get(id);
            // Directories still being scanned will be read soon anyway.
            if !n.is_dir() || !n.has(flags::SCANNED) || refreshed.contains(&id) {
                continue;
            }
            if self.last_refresh.get(&id).is_some_and(|t| t.elapsed() < REFRESH_INTERVAL) {
                self.changed.insert(path); // try again later
                continue;
            }
            refreshed.insert(id);
            self.last_refresh.insert(id, Instant::now());
            scan::refresh(self.tree.path(id), id, self.opts, &self.pool, self.tx.clone());
        }
    }

    /// Points the watcher at the current directory. Whether to watch
    /// recursively is decided once scanning has settled the directory's size;
    /// until then (and during later scans) a non-recursive watch is used, or
    /// the existing one kept.
    fn update_watch(&mut self) {
        let Some(w) = &self.watcher else { return };
        let target = match (self.live, self.watched) {
            (false, None) => return,
            (false, Some(_)) => None,
            (true, Some((id, _))) if id == self.cwd && self.scans > 0 => return,
            (true, _) => {
                let recursive = self.scans == 0 && self.tree.get(self.cwd).items <= RECURSIVE_WATCH_LIMIT;
                Some((self.cwd, recursive))
            }
        };
        if target != self.watched {
            w.watch(target.map(|(id, rec)| (self.tree.path(id), rec)));
            self.watched = target;
        }
    }

    pub fn refresh(&mut self) {
        self.children = self.tree.sorted_children(self.cwd);
        if self.search.open && self.search.stale && self.search.last_run.elapsed() >= SEARCH_RERUN {
            self.run_search(true);
        }
        self.update_watch();
        if self.message.as_ref().is_some_and(|(_, t)| t.elapsed() > MESSAGE_TTL) {
            self.message = None;
        }
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_ref().map(|(m, _)| m.as_str())
    }

    fn say(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), Instant::now()));
    }

    pub fn selected_index(&self) -> Option<usize> {
        if self.children.is_empty() {
            return None;
        }
        let id = self.sel.get(&self.cwd);
        Some(id.and_then(|id| self.children.iter().position(|c| c == id)).unwrap_or(0))
    }

    pub fn selected(&self) -> Option<NodeId> {
        self.selected_index().map(|i| self.children[i])
    }

    fn select_index(&mut self, i: usize) {
        if let Some(&id) = self.children.get(i) {
            self.sel.insert(self.cwd, id);
        }
    }

    /// Whether `tile` contains the current selection.
    pub fn tile_selected(&self, tile: &TileItem) -> bool {
        match *tile {
            TileItem::Node(id) => self.selected() == Some(id),
            TileItem::Others { first, .. } => self.selected_index().is_some_and(|i| i >= first),
        }
    }

    // Event handling

    /// Returns whether the screen needs redrawing.
    pub fn handle(&mut self, ev: Event) -> bool {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                self.key(k);
                true
            }
            Event::Mouse(m) => self.mouse(m),
            Event::Resize(..) => true,
            _ => false,
        }
    }

    fn key(&mut self, k: KeyEvent) {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.show_help {
            self.show_help = false;
            return;
        }
        if self.search.open {
            self.search_key(k);
            return;
        }
        match (self.view, k.code) {
            (_, KeyCode::Char('/')) => self.open_search(),
            (_, KeyCode::Char('q')) => self.quit = true,
            (_, KeyCode::Esc) => {}
            (_, KeyCode::Char('?')) => self.show_help = true,
            (_, KeyCode::Tab) => self.toggle_view(),
            (_, KeyCode::Enter) => self.enter(),
            (_, KeyCode::Backspace | KeyCode::Char('u')) => self.up(),
            (_, KeyCode::Char('r')) => self.rescan(self.selected().filter(|&id| self.tree.get(id).is_dir())),
            (_, KeyCode::Char('R')) => self.rescan(Some(self.cwd)),
            (_, KeyCode::Char('y')) => self.copy_path(),
            (_, KeyCode::Char('n')) => {
                self.nested = !self.nested;
                self.say(if self.nested { "nested treemap on" } else { "nested treemap off" });
            }
            (_, KeyCode::Char('w')) => self.toggle_live(),

            (View::Treemap, KeyCode::Left | KeyCode::Char('h')) => self.move_spatial(Dir::Left),
            (View::Treemap, KeyCode::Right | KeyCode::Char('l')) => self.move_spatial(Dir::Right),
            (View::Treemap, KeyCode::Up | KeyCode::Char('k')) => self.move_spatial(Dir::Up),
            (View::Treemap, KeyCode::Down | KeyCode::Char('j')) => self.move_spatial(Dir::Down),

            (View::List, KeyCode::Up | KeyCode::Char('k')) => self.move_list(-1),
            (View::List, KeyCode::Down | KeyCode::Char('j')) => self.move_list(1),
            (View::List, KeyCode::PageUp) => self.move_list(-(self.page() as isize)),
            (View::List, KeyCode::PageDown) => self.move_list(self.page() as isize),
            (View::List, KeyCode::Home | KeyCode::Char('g')) => self.select_index(0),
            (View::List, KeyCode::End | KeyCode::Char('G')) => self.select_index(self.children.len().saturating_sub(1)),
            (View::List, KeyCode::Right | KeyCode::Char('l')) => self.enter(),
            (View::List, KeyCode::Left | KeyCode::Char('h')) => self.up(),
            _ => {}
        }
    }

    fn mouse(&mut self, m: MouseEvent) -> bool {
        if self.search.open {
            return self.search_mouse(m);
        }
        let pos = Position::new(m.column, m.row);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                match self.view {
                    View::Treemap => {
                        let Some(item) = self.tiles.iter().find(|t| t.rect.contains(pos)).map(|t| t.item) else {
                            return false;
                        };
                        // Clicking the selected tile opens it.
                        if self.tile_selected(&item) {
                            self.enter();
                        } else {
                            match item {
                                TileItem::Node(id) => {
                                    self.sel.insert(self.cwd, id);
                                }
                                TileItem::Others { first, .. } => self.select_index(first),
                            }
                        }
                    }
                    View::List => {
                        let body_top = self.list_area.y + 1; // below the header row
                        if !self.list_area.contains(pos) || m.row < body_top {
                            return false;
                        }
                        let i = self.list_offset + (m.row - body_top) as usize;
                        if i >= self.children.len() {
                            return false;
                        }
                        if self.selected_index() == Some(i) {
                            self.enter();
                        } else {
                            self.select_index(i);
                        }
                    }
                }
                true
            }
            MouseEventKind::Down(MouseButton::Right) => {
                self.up();
                true
            }
            MouseEventKind::ScrollUp if self.view == View::List => {
                self.move_list(-3);
                true
            }
            MouseEventKind::ScrollDown if self.view == View::List => {
                self.move_list(3);
                true
            }
            _ => false,
        }
    }

    // Actions

    fn toggle_view(&mut self) {
        self.view = match self.view {
            View::Treemap => View::List,
            View::List => View::Treemap,
        };
    }

    fn toggle_live(&mut self) {
        if self.watcher.is_none() {
            self.watcher = Watcher::new();
            if self.watcher.is_none() {
                self.say("live updates are not available here");
                return;
            }
        }
        self.live = !self.live;
        self.say(if self.live { "live updates on" } else { "live updates off" });
    }

    fn set_cwd(&mut self, id: NodeId) {
        self.cwd = id;
        self.children = self.tree.sorted_children(id);
        self.list_offset = 0;
    }

    fn enter(&mut self) {
        let Some(i) = self.selected_index() else { return };
        // In the treemap, opening the merged "others" tile shows them in the list.
        if self.view == View::Treemap {
            let others = self.tiles.iter().find_map(|t| match t.item {
                TileItem::Others { first, .. } if i >= first => Some(first),
                _ => None,
            });
            if let Some(first) = others {
                self.view = View::List;
                self.select_index(first);
                return;
            }
        }
        let id = self.children[i];
        if self.tree.get(id).is_dir() {
            self.set_cwd(id);
        }
    }

    fn up(&mut self) {
        match self.tree.parent(self.cwd) {
            Some(p) => {
                let from = self.cwd;
                self.set_cwd(p);
                self.sel.insert(p, from);
            }
            None => {
                let from = self.cwd;
                match self.tree.grow_up() {
                    Some((root, skip)) => {
                        self.start_scan(root, Some(skip), true);
                        self.set_cwd(root);
                        self.sel.insert(root, from);
                    }
                    None => self.say("already at the top of the filesystem"),
                }
            }
        }
    }

    fn move_list(&mut self, delta: isize) {
        if let Some(i) = self.selected_index() {
            let last = self.children.len() as isize - 1;
            self.select_index((i as isize + delta).clamp(0, last) as usize);
        }
    }

    fn page(&self) -> usize {
        self.list_area.height.saturating_sub(2).max(1) as usize
    }

    /// Moves to the nearest tile in direction `dir`, preferring tiles that
    /// overlap the current one on the perpendicular axis.
    fn move_spatial(&mut self, dir: Dir) {
        let Some(cur) = self.tiles.iter().find(|t| self.tile_selected(&t.item)).map(|t| t.rect) else {
            return;
        };
        let best = self
            .tiles
            .iter()
            .filter_map(|t| {
                let r = t.rect;
                // (gap along the direction, perpendicular overlap, perpendicular center offset)
                let (gap, overlap, offset) = match dir {
                    Dir::Left if r.right() <= cur.x => (cur.x - r.right(), span_overlap(cur.y, cur.bottom(), r.y, r.bottom()), center_dist(cur.y, cur.height, r.y, r.height)),
                    Dir::Right if r.x >= cur.right() => (r.x - cur.right(), span_overlap(cur.y, cur.bottom(), r.y, r.bottom()), center_dist(cur.y, cur.height, r.y, r.height)),
                    Dir::Up if r.bottom() <= cur.y => (cur.y - r.bottom(), span_overlap(cur.x, cur.right(), r.x, r.right()), center_dist(cur.x, cur.width, r.x, r.width)),
                    Dir::Down if r.y >= cur.bottom() => (r.y - cur.bottom(), span_overlap(cur.x, cur.right(), r.x, r.right()), center_dist(cur.x, cur.width, r.x, r.width)),
                    _ => return None,
                };
                Some(((!overlap, gap, offset), t.item))
            })
            .min_by_key(|(key, _)| *key)
            .map(|(_, item)| item);
        match best {
            Some(TileItem::Node(id)) => {
                self.sel.insert(self.cwd, id);
            }
            Some(TileItem::Others { first, .. }) => self.select_index(first),
            None => {}
        }
    }

    fn rescan(&mut self, target: Option<NodeId>) {
        let Some(id) = target else {
            self.say("select a directory to rescan");
            return;
        };
        // The old nodes below `id` are discarded, so step out of them first.
        if self.is_ancestor(id, self.cwd) {
            self.set_cwd(id);
        }
        self.tree.reset(id);
        self.start_scan(id, None, true);
    }

    fn is_ancestor(&self, ancestor: NodeId, mut id: NodeId) -> bool {
        loop {
            if id == ancestor {
                return true;
            }
            match self.tree.parent(id) {
                Some(p) => id = p,
                None => return false,
            }
        }
    }

    /// Copies the selected path to the clipboard using the OSC 52 escape,
    /// which also works over ssh.
    fn copy_path(&mut self) {
        let path = self.tree.path(self.selected().unwrap_or(self.cwd));
        let seq = format!("\x1b]52;c;{}\x07", base64(path.as_os_str().as_encoded_bytes()));
        let mut out = std::io::stdout();
        let _ = out.write_all(seq.as_bytes()).and_then(|_| out.flush());
        self.say(format!("copied {}", path.display()));
    }
}

fn span_overlap(a0: u16, a1: u16, b0: u16, b1: u16) -> bool {
    a0.max(b0) < a1.min(b1)
}

fn center_dist(a: u16, alen: u16, b: u16, blen: u16) -> u32 {
    let ca = 2 * a as i32 + alen as i32;
    let cb = 2 * b as i32 + blen as i32;
    ca.abs_diff(cb)
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foo"), "Zm9v");
        assert_eq!(super::base64(b"/home/a"), "L2hvbWUvYQ==");
    }
}
