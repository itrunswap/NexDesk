use std::{env, fmt::Write as _, io, str::FromStr};

use base64::{Engine, engine::general_purpose::STANDARD};
use nexdesk_media::PixelFrame;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Backend {
    Ansi,
    Kitty,
    Sixel,
}

impl Backend {
    pub(crate) fn detect() -> Self {
        let term = env::var("TERM").unwrap_or_default();
        if term.starts_with("tmux") || term.starts_with("screen") || env::var_os("TMUX").is_some() {
            Self::Ansi
        } else if term == "xterm-kitty" || env::var_os("KITTY_WINDOW_ID").is_some() {
            Self::Kitty
        } else if term.contains("sixel") || term.starts_with("mlterm") {
            Self::Sixel
        } else {
            Self::Ansi
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Ansi => "ANSI",
            Self::Kitty => "Kitty",
            Self::Sixel => "Sixel",
        }
    }

    pub(crate) fn rate(self) -> u64 {
        match self {
            Self::Ansi => 12,
            Self::Kitty => 15,
            Self::Sixel => 8,
        }
    }
}

impl FromStr for Backend {
    type Err = io::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "auto" => Ok(Self::detect()),
            "ansi" => Ok(Self::Ansi),
            "kitty" => Ok(Self::Kitty),
            "sixel" => Ok(Self::Sixel),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unknown terminal renderer",
            )),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Viewport {
    zoom: f64,
    center_x: f64,
    center_y: f64,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            center_x: 0.5,
            center_y: 0.5,
        }
    }
}

impl Viewport {
    pub(crate) fn zoom(&mut self, value: f64) {
        self.zoom = value.clamp(1.0, 8.0);
        self.clamp();
    }

    pub(crate) fn pan(&mut self, x: f64, y: f64) {
        self.center_x += x / self.zoom;
        self.center_y += y / self.zoom;
        self.clamp();
    }

    fn clamp(&mut self) {
        let half = 0.5 / self.zoom;
        self.center_x = self.center_x.clamp(half, 1.0 - half);
        self.center_y = self.center_y.clamp(half, 1.0 - half);
    }

    fn layout(self, width: u32, height: u32, cols: u16, rows: u16) -> Layout {
        let ratio = f64::from(width) / f64::from(height) * 2.0;
        let width = f64::from(cols).min(f64::from(rows) * ratio);
        let height = f64::from(rows).min(f64::from(cols) / ratio);
        let cols = width.round().max(1.0) as u16;
        let rows = height.round().max(1.0) as u16;
        let extent = 1.0 / self.zoom;
        Layout {
            cols,
            rows,
            extent,
            left: self.center_x - extent / 2.0,
            top: self.center_y - extent / 2.0,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Layout {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    extent: f64,
    left: f64,
    top: f64,
}

impl Layout {
    pub(crate) fn point(self, column: u16, row: u16) -> Option<(i32, i32)> {
        if column >= self.cols || row == 0 || row > self.rows {
            return None;
        }
        let (x, y) = self.source(f64::from(column) + 0.5, f64::from(row) - 0.5);
        Some(((x * 65535.0).round() as i32, (y * 65535.0).round() as i32))
    }

    fn source(self, column: f64, row: f64) -> (f64, f64) {
        let x = self.left + column / f64::from(self.cols) * self.extent;
        let y = self.top + row / f64::from(self.rows) * self.extent;
        (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0))
    }

    fn rgb(self, frame: &PixelFrame, column: f64, row: f64) -> [u8; 3] {
        let (x, y) = self.source(column, row);
        let x = (x * f64::from(frame.width)).floor() as u32;
        let y = (y * f64::from(frame.height)).floor() as u32;
        let index = (y.min(frame.height - 1) * frame.width + x.min(frame.width - 1)) as usize * 4;
        [
            frame.rgba[index],
            frame.rgba[index + 1],
            frame.rgba[index + 2],
        ]
    }
}

pub(crate) struct Renderer {
    backend: Backend,
    previous: Vec<String>,
    truecolor: bool,
}

impl Renderer {
    pub(crate) fn new(backend: Backend) -> Self {
        let truecolor = env::var("COLORTERM")
            .is_ok_and(|value| matches!(value.as_str(), "truecolor" | "24bit"))
            || env::var_os("WT_SESSION").is_some();
        Self {
            backend,
            previous: Vec::new(),
            truecolor,
        }
    }

    pub(crate) fn draw(
        &mut self,
        frame: &PixelFrame,
        viewport: Viewport,
        cols: u16,
        rows: u16,
        out: &mut impl io::Write,
    ) -> io::Result<Layout> {
        let layout = viewport.layout(
            frame.width,
            frame.height,
            cols.saturating_sub(1).clamp(1, 120),
            rows.saturating_sub(3).clamp(1, 44),
        );
        match self.backend {
            Backend::Ansi => {
                let lines = ansi(frame, layout, self.truecolor);
                for (row, line) in lines.iter().enumerate() {
                    if self.previous.get(row) != Some(line) {
                        write!(out, "\x1b[{};1H\x1b[2K{}", row + 2, line)?;
                    }
                }
                for row in lines.len()..self.previous.len() {
                    write!(out, "\x1b[{};1H\x1b[2K", row + 2)?;
                }
                self.previous = lines;
            }
            Backend::Kitty => kitty(frame, layout, out)?,
            Backend::Sixel => sixel(frame, layout, out)?,
        }
        Ok(layout)
    }

    pub(crate) fn clear(&mut self, out: &mut impl io::Write) -> io::Result<()> {
        if self.backend == Backend::Kitty {
            out.write_all(b"\x1b_Ga=d,d=i,i=9284,q=2;\x1b\\")?;
        }
        out.write_all(b"\x1b[2J")?;
        self.previous.clear();
        Ok(())
    }
}

pub(crate) fn bridge_image(
    frame: &PixelFrame,
    viewport: Viewport,
    cols: u16,
    rows: u16,
) -> (Vec<String>, Layout) {
    let layout = viewport.layout(
        frame.width,
        frame.height,
        cols.clamp(1, 100),
        rows.clamp(1, 30),
    );
    (ansi(frame, layout, true), layout)
}

fn ansi(frame: &PixelFrame, layout: Layout, truecolor: bool) -> Vec<String> {
    (0..layout.rows)
        .map(|y| {
            let mut row = String::with_capacity(usize::from(layout.cols) * 24);
            let mut foreground = None;
            let mut background = None;
            for x in 0..layout.cols {
                let top = layout.rgb(frame, f64::from(x) + 0.5, f64::from(y) + 0.25);
                let bottom = layout.rgb(frame, f64::from(x) + 0.5, f64::from(y) + 0.75);
                set_color(&mut row, top, &mut foreground, 38, truecolor);
                set_color(&mut row, bottom, &mut background, 48, truecolor);
                row.push('▀');
            }
            row.push_str("\x1b[0m");
            row
        })
        .collect()
}

fn set_color(
    out: &mut String,
    rgb: [u8; 3],
    last: &mut Option<[u8; 3]>,
    mode: u8,
    truecolor: bool,
) {
    let color = if truecolor {
        rgb
    } else {
        rgb.map(|channel| ((u16::from(channel) * 5 + 127) / 255) as u8)
    };
    if *last == Some(color) {
        return;
    }
    let [r, g, b] = color;
    if truecolor {
        let _ = write!(out, "\x1b[{mode};2;{r};{g};{b}m");
    } else {
        let index = 16 + r * 36 + g * 6 + b;
        let _ = write!(out, "\x1b[{mode};5;{index}m");
    }
    *last = Some(color);
}

fn image(
    frame: &PixelFrame,
    layout: Layout,
    cell_width: u32,
    cell_height: u32,
) -> (u32, u32, Vec<u8>) {
    let width = u32::from(layout.cols) * cell_width;
    let height = u32::from(layout.rows) * cell_height;
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            rgb.extend(layout.rgb(
                frame,
                (f64::from(x) + 0.5) / f64::from(cell_width),
                (f64::from(y) + 0.5) / f64::from(cell_height),
            ));
        }
    }
    (width, height, rgb)
}

fn kitty(frame: &PixelFrame, layout: Layout, out: &mut impl io::Write) -> io::Result<()> {
    let (width, height, pixels) = image(frame, layout, 8, 16);
    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    encoder.write_header()?.write_image_data(&pixels)?;
    let encoded = STANDARD.encode(png);
    let chunks = encoded.as_bytes().chunks(4096);
    let count = chunks.len();
    out.write_all(b"\x1b_Ga=d,d=i,i=9284,q=2;\x1b\\\x1b[2;1H")?;
    for (index, chunk) in chunks.enumerate() {
        let more = u8::from(index + 1 < count);
        if index == 0 {
            write!(
                out,
                "\x1b_Ga=T,f=100,i=9284,C=1,q=2,c={},r={},m={more};",
                layout.cols, layout.rows
            )?;
        } else {
            write!(out, "\x1b_Gm={more};")?;
        }
        out.write_all(chunk)?;
        out.write_all(b"\x1b\\")?;
    }
    Ok(())
}

fn sixel(frame: &PixelFrame, layout: Layout, out: &mut impl io::Write) -> io::Result<()> {
    let (cell_width, cell_height) = crossterm::terminal::window_size()
        .ok()
        .and_then(|window| {
            let width = u32::from(window.width).checked_div(u32::from(window.columns))?;
            let height = u32::from(window.height).checked_div(u32::from(window.rows))?;
            (width > 0 && height > 0).then_some((width.clamp(4, 20), height.clamp(8, 40)))
        })
        .unwrap_or((8, 16));
    let (width, height, rgb) = image(frame, layout, cell_width, cell_height);
    let indexed: Vec<_> = rgb
        .chunks_exact(3)
        .map(|p| ((p[0] >> 6) << 4) | ((p[1] >> 6) << 2) | (p[2] >> 6))
        .collect();
    let mut data = String::new();
    let _ = write!(data, "\x1b[2;1H\x1bP0;0;0q\"1;1;{width};{height}");
    for palette in 0..64 {
        let _ = write!(
            data,
            "#{palette};2;{};{};{}",
            (palette >> 4) * 100 / 3,
            ((palette >> 2) & 3) * 100 / 3,
            (palette & 3) * 100 / 3
        );
    }
    let width = width as usize;
    for first in (0..height as usize).step_by(6) {
        let last = (first + 6).min(height as usize);
        let mut masks = vec![vec![0u8; width]; 64];
        for y in first..last {
            for (x, &color) in indexed[y * width..(y + 1) * width].iter().enumerate() {
                masks[color as usize][x] |= 1 << (y - first);
            }
        }
        for (color, row) in masks.iter().enumerate() {
            let Some(end) = row.iter().rposition(|&mask| mask != 0) else {
                continue;
            };
            let _ = write!(data, "#{color}");
            for run in row[..=end].chunk_by(|a, b| a == b) {
                let ch = char::from(63 + run[0]);
                if run.len() >= 4 {
                    let _ = write!(data, "!{}{ch}", run.len());
                } else {
                    data.extend(std::iter::repeat_n(ch, run.len()));
                }
            }
            data.push('$');
        }
        data.push('-');
    }
    data.push_str("\x1b\\");
    out.write_all(data.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_and_pan_map_to_the_same_source_pixel() {
        let mut viewport = Viewport::default();
        let original = viewport.layout(1920, 1080, 100, 30);
        let initial = original.point(50, 15).expect("interior point");
        viewport.zoom(2.0);
        let zoomed = viewport.layout(1920, 1080, 100, 30);
        let center = zoomed.point(50, 15).expect("interior point");
        assert!((initial.0 - center.0).abs() < 350);
        viewport.pan(10.0, 0.0);
        let right = viewport.layout(1920, 1080, 100, 30);
        assert!(right.point(50, 15).expect("panned point").0 > 48_000);
        assert_eq!(right.point(50, 0), None);
    }
}
