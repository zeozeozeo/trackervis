use std::collections::HashSet;

use font8x8::{BASIC_FONTS, UnicodeFonts};
use vello::kurbo::{Affine, BezPath, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};
use vello::{AaConfig, Scene};

use crate::oscilloscope::ScopeTrace;

#[derive(Debug, Clone)]
pub struct FrameModule<'a> {
    pub local_time_seconds: f64,
    pub sample_rate: u32,
    pub channels: &'a [Vec<f32>],
    pub channel_panning: Option<&'a [f32]>,
    pub channel_labels: Option<&'a [String]>,
    pub channel_effects: Option<&'a [String]>,
    pub song_info: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct FrameView<'a> {
    pub width: u32,
    pub height: u32,
    pub max_history_samples: usize,
    pub module: FrameModule<'a>,
}

#[derive(Debug, Clone, Copy)]
struct LayoutCell {
    rect: [f32; 4],
}

#[derive(Debug, Clone)]
struct LayoutGrid {
    cells: Vec<LayoutCell>,
    edges: Vec<[f32; 4]>,
}

const BACKGROUND: [u8; 4] = [0, 0, 0, 255];
const GRID: [u8; 4] = [255, 255, 255, 255];
const CROSSHAIR: [u8; 4] = [112, 112, 112, 255];
const TRACE: [u8; 4] = [255, 255, 255, 255];
const TEXT: [u8; 4] = [255, 255, 255, 255];

pub fn render_to_scene(scene: &mut Scene, frame: &FrameView<'_>) {
    scene.reset();
    let background = Rect::new(0.0, 0.0, frame.width as f64, frame.height as f64);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        color(BACKGROUND),
        None,
        &background,
    );

    let layout = layout_grid(frame);
    stroke_grid(scene, &layout, GRID);

    for (index, (cell, samples)) in layout
        .cells
        .into_iter()
        .zip(frame.module.channels.iter())
        .enumerate()
    {
        let [x0, _y0, x1, _y1] = cell.rect;
        let crosshair = vertical_crosshair_rect(cell);
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            color(CROSSHAIR),
            None,
            &Rect::new(
                crosshair[0] as f64,
                crosshair[1] as f64,
                crosshair[2] as f64,
                crosshair[3] as f64,
            ),
        );

        let inner_width = ((x1 - x0) - 8.0).max(32.0) as usize;
        let cursor_samples = ((frame.module.local_time_seconds * frame.module.sample_rate as f64)
            .round() as usize)
            .min(samples.len());
        let trace = ScopeTrace::from_samples(
            samples,
            cursor_samples,
            frame.module.sample_rate,
            inner_width,
            frame.max_history_samples,
        );
        let path = scope_path(&trace, cell);
        scene.stroke(
            &Stroke::new(1.5),
            Affine::IDENTITY,
            color(TRACE),
            None,
            &path,
        );

        if let Some(pan) = frame
            .module
            .channel_panning
            .and_then(|panning| panning.get(index))
            .copied()
        {
            draw_pan_marker_scene(scene, cell, pan);
        }

        if let Some(label) = frame
            .module
            .channel_labels
            .and_then(|labels| labels.get(index))
            .filter(|label| !label.is_empty())
            .filter(|_| channel_is_active(samples, cursor_samples))
        {
            draw_text_scene(scene, cell, label, TEXT);
        }

        if let Some(effect) = frame
            .module
            .channel_effects
            .and_then(|effects| effects.get(index))
            .filter(|effect| !effect.is_empty())
        {
            draw_bottom_text_scene(scene, cell, effect, TEXT);
        }
    }

    if let Some(song_info) = frame.module.song_info.filter(|text| !text.is_empty()) {
        draw_song_info_scene(scene, frame.width, frame.height, song_info, TEXT);
    }
}

pub fn aa_config() -> AaConfig {
    AaConfig::Area
}

fn layout_grid(frame: &FrameView<'_>) -> LayoutGrid {
    let count = frame.module.channels.len().max(1);
    let cols = ((count as f64 * frame.width as f64 / frame.height as f64)
        .sqrt()
        .ceil() as usize)
        .max(1);
    let rows = count.div_ceil(cols);
    let base_cols_per_row = count / rows;
    let extra = count % rows;
    let mut cells = Vec::with_capacity(count);

    let row_bounds = axis_bounds(frame.height, rows);
    for row in 0..rows {
        let cols_in_row = base_cols_per_row + usize::from(row < extra);
        if cols_in_row == 0 {
            continue;
        }

        let y0 = row_bounds[row];
        let y1 = row_bounds[row + 1];
        let col_bounds = axis_bounds(frame.width, cols_in_row);
        for col in 0..cols_in_row {
            let x0 = col_bounds[col];
            let x1 = col_bounds[col + 1];
            cells.push(LayoutCell {
                rect: [x0, y0, x1, y1],
            });
        }
    }

    let mut edge_set = HashSet::new();
    let mut edges = Vec::new();
    for cell in &cells {
        for edge in cell_edges(*cell) {
            let key = edge_key(edge);
            if edge_set.insert(key) {
                edges.push(edge);
            }
        }
    }

    LayoutGrid { cells, edges }
}

fn scope_path(trace: &ScopeTrace, cell: LayoutCell) -> BezPath {
    let [x0, y0, x1, y1] = cell.rect;
    let inner_x0 = x0 + 4.0;
    let inner_x1 = x1 - 4.0;
    let center = (y0 + y1) * 0.5;
    let amplitude = ((y1 - y0) * 0.42).max(1.0);
    let mut path = BezPath::new();

    for (index, &(nx, sample)) in trace.points.iter().enumerate() {
        let x = inner_x0 + (inner_x1 - inner_x0) * nx.clamp(0.0, 1.0);
        let y = center - sample * amplitude;
        if index == 0 {
            path.move_to(Point::new(x as f64, y as f64));
        } else {
            path.line_to(Point::new(x as f64, y as f64));
        }
    }

    path
}

fn stroke_grid(scene: &mut Scene, layout: &LayoutGrid, rgba: [u8; 4]) {
    for &[x0, y0, x1, y1] in &layout.edges {
        let rect = edge_rect([x0, y0, x1, y1]);
        scene.fill(Fill::NonZero, Affine::IDENTITY, color(rgba), None, &rect);
    }
}

fn draw_pan_marker_scene(scene: &mut Scene, cell: LayoutCell, pan: f32) {
    let marker = pan_marker_rect(cell, pan);
    let rect = Rect::new(
        marker[0] as f64,
        marker[1] as f64,
        marker[2] as f64,
        marker[3] as f64,
    );
    scene.fill(Fill::NonZero, Affine::IDENTITY, color(TRACE), None, &rect);
}

fn draw_text_scene(scene: &mut Scene, cell: LayoutCell, text: &str, rgba: [u8; 4]) {
    draw_bitmap_text_scene(scene, cell, text, rgba, text_origin(cell));
}

fn draw_bottom_text_scene(scene: &mut Scene, cell: LayoutCell, text: &str, rgba: [u8; 4]) {
    let origin = bottom_text_origin(cell);
    draw_bitmap_text_scene(scene, cell, text, rgba, origin);
}

fn draw_song_info_scene(scene: &mut Scene, width: u32, height: u32, text: &str, rgba: [u8; 4]) {
    let origin = viewport_bottom_right_text_origin(width, height, text);
    draw_bitmap_text_scene_unbounded(scene, text, rgba, origin);
}

fn text_origin(cell: LayoutCell) -> (f32, f32) {
    (cell.rect[0] + 6.0, cell.rect[1] + 6.0)
}

fn bottom_text_origin(cell: LayoutCell) -> (f32, f32) {
    (cell.rect[0] + 6.0, cell.rect[3] - 14.0)
}

fn viewport_bottom_right_text_origin(width: u32, height: u32, text: &str) -> (f32, f32) {
    let text_width = text.chars().count() as f32 * 8.0;
    let x = (width as f32 - 8.0 - text_width).max(8.0);
    let y = height as f32 - 14.0;
    (x, y)
}

fn pan_marker_rect(cell: LayoutCell, pan: f32) -> [f32; 4] {
    let [x0, _y0, x1, y1] = cell.rect;
    let marker = 6.0f32;
    let margin = 8.0f32;
    let track_left = x0 + margin;
    let track_right = x1 - margin - marker;
    let x = track_left + (track_right - track_left).max(0.0) * ((pan + 1.0) * 0.5);
    let y = y1 - margin - marker;
    [
        x.round(),
        y.round(),
        (x + marker).round(),
        (y + marker).round(),
    ]
}

fn axis_bounds(length: u32, parts: usize) -> Vec<f32> {
    (0..=parts)
        .map(|index| {
            ((length as f64 * index as f64 / parts as f64).round() as u32).min(length) as f32
        })
        .collect()
}

fn cell_edges(cell: LayoutCell) -> [[f32; 4]; 4] {
    let [x0, y0, x1, y1] = cell.rect;
    let left = if x0 <= 0.0 { 0.5 } else { x0 - 0.5 };
    let top = if y0 <= 0.0 { 0.5 } else { y0 - 0.5 };
    let right = x1 - 0.5;
    let bottom = y1 - 0.5;
    [
        [left, top, right, top],
        [left, bottom, right, bottom],
        [left, top, left, bottom],
        [right, top, right, bottom],
    ]
}

fn vertical_crosshair_rect(cell: LayoutCell) -> [f32; 4] {
    let [x0, y0, x1, y1] = cell.rect;
    let center = ((x0 + x1) * 0.5).floor().clamp(x0, (x1 - 1.0).max(x0));
    [center, y0, center + 1.0, y1]
}

fn edge_rect(edge: [f32; 4]) -> Rect {
    let [x0, y0, x1, y1] = edge_pixel_rect(edge);
    Rect::new(x0 as f64, y0 as f64, x1 as f64, y1 as f64)
}

fn edge_pixel_rect(edge: [f32; 4]) -> [f32; 4] {
    if (edge[1] - edge[3]).abs() < 0.25 {
        let x0 = (edge[0] - 0.5).round().max(0.0);
        let x1 = (edge[2] + 0.5).round().max(x0 + 1.0);
        let y = (edge[1] - 0.5).round().max(0.0);
        [x0, y, x1, y + 1.0]
    } else {
        let x = (edge[0] - 0.5).round().max(0.0);
        let y0 = (edge[1] - 0.5).round().max(0.0);
        let y1 = (edge[3] + 0.5).round().max(y0 + 1.0);
        [x, y0, x + 1.0, y1]
    }
}

fn edge_key(edge: [f32; 4]) -> (i32, i32, i32, i32) {
    (
        (edge[0] * 2.0).round() as i32,
        (edge[1] * 2.0).round() as i32,
        (edge[2] * 2.0).round() as i32,
        (edge[3] * 2.0).round() as i32,
    )
}

fn channel_is_active(samples: &[f32], cursor_samples: usize) -> bool {
    let cursor = cursor_samples.min(samples.len());
    if cursor == 0 {
        return false;
    }
    let start = cursor.saturating_sub(1024);
    let peak = samples[start..cursor]
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    peak > 0.003
}

fn draw_bitmap_text_scene(
    scene: &mut Scene,
    cell: LayoutCell,
    text: &str,
    rgba: [u8; 4],
    origin: (f32, f32),
) {
    let scale = 1.0f32;
    for (index, ch) in text.chars().enumerate() {
        let glyph_x = origin.0 + index as f32 * 8.0 * scale;
        if glyph_x + 8.0 * scale > cell.rect[2] - 6.0 {
            break;
        }
        let Some(bitmap) = BASIC_FONTS.get(ch) else {
            continue;
        };
        for (row, bits) in bitmap.iter().enumerate() {
            for col in 0..8 {
                if (bits >> col) & 1 == 0 {
                    continue;
                }
                let x = glyph_x + col as f32 * scale;
                let y = origin.1 + row as f32 * scale;
                let rect = Rect::new(x as f64, y as f64, (x + scale) as f64, (y + scale) as f64);
                scene.fill(Fill::NonZero, Affine::IDENTITY, color(rgba), None, &rect);
            }
        }
    }
}

fn draw_bitmap_text_scene_unbounded(
    scene: &mut Scene,
    text: &str,
    rgba: [u8; 4],
    origin: (f32, f32),
) {
    let scale = 1.0f32;
    for (index, ch) in text.chars().enumerate() {
        let glyph_x = origin.0 + index as f32 * 8.0 * scale;
        let Some(bitmap) = BASIC_FONTS.get(ch) else {
            continue;
        };
        for (row, bits) in bitmap.iter().enumerate() {
            for col in 0..8 {
                if (bits >> col) & 1 == 0 {
                    continue;
                }
                let x = glyph_x + col as f32 * scale;
                let y = origin.1 + row as f32 * scale;
                let rect = Rect::new(x as f64, y as f64, (x + scale) as f64, (y + scale) as f64);
                scene.fill(Fill::NonZero, Affine::IDENTITY, color(rgba), None, &rect);
            }
        }
    }
}

fn color(rgba: [u8; 4]) -> Color {
    Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3])
}
