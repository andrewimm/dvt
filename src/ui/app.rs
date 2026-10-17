use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::widgets::TableState;
use rayon::ThreadPool;

use crate::scan::{self, Msg};
use crate::tree::{NodeId, ROOT, Tree};

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

struct Scan {
    started: Instant,
}

pub struct App {
    pub tree: Tree,
    pub cwd: NodeId,
    pub view: View,
    /// Children of `cwd`, largest first. Refreshed before every frame.
    pub children: Vec<NodeId>,
    /// The selected child, remembered per directory.
    sel: HashMap<NodeId, NodeId>,
    pub show_help: bool,
    pub quit: bool,
    message: Option<(String, Instant)>,

    scan: Option<Scan>,
    pub last_scan: Option<Duration>,
    opts: scan::Options,
    pool: Arc<ThreadPool>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,

    /// Written by the views while rendering, read by navigation and the mouse.
    pub tiles: Vec<Tile>,
    pub list: TableState,
    pub list_area: Rect,
}

const MESSAGE_TTL: Duration = Duration::from_secs(3);

impl App {
    pub fn new(root: PathBuf, opts: scan::Options) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut app = App {
            tree: Tree::new(root),
            cwd: ROOT,
            view: View::Treemap,
            children: Vec::new(),
            sel: HashMap::new(),
            show_help: false,
            quit: false,
            message: None,
            scan: None,
            last_scan: None,
            opts,
            pool: scan::pool(),
            tx,
            rx,
            tiles: Vec::new(),
            list: TableState::default(),
            list_area: Rect::default(),
        };
        app.start_scan(ROOT);
        app
    }

    fn start_scan(&mut self, id: NodeId) {
        let path = self.tree.path(id);
        scan::spawn(path, id, self.tree.ids.clone(), self.opts, self.pool.clone(), self.tx.clone());
        self.scan = Some(Scan { started: Instant::now() });
    }

    pub fn scan_elapsed(&self) -> Option<Duration> {
        self.scan.as_ref().map(|s| s.started.elapsed())
    }

    /// Applies pending scanner messages for up to `budget`. Returns whether
    /// anything changed.
    pub fn drain(&mut self, budget: Duration) -> bool {
        let start = Instant::now();
        let mut changed = false;
        while let Ok(msg) = self.rx.try_recv() {
            changed = true;
            if matches!(msg, Msg::Done) {
                self.last_scan = self.scan.take().map(|s| s.started.elapsed());
            }
            self.tree.apply(msg);
            if start.elapsed() >= budget {
                break;
            }
        }
        changed
    }

    pub fn refresh(&mut self) {
        self.children = self.tree.sorted_children(self.cwd);
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
        match (self.view, k.code) {
            (_, KeyCode::Char('q')) => self.quit = true,
            (_, KeyCode::Esc) => {}
            (_, KeyCode::Char('?')) => self.show_help = true,
            (_, KeyCode::Tab) => self.toggle_view(),
            (_, KeyCode::Enter) => self.enter(),
            (_, KeyCode::Backspace | KeyCode::Char('u')) => self.up(),
            (_, KeyCode::Char('r')) => self.rescan(self.selected().filter(|&id| self.tree.get(id).is_dir())),
            (_, KeyCode::Char('R')) => self.rescan(Some(self.cwd)),
            (_, KeyCode::Char('y')) => self.copy_path(),

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
                        let i = self.list.offset() + (m.row - body_top) as usize;
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

    fn set_cwd(&mut self, id: NodeId) {
        self.cwd = id;
        self.children = self.tree.sorted_children(id);
        *self.list.offset_mut() = 0;
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
            None => self.say("already at the scan root"),
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
        if self.scan.is_some() {
            self.say("a scan is already running");
            return;
        }
        // The old nodes below `id` are discarded, so step out of them first.
        if self.is_ancestor(id, self.cwd) {
            self.set_cwd(id);
        }
        self.tree.reset(id);
        self.start_scan(id);
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
