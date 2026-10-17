use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Row, Table, TableState};

use super::app::App;
use super::theme;
use crate::filetype::categorize;
use crate::fmt;
use crate::tree::{Kind, NodeId, Tree, flags};

const BAR_WIDTH: usize = 20;

pub fn render(f: &mut Frame, area: Rect, app: &mut App) {
    app.list_area = area;
    let tree = &app.tree;
    let total = tree.get(app.cwd).disk.max(1);
    let largest = app.children.first().map_or(1, |&id| tree.get(id).disk.max(1));
    let sel = app.selected_index();
    let visible = scroll(&mut app.list_offset, sel, app.children.len(), body_height(area));

    let rows = app.children[visible.clone()].iter().map(|&id| {
        let n = tree.get(id);
        let items = if n.is_dir() { fmt::count(n.items as u64) } else { String::new() };
        Row::new([
            Cell::from(Line::from(fmt::size(n.disk)).right_aligned()),
            Cell::from(bar(n.disk as f64 / largest as f64)),
            Cell::from(Line::from(format!("{:.1}%", n.disk as f64 * 100.0 / total as f64)).right_aligned()),
            Cell::from(Line::from(items).right_aligned()),
            Cell::from(name_line(tree, id, tree.name(id).to_string_lossy().into_owned())),
        ])
    });

    let header = Row::new(["Size", "", "%", "Items", "Name"].map(|h| Cell::from(h).style(theme::dim())));
    let widths = [
        Constraint::Length(7),
        Constraint::Length(BAR_WIDTH as u16),
        Constraint::Length(6),
        Constraint::Length(10),
        Constraint::Min(10),
    ];
    render_table(f, area, rows, header, &widths, sel.map(|i| i - visible.start));
}

/// Renders a table whose rows are already cut to the visible window, with the
/// window-relative `selected` row highlighted.
pub fn render_table<'a>(
    f: &mut Frame,
    area: Rect,
    rows: impl IntoIterator<Item = Row<'a>>,
    header: Row<'a>,
    widths: &[Constraint],
    selected: Option<usize>,
) {
    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(2)
        .row_highlight_style(theme::selected_row());
    let mut state = TableState::default().with_selected(selected);
    f.render_stateful_widget(table, area, &mut state);
}

/// Rows available below the header.
pub fn body_height(area: Rect) -> usize {
    area.height.saturating_sub(1) as usize
}

/// Adjusts `offset` so the selected row stays in view, and returns the range
/// of rows to draw. Only visible rows are built, so huge directories stay fast.
pub fn scroll(offset: &mut usize, selected: Option<usize>, len: usize, height: usize) -> std::ops::Range<usize> {
    let height = height.max(1);
    if let Some(sel) = selected {
        if sel < *offset {
            *offset = sel;
        } else if sel >= *offset + height {
            *offset = sel + 1 - height;
        }
    }
    *offset = (*offset).min(len.saturating_sub(height));
    *offset..(*offset + height).min(len)
}

/// An entry's name (or `label`, e.g. a relative path) with its type swatch,
/// a trailing `/` or `@`, and any flags.
pub fn name_line(tree: &Tree, id: NodeId, mut label: String) -> Line<'static> {
    let n = tree.get(id);
    let mut style = Style::new();
    match n.kind {
        Kind::Dir => {
            label.push('/');
            style = style.fg(theme::DIR_FG).add_modifier(Modifier::BOLD);
        }
        Kind::Symlink => label.push('@'),
        _ => {}
    }
    let swatch = theme::swatch(categorize(n.kind, tree.name(id)));
    let mut spans = vec![Span::styled("■ ", Style::new().fg(swatch))];
    if n.has(flags::ERROR) {
        spans.push(Span::styled("! ", Style::new().fg(theme::ERROR_FG)));
    }
    spans.push(Span::styled(label, style));
    if n.has(flags::HARDLINK_DUP) {
        spans.push(Span::styled("  (hardlink, counted elsewhere)", theme::dim()));
    }
    if n.has(flags::OTHER_FS) {
        spans.push(Span::styled("  (other filesystem, skipped)", theme::dim()));
    }
    Line::from(spans)
}

/// A bar of `BAR_WIDTH` cells filled to `frac`, with eighth-block precision.
fn bar(frac: f64) -> String {
    const PARTIAL: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    let eighths = (frac.clamp(0.0, 1.0) * (BAR_WIDTH * 8) as f64).round() as usize;
    let mut s: String = "█".repeat(eighths / 8);
    if !eighths.is_multiple_of(8) {
        s.push(PARTIAL[eighths % 8]);
    }
    s
}
