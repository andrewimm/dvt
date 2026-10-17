use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Row, Table};

use super::app::App;
use super::theme;
use crate::fmt;
use crate::tree::{Kind, flags};

const BAR_WIDTH: usize = 20;

pub fn render(f: &mut Frame, area: Rect, app: &mut App) {
    app.list_area = area;
    let tree = &app.tree;
    let total = tree.get(app.cwd).disk.max(1);
    let largest = app.children.first().map_or(1, |&id| tree.get(id).disk.max(1));

    let rows = app.children.iter().map(|&id| {
        let n = tree.get(id);
        let mut name = tree.name(id).to_string_lossy().into_owned();
        let mut style = Style::new();
        match n.kind {
            Kind::Dir => {
                name.push('/');
                style = style.fg(theme::DIR_FG).add_modifier(Modifier::BOLD);
            }
            Kind::Symlink => name.push('@'),
            _ => {}
        }
        let mut name_cell = vec![Span::styled(name, style)];
        if n.has(flags::ERROR) {
            name_cell.insert(0, Span::styled("! ", Style::new().fg(theme::ERROR_FG)));
        }
        if n.has(flags::HARDLINK_DUP) {
            name_cell.push(Span::styled("  (hardlink, counted elsewhere)", theme::dim()));
        }
        if n.has(flags::OTHER_FS) {
            name_cell.push(Span::styled("  (other filesystem, skipped)", theme::dim()));
        }
        let items = if n.is_dir() { fmt::count(n.items as u64) } else { String::new() };
        Row::new([
            Cell::from(Line::from(fmt::size(n.disk)).right_aligned()),
            Cell::from(bar(n.disk as f64 / largest as f64)),
            Cell::from(Line::from(format!("{:.1}%", n.disk as f64 * 100.0 / total as f64)).right_aligned()),
            Cell::from(Line::from(items).right_aligned()),
            Cell::from(Line::from(name_cell)),
        ])
    });

    let header = Row::new(["Size", "", "%", "Items", "Name"].map(|h| Cell::from(h).style(theme::dim())));
    let table = Table::new(
        rows,
        [
            Constraint::Length(7),
            Constraint::Length(BAR_WIDTH as u16),
            Constraint::Length(6),
            Constraint::Length(10),
            Constraint::Min(10),
        ],
    )
    .header(header)
    .column_spacing(2)
    .row_highlight_style(theme::selected_row());

    app.list.select(app.selected_index());
    f.render_stateful_widget(table, area, &mut app.list);
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
