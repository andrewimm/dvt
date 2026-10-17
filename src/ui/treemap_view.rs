use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::Paragraph;

use super::app::{App, Tile, TileItem};
use super::theme::{self, Rgb};
use crate::fmt;
use crate::layout::treemap::{FRect, squarify};
use crate::tree::{Kind, NodeId, Tree};

/// Entries smaller than this many cells are merged into one "others" tile.
const MIN_TILE_CELLS: f64 = 20.0;
/// The same, for tiles drawn inside a directory's tile in nested mode.
const MIN_INNER_CELLS: f64 = 12.0;
const MAX_TILES: usize = 400;
const MAX_INNER_TILES: usize = 64;
/// A directory tile must be at least this big to show its contents.
const NEST_MIN_W: u16 = 8;
const NEST_MIN_H: u16 = 4;
/// Don't draw a name unless at least this many of its columns fit.
const MIN_LABEL: usize = 3;
/// Terminal cells are roughly twice as tall as they are wide.
const CELL_ASPECT: f64 = 2.0;

pub fn render(f: &mut Frame, area: Rect, app: &mut App) {
    let total: u64 = app.children.iter().map(|&id| app.tree.get(id).disk).sum();
    app.tiles = layout(&app.tree, &app.children, app.children.len(), total, area, MIN_TILE_CELLS, MAX_TILES);
    if app.tiles.is_empty() {
        let text = if app.scan_elapsed().is_some() { "scanning…" } else { "(empty)" };
        let y = area.y + area.height / 2;
        f.render_widget(
            Paragraph::new(text).style(theme::dim()).alignment(Alignment::Center),
            Rect::new(area.x, y, area.width, 1),
        );
        return;
    }
    let colors = assign_colors(&app.tree, &app.tiles, theme::tile_bg);
    let buf = f.buffer_mut();
    for (tile, &bg) in app.tiles.iter().zip(&colors) {
        let selected = app.tile_selected(&tile.item);
        draw_tile(buf, &app.tree, tile, bg, selected, app.nested);
    }
}

/// Lays out the `ids` (sorted largest first; the largest of `n_all` children
/// totalling `total`) in `area`. Whatever is too small, beyond `max_tiles`, or
/// not in `ids` becomes one trailing "others" tile.
fn layout(tree: &Tree, ids: &[NodeId], n_all: usize, total: u64, area: Rect, min_cells: f64, max_tiles: usize) -> Vec<Tile> {
    if total == 0 || area.is_empty() {
        return Vec::new();
    }
    let cells = area.width as f64 * area.height as f64;
    let mut items = Vec::new();
    let mut values = Vec::new();
    let mut kept_disk = 0;
    for &id in ids.iter().take(max_tiles) {
        let disk = tree.get(id).disk;
        if (disk as f64 / total as f64) * cells < min_cells {
            break;
        }
        items.push(TileItem::Node(id));
        values.push(disk as f64);
        kept_disk += disk;
    }
    let kept = items.len();
    let rest = total.saturating_sub(kept_disk);
    if rest > 0 {
        items.push(TileItem::Others { first: kept, count: n_all - kept, disk: rest });
        values.push(rest as f64);
    }

    let bounds = FRect { x: 0.0, y: 0.0, w: area.width as f64, h: area.height as f64 * CELL_ASPECT };
    squarify(&values, bounds)
        .into_iter()
        .zip(items)
        .filter_map(|(r, item)| {
            let x0 = r.x.round() as u16;
            let x1 = (r.x + r.w).round() as u16;
            let y0 = (r.y / CELL_ASPECT).round() as u16;
            let y1 = ((r.y + r.h) / CELL_ASPECT).round() as u16;
            let rect = Rect::new(area.x + x0, area.y + y0, x1.saturating_sub(x0), y1.saturating_sub(y0));
            (!rect.is_empty()).then_some(Tile { item, rect })
        })
        .collect()
}

/// Greedy colouring: each tile takes the first shade (from `shade(kind, n)`)
/// that none of its already-coloured neighbours in the same palette use.
fn assign_colors(tree: &Tree, tiles: &[Tile], shade: impl Fn(Option<Kind>, usize) -> Rgb) -> Vec<Rgb> {
    let kind = |item: TileItem| match item {
        TileItem::Node(id) => Some(tree.get(id).kind),
        TileItem::Others { .. } => None,
    };
    let mut chosen: Vec<usize> = Vec::with_capacity(tiles.len());
    for (i, t) in tiles.iter().enumerate() {
        let k = kind(t.item);
        let taken: Vec<usize> = tiles[..i]
            .iter()
            .zip(&chosen)
            .filter(|(o, _)| theme::same_palette(kind(o.item), k) && touching(o.rect, t.rect))
            .map(|(_, &c)| c)
            .collect();
        chosen.push((0..).find(|c| !taken.contains(c)).unwrap());
    }
    tiles.iter().zip(chosen).map(|(t, c)| shade(kind(t.item), c)).collect()
}

fn touching(a: Rect, b: Rect) -> bool {
    let overlap = |a0: u16, a1: u16, b0: u16, b1: u16| a0.max(b0) < a1.min(b1);
    ((a.right() == b.x || b.right() == a.x) && overlap(a.y, a.bottom(), b.y, b.bottom()))
        || ((a.bottom() == b.y || b.bottom() == a.y) && overlap(a.x, a.right(), b.x, b.right()))
}

fn label(tree: &Tree, item: TileItem) -> (Option<Kind>, String, u64) {
    match item {
        TileItem::Node(id) => {
            let n = tree.get(id);
            let mut name = tree.name(id).to_string_lossy().into_owned();
            if n.is_dir() {
                name.push('/');
            }
            (Some(n.kind), name, n.disk)
        }
        TileItem::Others { count, disk, .. } => (None, format!("+{} more", fmt::count(count as u64)), disk),
    }
}

fn draw_tile(buf: &mut Buffer, tree: &Tree, tile: &Tile, base: Rgb, selected: bool, nested: bool) {
    let r = tile.rect;
    let (kind, name, disk) = label(tree, tile.item);
    let bg = if selected { theme::SELECTED } else { base };
    let bold = kind == Some(Kind::Dir) || selected;
    let inner = Rect::new(r.x + 1, r.y + 1, r.width.saturating_sub(2), r.height.saturating_sub(2));

    let TileItem::Node(id) = tile.item else {
        paint(buf, r, bg, &name, disk, bold, false);
        return;
    };
    if !(nested && kind == Some(Kind::Dir) && r.width >= NEST_MIN_W && r.height >= NEST_MIN_H) {
        paint(buf, r, bg, &name, disk, bold, false);
        return;
    }

    // Nested: a one-line header, then the directory's own children inside a
    // frame of the tile's colour, shaded from the tile's base colour.
    paint(buf, r, bg, &name, disk, bold, true);
    let n_all = tree.children(id).len();
    let ids = tree.largest_children(id, MAX_INNER_TILES);
    let total: u64 = tree.children(id).iter().map(|&c| tree.get(c).disk).sum();
    let tiles = layout(tree, &ids, n_all, total, inner, MIN_INNER_CELLS, MAX_INNER_TILES);
    let colors = assign_colors(tree, &tiles, |_, c| theme::inner_shade(base, c));
    for (t, &c) in tiles.iter().zip(&colors) {
        let (kind, name, disk) = label(tree, t.item);
        paint(buf, t.rect, c, &name, disk, kind == Some(Kind::Dir), false);
    }
}

/// Fills `r` with `bg` (darker on the right and bottom edges, to separate it
/// from its neighbours) and labels it with the name and size where they fit.
/// With `header_only`, the label is a single line however tall the tile is.
fn paint(buf: &mut Buffer, r: Rect, bg: Rgb, name: &str, disk: u64, bold: bool, header_only: bool) {
    let style = theme::tile_style(bg);
    buf.set_style(r, style);
    for y in r.top()..r.bottom() {
        for x in r.left()..r.right() {
            buf[(x, y)].set_char(' ');
        }
    }
    let edge = theme::edge(bg);
    let has_right_edge = r.width >= 3;
    if has_right_edge {
        for y in r.top()..r.bottom() {
            buf[(r.right() - 1, y)].set_bg(edge);
        }
    }
    if r.height >= 3 {
        for x in r.left()..r.right() {
            buf[(x, r.bottom() - 1)].set_bg(edge);
        }
    }

    let margin = u16::from(r.width >= 4);
    let x = r.x + margin;
    let width = r.width - margin - u16::from(has_right_edge);
    if width == 0 {
        return;
    }
    let label = if bold { style.add_modifier(Modifier::BOLD) } else { style };
    let size = fmt::size(disk);
    let fits = |s: &str| Span::raw(s).width() <= width as usize;
    // A name cut to a letter or two is noise; a cut size is misleading.
    let name_ok = fits(name) || width as usize > MIN_LABEL;
    if r.height >= 2 && !header_only {
        if name_ok {
            put(buf, x, r.y, name, width, label);
        }
        if fits(&size) {
            put(buf, x, r.y + 1, &size, width, style);
        }
    } else {
        let one_line = format!("{name} {size}");
        if fits(&one_line) {
            put(buf, x, r.y, &one_line, width, label);
        } else if name_ok {
            put(buf, x, r.y, name, width, label);
        }
    }
}

/// Writes `text` truncated to `width` columns, ending in "…" if cut.
fn put(buf: &mut Buffer, x: u16, y: u16, text: &str, width: u16, style: Style) {
    if Span::raw(text).width() <= width as usize {
        buf.set_stringn(x, y, text, width as usize, style);
    } else {
        let (end, _) = buf.set_stringn(x, y, text, width as usize - 1, style);
        buf.set_stringn(end, y, "…", 1, style);
    }
}
