use std::num::NonZeroU16;

use libghostty_vt::RenderState;
use libghostty_vt::render::{CellIterator, CursorVisualStyle, RowIterator};
use libghostty_vt::screen::{CellContentTag, CellWide};
use libghostty_vt::style::{Palette, RgbColor, StyleColor, Underline};
use ratatui_core::buffer::{Buffer, Cell, CellDiffOption};
use ratatui_core::layout::Rect;
use ratatui_core::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthStr;

use crate::grid::Point;
use crate::term::{Error, Terminal, Vt};

const UNDERLINE_SHIFT: u16 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnderlineStyle {
    Single = 1,
    Double = 2,
    Curly = 3,
    Dotted = 4,
    Dashed = 5,
}

impl UnderlineStyle {
    pub fn of(modifier: Modifier) -> Option<Self> {
        if !modifier.contains(Modifier::UNDERLINED) {
            return None;
        }
        Some(match (modifier.bits() >> UNDERLINE_SHIFT) & 0x7 {
            2 => Self::Double,
            3 => Self::Curly,
            4 => Self::Dotted,
            5 => Self::Dashed,
            _ => Self::Single,
        })
    }

    fn mark(underline: Underline, modifier: Modifier) -> Modifier {
        let style = match underline {
            Underline::None => return modifier,
            Underline::Double => Self::Double,
            Underline::Curly => Self::Curly,
            Underline::Dotted => Self::Dotted,
            Underline::Dashed => Self::Dashed,
            _ => Self::Single,
        };
        Modifier::from_bits_retain(modifier.bits() | ((style as u16) << UNDERLINE_SHIFT))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Block,
    BlockHollow,
    Underline,
    Bar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub x: u16,
    pub y: u16,
    pub shape: CursorShape,
    pub blinking: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Highlight {
    pub start: Point,
    pub end: Point,
    pub style: Style,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RenderOptions<'a> {
    pub highlights: &'a [Highlight],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rendered {
    pub cursor: Option<Cursor>,
}

struct BaseColors {
    palette: Palette,
    fg: Option<RgbColor>,
    bg: Option<RgbColor>,
}

struct FrameCache {
    generation: u64,
    size: (u16, u16),
    cells: Vec<Cell>,
    cursor: Option<Cursor>,
}

pub(crate) struct Renderer {
    state: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    text: String,
    base: Option<BaseColors>,
    cache: Option<FrameCache>,
}

struct Colors {
    fg: Color,
    bg: Color,
    palette: Palette,
    changed: [bool; 256],
    custom_palette: bool,
}

impl Colors {
    fn new(vt: &Vt, base: &BaseColors) -> Self {
        let palette = vt
            .color_palette()
            .unwrap_or(Palette([RgbColor::default(); 256]));
        let custom_palette = palette.0 != base.palette.0;
        let mut changed = [false; 256];
        if custom_palette {
            for (i, flag) in changed.iter_mut().enumerate() {
                *flag = palette.0[i] != base.palette.0[i];
            }
        }
        let fixed = |current: Option<RgbColor>, default: Option<RgbColor>| match current {
            Some(color) if Some(color) != default => rgb(color),
            _ => Color::Reset,
        };
        Self {
            fg: fixed(vt.fg_color().ok().flatten(), base.fg),
            bg: fixed(vt.bg_color().ok().flatten(), base.bg),
            palette,
            changed,
            custom_palette,
        }
    }

    fn color(&self, color: StyleColor, default: Color) -> Color {
        match color {
            StyleColor::None => default,
            StyleColor::Rgb(value) => rgb(value),
            StyleColor::Palette(index) => self.indexed(index.0),
        }
    }

    fn indexed(&self, index: u8) -> Color {
        if self.custom_palette && self.changed[usize::from(index)] {
            rgb(self.palette.0[usize::from(index)])
        } else {
            Color::Indexed(index)
        }
    }
}

impl Renderer {
    pub(crate) fn new() -> Result<Self, Error> {
        Ok(Self {
            state: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            text: String::with_capacity(16),
            base: None,
            cache: None,
        })
    }

    pub(crate) fn cursor_style(&mut self, vt: &Vt) -> (CursorShape, bool) {
        let Ok(snapshot) = self.state.update(vt) else {
            return (CursorShape::Block, false);
        };
        (
            shape(snapshot.cursor_visual_style().ok()),
            snapshot.cursor_blinking().unwrap_or(false),
        )
    }

    fn draw_cached(
        &mut self,
        vt: &Vt,
        area: Rect,
        buf: &mut Buffer,
        scrolled: bool,
        generation: u64,
    ) -> Result<Option<Cursor>, Error> {
        let size = (area.width, area.height);
        if let Some(cache) = &self.cache
            && cache.generation == generation
            && cache.size == size
        {
            blit(&cache.cells, area, buf);
            return Ok(cache.cursor.map(|cursor| Cursor {
                x: area.x + cursor.x,
                y: area.y + cursor.y,
                ..cursor
            }));
        }
        let cursor = self.draw(vt, area, buf, scrolled)?;
        let relative = cursor.map(|c| Cursor {
            x: c.x - area.x,
            y: c.y - area.y,
            ..c
        });
        let total = usize::from(area.width) * usize::from(area.height);
        let mut cells = self
            .cache
            .take()
            .map_or_else(|| Vec::with_capacity(total), |c| c.cells);
        cells.clear();
        cells.reserve(total.saturating_sub(cells.capacity()));
        for row in 0..area.height {
            let y = area.y + row;
            for col in 0..area.width {
                cells.push(buf[(area.x + col, y)].clone());
            }
        }
        self.cache = Some(FrameCache {
            generation,
            size,
            cells,
            cursor: relative,
        });
        Ok(cursor)
    }

    fn draw(
        &mut self,
        vt: &Vt,
        area: Rect,
        buf: &mut Buffer,
        scrolled: bool,
    ) -> Result<Option<Cursor>, Error> {
        let base = self.base.get_or_insert_with(|| BaseColors {
            palette: vt
                .default_color_palette()
                .unwrap_or(Palette([RgbColor::default(); 256])),
            fg: vt.default_fg_color().ok().flatten(),
            bg: vt.default_bg_color().ok().flatten(),
        });
        let colors = Colors::new(vt, base);
        let Self {
            state,
            rows,
            cells,
            text,
            ..
        } = self;
        let snapshot = state.update(vt)?;
        let (cols, height) = (snapshot.cols()?, snapshot.rows()?);
        let width = cols.min(area.width);
        let mut row_iter = rows.update(&snapshot)?;
        let mut y = 0u16;
        while let Some(row) = row_iter.next() {
            if y >= height.min(area.height) {
                break;
            }
            let mut cell_iter = cells.update(row)?;
            let mut x = 0u16;
            while let Some(cell) = cell_iter.next() {
                if x >= width {
                    break;
                }
                let raw = cell.raw_cell()?;
                let vt_style = cell.style()?;
                let mut style = Style::new()
                    .fg(colors.color(vt_style.fg_color, colors.fg))
                    .bg(colors.color(vt_style.bg_color, colors.bg))
                    .underline_color(colors.color(vt_style.underline_color, Color::Reset));
                style = match raw.content_tag()? {
                    CellContentTag::BgColorPalette => {
                        style.bg(colors.indexed(raw.bg_color_palette()?.0))
                    }
                    CellContentTag::BgColorRgb => style.bg(rgb(raw.bg_color_rgb()?)),
                    _ => style,
                };
                style = style.add_modifier(vt_modifier(&vt_style));
                text.clear();
                cell.graphemes_utf8(text)?;
                let target = &mut buf[(area.x + x, area.y + y)];
                target.reset();
                target.set_style(style);
                match raw.wide()? {
                    CellWide::Wide if x + 1 < width => {
                        symbol(target, text, 2);
                    }
                    CellWide::Wide | CellWide::SpacerHead => {
                        target.set_symbol(" ");
                    }
                    CellWide::SpacerTail => {}
                    CellWide::Narrow => symbol(target, text, 1),
                }
                x += 1;
            }
            for blank in x..area.width {
                buf[(area.x + blank, area.y + y)].reset();
            }
            y += 1;
        }
        for row in y..area.height {
            for col in 0..area.width {
                buf[(area.x + col, area.y + row)].reset();
            }
        }
        if scrolled || !snapshot.cursor_visible()? {
            return Ok(None);
        }
        let shape = shape(snapshot.cursor_visual_style().ok());
        let blinking = snapshot.cursor_blinking().unwrap_or(false);
        Ok(snapshot
            .cursor_viewport()?
            .filter(|at| at.x < width && at.y < area.height.min(height))
            .map(|at| Cursor {
                x: area.x + at.x,
                y: area.y + at.y,
                shape,
                blinking,
            }))
    }
}

fn vt_modifier(vt_style: &libghostty_vt::style::Style) -> Modifier {
    let mut modifier = Modifier::empty();
    if vt_style.bold {
        modifier |= Modifier::BOLD;
    }
    if vt_style.faint {
        modifier |= Modifier::DIM;
    }
    if vt_style.italic {
        modifier |= Modifier::ITALIC;
    }
    if vt_style.underline != Underline::None {
        modifier |= Modifier::UNDERLINED;
    }
    if vt_style.blink {
        modifier |= Modifier::SLOW_BLINK;
    }
    if vt_style.inverse {
        modifier |= Modifier::REVERSED;
    }
    if vt_style.invisible {
        modifier |= Modifier::HIDDEN;
    }
    if vt_style.strikethrough {
        modifier |= Modifier::CROSSED_OUT;
    }
    UnderlineStyle::mark(vt_style.underline, modifier)
}

fn blit(cells: &[Cell], area: Rect, buf: &mut Buffer) {
    let width = usize::from(area.width);
    for row in 0..area.height {
        let y = area.y + row;
        let offset = usize::from(row) * width;
        for col in 0..area.width {
            buf[(area.x + col, y)] = cells[offset + usize::from(col)].clone();
        }
    }
}

fn symbol(target: &mut Cell, text: &str, width: u16) {
    if text.is_empty() || text.starts_with('\u{10EEEE}') {
        target.set_symbol(" ");
        return;
    }
    target.set_symbol(text);
    let bytes = text.as_bytes();
    if width == 1 && bytes.len() == 1 && (bytes[0].is_ascii_graphic() || bytes[0] == b' ') {
        return;
    }
    if text.width() != usize::from(width) {
        target.set_diff_option(CellDiffOption::ForcedWidth(
            NonZeroU16::new(width).unwrap_or(NonZeroU16::MIN),
        ));
    }
}

impl Terminal {
    pub fn render(&self, area: Rect, buf: &mut Buffer, options: &RenderOptions<'_>) -> Rendered {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return Rendered::default();
        }
        let scrolled = self.is_scrolled_back();
        let cursor = self
            .renderer()
            .borrow_mut()
            .draw_cached(self.vt(), area, buf, scrolled, self.render_gen())
            .ok()
            .flatten();
        let (cols, rows) = self.size();
        let (width, height) = (cols.min(area.width), rows.min(area.height));
        if width == 0 || height == 0 {
            return Rendered { cursor };
        }
        let top = self.viewport_top();
        let bottom = top + u64::from(height) - 1;
        for highlight in options.highlights {
            let (start, end) = if highlight.start <= highlight.end {
                (highlight.start, highlight.end)
            } else {
                (highlight.end, highlight.start)
            };
            for row in start.row.max(top)..=end.row.min(bottom) {
                let y = area.y + u16::try_from(row - top).unwrap_or(0);
                let from = if row == start.row { start.col } else { 0 };
                let to = if row == end.row { end.col } else { cols - 1 };
                for col in from..=to.min(width - 1) {
                    buf[(area.x + col, y)].set_style(highlight.style);
                }
            }
        }
        Rendered { cursor }
    }
}

fn rgb(color: RgbColor) -> Color {
    Color::Rgb(color.r, color.g, color.b)
}

fn shape(style: Option<CursorVisualStyle>) -> CursorShape {
    match style {
        Some(CursorVisualStyle::Bar) => CursorShape::Bar,
        Some(CursorVisualStyle::Underline) => CursorShape::Underline,
        Some(CursorVisualStyle::BlockHollow) => CursorShape::BlockHollow,
        _ => CursorShape::Block,
    }
}
