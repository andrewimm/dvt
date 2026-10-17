use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Cell, Paragraph, Row};

use super::app::App;
use super::list_view::{body_height, name_line, render_table, scroll};
use super::{theme, tilde};
use crate::fmt;

pub fn render(f: &mut Frame, area: Rect, app: &mut App) {
    app.search.area = area;
    let tree = &app.tree;
    let s = &mut app.search;
    if s.results.is_empty() {
        let under = tilde(&tree.path(s.under));
        let text = if s.query.is_empty() {
            format!("Type to search names under {under}\nsubstring, or a glob like *.iso or node_mod*")
        } else {
            format!("No matches under {under}")
        };
        let y = area.y + area.height / 2;
        f.render_widget(
            Paragraph::new(text).style(theme::dim()).alignment(Alignment::Center),
            Rect::new(area.x, y.saturating_sub(1), area.width, 2.min(area.height)),
        );
        return;
    }

    let base = tree.path(s.under);
    let total = tree.get(s.under).disk.max(1);
    let visible = scroll(&mut s.offset, Some(s.sel), s.results.len(), body_height(area));
    let rows = s.results[visible.clone()].iter().map(|&id| {
        let n = tree.get(id);
        let path = tree.path(id);
        let rel = path.strip_prefix(&base).unwrap_or(&path).to_string_lossy().into_owned();
        let items = if n.is_dir() { fmt::count(n.items as u64) } else { String::new() };
        Row::new([
            Cell::from(Line::from(fmt::size(n.disk)).right_aligned()),
            Cell::from(Line::from(format!("{:.1}%", n.disk as f64 * 100.0 / total as f64)).right_aligned()),
            Cell::from(Line::from(items).right_aligned()),
            Cell::from(name_line(tree, id, rel)),
        ])
    });
    let header = Row::new(["Size", "%", "Items", "Path"].map(|h| Cell::from(h).style(theme::dim())));
    let widths = [Constraint::Length(7), Constraint::Length(6), Constraint::Length(10), Constraint::Min(10)];
    render_table(f, area, rows, header, &widths, Some(s.sel - visible.start));
}
