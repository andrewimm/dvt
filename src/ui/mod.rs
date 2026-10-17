mod app;
mod list_view;
mod theme;
mod treemap_view;

use std::io::stdout;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::{DefaultTerminal, Frame};

use crate::fmt;
use crate::scan;
use crate::tree::flags;
use app::{App, View};

/// Minimum time between frames while a scan is updating the tree.
const FRAME: Duration = Duration::from_millis(33);
/// Time per frame spent applying scanner messages.
const DRAIN_BUDGET: Duration = Duration::from_millis(20);

pub fn run(root: PathBuf, opts: scan::Options, live: bool) -> anyhow::Result<()> {
    let mut app = App::new(root, opts, live);
    let mut terminal = ratatui::init();
    execute!(stdout(), EnableMouseCapture)?;
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(stdout(), DisableMouseCapture);
        prev_hook(info);
    }));

    let result = event_loop(&mut terminal, &mut app);
    let _ = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> anyhow::Result<()> {
    let mut dirty = true;
    let mut last_draw = Instant::now() - FRAME;
    while !app.quit {
        let scanning = app.scan_elapsed().is_some();
        dirty |= app.drain(DRAIN_BUDGET);
        // While scanning, redraw on a fixed cadence to animate the spinner and
        // pick up new totals; otherwise only when something happened.
        if (dirty || scanning) && (!scanning || last_draw.elapsed() >= FRAME) {
            app.refresh();
            terminal.draw(|f| draw(f, app))?;
            last_draw = Instant::now();
            dirty = false;
        }
        let timeout = if scanning { Duration::from_millis(15) } else { Duration::from_millis(500) };
        if event::poll(timeout)? {
            loop {
                dirty |= app.handle(event::read()?);
                if !event::poll(Duration::ZERO)? {
                    break;
                }
            }
            // Keep input responsive: draw right away after an event.
            if dirty {
                app.refresh();
                terminal.draw(|f| draw(f, app))?;
                last_draw = Instant::now();
                dirty = false;
            }
        } else if !scanning {
            // Expire transient messages.
            dirty |= app.message().is_some();
        }
    }
    Ok(())
}

fn draw(f: &mut Frame, app: &mut App) {
    let [header, body, info, status] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1), Constraint::Length(1)])
            .areas(f.area());
    draw_header(f, header, app);
    match app.view {
        View::Treemap => treemap_view::render(f, body, app),
        View::List => list_view::render(f, body, app),
    }
    draw_info(f, info, app);
    draw_status(f, status, app);
    if app.show_help {
        draw_help(f, body);
    }
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let tree = &app.tree;
    let cwd = tree.get(app.cwd);
    let mut spans = vec![
        Span::styled(" dvt ", Style::new().bg(theme::ACCENT).fg(Color::Black).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(tilde(&tree.path(app.cwd)), Style::new().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(fmt::size(cwd.disk), Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD)),
    ];
    if app.cwd != tree.root() {
        let pct = cwd.disk as f64 * 100.0 / tree.get(tree.root()).disk.max(1) as f64;
        spans.push(Span::styled(format!("  {pct:.1}% of scan root"), theme::dim()));
    }
    spans.push(Span::styled(format!("  {} items", fmt::count(cwd.items as u64)), theme::dim()));
    if cwd.has(flags::ERROR) {
        spans.push(Span::styled("  ! could not read this directory", Style::new().fg(theme::ERROR_FG)));
    }
    let view = match app.view {
        View::Treemap => "treemap",
        View::List => "list",
    };
    let tag = format!(" [{view}] ");
    let [left, right] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(Span::raw(&tag).width() as u16)]).areas(area);
    f.render_widget(Line::from(spans), left);
    f.render_widget(Line::from(tag).right_aligned().style(theme::dim()), right);
}

/// Details of the selected entry, since small tiles can't show their own label.
fn draw_info(f: &mut Frame, area: Rect, app: &App) {
    let Some(id) = app.selected() else {
        return;
    };
    let tree = &app.tree;
    let n = tree.get(id);
    let total = tree.get(app.cwd).disk.max(1);
    let mut name = tree.name(id).to_string_lossy().into_owned();
    if n.is_dir() {
        name.push('/');
    }
    let mut spans = vec![
        Span::styled(" ▸ ", Style::new().fg(theme::ACCENT)),
        Span::styled(name, Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(format!("  {}  {:.1}%", fmt::size(n.disk), n.disk as f64 * 100.0 / total as f64)),
    ];
    if n.is_dir() {
        spans.push(Span::styled(format!("  {} items", fmt::count(n.items as u64)), theme::dim()));
    }
    if let Some(i) = app.selected_index() {
        spans.push(Span::styled(format!("  #{} of {}", i + 1, app.children.len()), theme::dim()));
    }
    f.render_widget(Line::from(spans), area);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let tree = &app.tree;
    let root = tree.get(tree.root());
    let mut spans = Vec::new();
    if let Some(msg) = app.message() {
        spans.push(Span::styled(format!(" {msg}"), Style::new().fg(theme::ACCENT)));
    } else if let Some(t) = app.scan_elapsed() {
        let frame = SPINNER[(t.as_millis() / 80) as usize % SPINNER.len()];
        spans.push(Span::styled(
            format!(" {frame} scanning  {} items  {}  {:.1}s", fmt::count(root.items as u64), fmt::size(root.disk), t.as_secs_f64()),
            Style::new().fg(theme::ACCENT),
        ));
    } else {
        let took = app.last_scan.map(|d| format!(" in {:.2}s", d.as_secs_f64())).unwrap_or_default();
        spans.push(Span::styled(format!(" {} items scanned{took}", fmt::count(root.items as u64)), theme::dim()));
    }
    match app.watched {
        Some((_, true)) => spans.push(Span::styled("  ● live", Style::new().fg(theme::LIVE_FG))),
        Some((_, false)) => spans.push(Span::styled("  ● live (top level only)", Style::new().fg(theme::LIVE_FG))),
        None => {}
    }
    if tree.errors > 0 {
        spans.push(Span::styled(format!("  {} unreadable", fmt::count(tree.errors as u64)), Style::new().fg(theme::ERROR_FG)));
    }
    let left = Line::from(spans);
    let hints = match app.view {
        View::Treemap => "←↓↑→ move  ⏎ open  ⌫ up  ⇥ list  ? help  q quit ",
        View::List => "↑↓ move  ⏎/→ open  ⌫/← up  ⇥ treemap  ? help  q quit ",
    };
    if left.width() + Span::raw(hints).width() + 2 <= area.width as usize {
        f.render_widget(Line::from(hints).right_aligned().style(theme::dim()), area);
    }
    f.render_widget(left, area);
}

fn draw_help(f: &mut Frame, area: Rect) {
    const KEYS: &[(&str, &str)] = &[
        ("←↓↑→ / hjkl", "move (spatially in the treemap)"),
        ("Enter / click", "open directory"),
        ("Backspace / u", "go up (past the start dir too)"),
        ("right click", "go up a directory"),
        ("Tab", "switch between treemap and list"),
        ("g / G, PgUp/PgDn", "jump in the list"),
        ("r", "rescan the selected directory"),
        ("R", "rescan the current directory"),
        ("n", "nested treemap: show inside dirs"),
        ("w", "live updates on/off"),
        ("y", "copy path to clipboard (OSC 52)"),
        ("q / Ctrl-C", "quit"),
    ];
    let lines: Vec<Line> = KEYS
        .iter()
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!(" {k:<18}"), Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD)),
                Span::raw(*v),
            ])
        })
        .collect();
    let w = 56.min(area.width);
    let h = (lines.len() as u16 + 2).min(area.height);
    let popup = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" dvt keys ").title_bottom(" any key to close ")),
        popup,
    );
}

/// Shortens paths under $HOME to `~/...`.
fn tilde(path: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() { "~".into() } else { format!("~/{}", rest.display()) };
    }
    path.display().to_string()
}
