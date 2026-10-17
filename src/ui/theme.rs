use ratatui::style::{Color, Modifier, Style};

use crate::tree::Kind;

pub type Rgb = (u8, u8, u8);

const DIRS: [Rgb; 6] = [
    (52, 92, 158),  // blue
    (38, 120, 112), // teal
    (96, 76, 150),  // purple
    (54, 118, 70),  // green
    (134, 72, 120), // plum
    (60, 106, 140), // steel
];
const FILES: [Rgb; 3] = [
    (118, 98, 70),  // khaki
    (96, 96, 100),  // grey
    (136, 86, 62),  // rust
];
const OTHERS: Rgb = (58, 58, 62);
pub const SELECTED: Rgb = (236, 190, 84);

pub const ACCENT: Color = Color::Rgb(SELECTED.0, SELECTED.1, SELECTED.2);
pub const DIR_FG: Color = Color::Rgb(110, 160, 235);
pub const ERROR_FG: Color = Color::Rgb(230, 100, 90);
pub const LIVE_FG: Color = Color::Rgb(110, 190, 120);

fn rgb((r, g, b): Rgb) -> Color {
    Color::Rgb(r, g, b)
}

/// Whether tiles of these kinds draw from the same palette, so neighbours
/// need different shades.
pub fn same_palette(a: Option<Kind>, b: Option<Kind>) -> bool {
    let group = |k: Option<Kind>| k.map(|k| k == Kind::Dir);
    group(a) == group(b)
}

/// Background for a tile given its shade index within its palette.
pub fn tile_bg(kind: Option<Kind>, index: usize) -> Rgb {
    match kind {
        Some(Kind::Dir) => DIRS[index % DIRS.len()],
        Some(_) => FILES[index % FILES.len()],
        None => OTHERS,
    }
}

pub fn tile_style(bg: Rgb) -> Style {
    let (r, g, b) = bg;
    let luma = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
    let fg = if luma > 150.0 { Color::Black } else { Color::White };
    Style::new().bg(rgb(bg)).fg(fg)
}

/// A darker shade of `bg`, used for the right and bottom edge of a tile.
pub fn edge(bg: Rgb) -> Color {
    let d = |c: u8| (c as u16 * 3 / 5) as u8;
    Color::Rgb(d(bg.0), d(bg.1), d(bg.2))
}

pub fn selected_row() -> Style {
    Style::new().bg(ACCENT).fg(Color::Black).add_modifier(Modifier::BOLD)
}

pub fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

/// Shades of a directory tile's colour for the tiles nested inside it.
pub fn inner_shade(base: Rgb, index: usize) -> Rgb {
    // Positive mixes toward white, negative toward black.
    const MIX: [f32; 4] = [0.18, -0.22, 0.32, -0.38];
    let t = MIX[index % MIX.len()];
    let (target, t) = if t >= 0.0 { (255.0, t) } else { (0.0, -t) };
    let mix = |c: u8| (c as f32 + (target - c as f32) * t).round() as u8;
    (mix(base.0), mix(base.1), mix(base.2))
}
