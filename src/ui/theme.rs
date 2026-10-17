use ratatui::style::{Color, Modifier, Style};

use crate::filetype::Category;

pub type Rgb = (u8, u8, u8);

/// Base tile colour per category. Neighbouring tiles of the same category get
/// lighter or darker shades of it (see `tile_bg`).
fn base(cat: Category) -> Rgb {
    match cat {
        Category::Dir => (52, 92, 158),
        Category::Media => (150, 62, 120),
        Category::Archive => (172, 76, 52),
        Category::Code => (56, 128, 72),
        Category::Document => (150, 126, 36),
        Category::Build => (114, 90, 72),
        Category::Data => (32, 124, 138),
        Category::Other => (96, 96, 102),
    }
}
const OTHERS: Rgb = (58, 58, 62);
pub const SELECTED: Rgb = (236, 190, 84);

pub const ACCENT: Color = Color::Rgb(SELECTED.0, SELECTED.1, SELECTED.2);
pub const DIR_FG: Color = Color::Rgb(110, 160, 235);
pub const ERROR_FG: Color = Color::Rgb(230, 100, 90);
pub const LIVE_FG: Color = Color::Rgb(110, 190, 120);

fn rgb((r, g, b): Rgb) -> Color {
    Color::Rgb(r, g, b)
}

/// Background for a tile of category `cat` (`None` for the merged "others"
/// tile) given its shade index.
pub fn tile_bg(cat: Option<Category>, index: usize) -> Rgb {
    const MIX: [f32; 5] = [0.0, -0.2, 0.15, -0.34, 0.28];
    match cat {
        Some(c) => mix(base(c), MIX[index % MIX.len()]),
        None => OTHERS,
    }
}

/// A category's colour for use as text or a swatch on the terminal background.
pub fn swatch(cat: Category) -> Color {
    rgb(mix(base(cat), 0.2))
}

/// Mixes toward white for positive `t`, toward black for negative.
fn mix(c: Rgb, t: f32) -> Rgb {
    let (target, t) = if t >= 0.0 { (255.0, t) } else { (0.0, -t) };
    let m = |v: u8| (v as f32 + (target - v as f32) * t).round() as u8;
    (m(c.0), m(c.1), m(c.2))
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

/// Shades of a directory tile's colour for directories nested inside it,
/// all distinct from the tile itself.
pub fn inner_shade(base: Rgb, index: usize) -> Rgb {
    const MIX: [f32; 4] = [0.18, -0.22, 0.32, -0.38];
    mix(base, MIX[index % MIX.len()])
}
