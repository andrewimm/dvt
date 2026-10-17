use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::Paragraph;

use super::app::{App, Tile, TileItem};
use super::theme;
use crate::fmt;
use crate::layout::treemap::{FRect, squarify};
use crate::tree::Kind;

/// Entries smaller than this many cells are merged into one "others" tile.
const MIN_TILE_CELLS: f64 = 20.0;
/// Don't draw a name unless at least this many of its columns fit.
const MIN_LABEL: usize = 3;
const MAX_TILES: usize = 400;
/// Terminal cells are roughly twice as tall as they are wide.
const CELL_ASPECT: f64 = 2.0;

pub fn render(f: &mut Frame, area: Rect, app: &mut App) {
    app.tiles = layout(app, area);
    if app.tiles.is_empty() {
        let text = if app.scan_elapsed().is_some() { "scanning…" } else { "(empty)" };
        let y = area.y + area.height / 2;
        f.render_widget(
            Paragraph::new(text).style(theme::dim()).alignment(Alignment::Center),
            Rect::new(area.x, y, area.width, 1),
        );
        return;
    }
    let colors = assign_colors(app);
    let buf = f.buffer_mut();
    for (tile, &color) in app.tiles.iter().zip(&colors) {
        draw_tile(buf, app, tile, color);
    }
}

/// Greedy colouring: each tile takes the first palette shade that none of its
/// already-coloured neighbours use.
fn assign_colors(app: &App) -> Vec<usize> {
    let tiles = &app.tiles;
    let mut colors: Vec<usize> = Vec::with_capacity(tiles.len());
    for (i, t) in tiles.iter().enumerate() {
        let kind = kind_of(app, t.item);
        let taken: Vec<usize> = tiles[..i]
            .iter()
            .zip(&colors)
            .filter(|(o, _)| theme::same_palette(kind_of(app, o.item), kind) && touching(o.rect, t.rect))
            .map(|(_, &c)| c)
            .collect();
        colors.push((0..).find(|c| !taken.contains(c)).unwrap());
    }
    colors
}

fn kind_of(app: &App, item: TileItem) -> Option<Kind> {
    match item {
        TileItem::Node(id) => Some(app.tree.get(id).kind),
        TileItem::Others { .. } => None,
    }
}

fn touching(a: Rect, b: Rect) -> bool {
    let overlap = |a0: u16, a1: u16, b0: u16, b1: u16| a0.max(b0) < a1.min(b1);
    ((a.right() == b.x || b.right() == a.x) && overlap(a.y, a.bottom(), b.y, b.bottom()))
        || ((a.bottom() == b.y || b.bottom() == a.y) && overlap(a.x, a.right(), b.x, b.right()))
}

fn layout(app: &App, area: Rect) -> Vec<Tile> {
    let tree = &app.tree;
    let total: u64 = app.children.iter().map(|&id| tree.get(id).disk).sum();
    if total == 0 || area.is_empty() {
        return Vec::new();
    }
    let cells = area.width as f64 * area.height as f64;

    let mut items = Vec::new();
    let mut values = Vec::new();
    for (i, &id) in app.children.iter().enumerate() {
        let disk = tree.get(id).disk;
        let small = (disk as f64 / total as f64) * cells < MIN_TILE_CELLS;
        if small || i == MAX_TILES {
            let rest = &app.children[i..];
            let disk: u64 = rest.iter().map(|&id| tree.get(id).disk).sum();
            if disk > 0 {
                items.push(TileItem::Others { first: i, count: rest.len(), disk });
                values.push(disk as f64);
            }
            break;
        }
        items.push(TileItem::Node(id));
        values.push(disk as f64);
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

fn draw_tile(buf: &mut Buffer, app: &App, tile: &Tile, color: usize) {
    let tree = &app.tree;
    let r = tile.rect;
    let selected = app.tile_selected(&tile.item);
    let (kind, name, disk) = match tile.item {
        TileItem::Node(id) => {
            let n = tree.get(id);
            let mut name = tree.name(id).to_string_lossy().into_owned();
            if n.is_dir() {
                name.push('/');
            }
            (Some(n.kind), name, n.disk)
        }
        TileItem::Others { count, disk, .. } => (None, format!("… {} more", fmt::count(count as u64)), disk),
    };

    let bg = theme::tile_bg(kind, color, selected);
    let style = theme::tile_style(bg);
    buf.set_style(r, style);
    for y in r.top()..r.bottom() {
        for x in r.left()..r.right() {
            buf[(x, y)].set_char(' ');
        }
    }
    // A darker right and bottom edge separates neighbouring tiles.
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
    let mut label = style;
    if kind == Some(Kind::Dir) || selected {
        label = label.add_modifier(Modifier::BOLD);
    }
    let size = fmt::size(disk);
    let fits = |s: &str| Span::raw(s).width() <= width as usize;
    // A name cut to a letter or two is noise; a cut size is misleading.
    let name_ok = fits(&name) || width as usize > MIN_LABEL;
    if r.height >= 2 {
        if name_ok {
            put(buf, x, r.y, &name, width, label);
        }
        if fits(&size) {
            put(buf, x, r.y + 1, &size, width, style);
        }
    } else {
        let one_line = format!("{name} {size}");
        if fits(&one_line) {
            put(buf, x, r.y, &one_line, width, label);
        } else if name_ok {
            put(buf, x, r.y, &name, width, label);
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
