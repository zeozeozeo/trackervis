use font8x8::{BASIC_FONTS, UnicodeFonts};
use vello::kurbo::{Affine, BezPath, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};
use vello::{AaConfig, Scene};

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

#[derive(Debug, Default)]
pub struct RenderScratch {
    trace_path: BezPath,
    trace_path_capacity: usize,
    trace_samples: Vec<f32>,
}

impl RenderScratch {
    pub fn new() -> Self {
        Self::default()
    }

    fn ensure_scope_capacity(&mut self, points: usize) {
        let points = points.max(2);
        if self.trace_samples.capacity() < points {
            self.trace_samples = Vec::with_capacity(points);
        }
        if self.trace_path_capacity < points {
            self.trace_path = BezPath::with_capacity(points);
            self.trace_path_capacity = points;
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct LayoutGrid {
    edges: Vec<[f32; 4]>,
}

const BACKGROUND: [u8; 4] = [0, 0, 0, 255];
const GRID: [u8; 4] = [255, 255, 255, 255];
const CROSSHAIR: [u8; 4] = [112, 112, 112, 255];
const TRACE: [u8; 4] = [255, 255, 255, 255];
const TEXT: [u8; 4] = [255, 255, 255, 255];

pub fn render_to_scene(scene: &mut Scene, frame: &FrameView<'_>, scratch: &mut RenderScratch) {
    scene.reset();
    let background = Rect::new(0.0, 0.0, frame.width as f64, frame.height as f64);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        color(BACKGROUND),
        None,
        &background,
    );

    let channel_count = frame.module.channels.len();
    let layout_count = channel_count.max(1);
    let cols = ((layout_count as f64 * frame.width as f64 / frame.height as f64)
        .sqrt()
        .ceil() as usize)
        .max(1);
    let rows = layout_count.div_ceil(cols);
    let base_cols_per_row = layout_count / rows;
    let extra = layout_count % rows;
    scratch.ensure_scope_capacity(frame.width.max(32) as usize);

    let mut channel_index = 0usize;
    for row in 0..rows {
        let cols_in_row = base_cols_per_row + usize::from(row < extra);
        if cols_in_row == 0 {
            continue;
        }

        let y0 = axis_bound(frame.height, rows, row);
        let y1 = axis_bound(frame.height, rows, row + 1);
        for col in 0..cols_in_row {
            let x0 = axis_bound(frame.width, cols_in_row, col);
            let x1 = axis_bound(frame.width, cols_in_row, col + 1);
            let cell = LayoutCell {
                rect: [x0, y0, x1, y1],
            };
            if let Some(samples) = frame.module.channels.get(channel_index) {
                draw_cell_scene(scene, frame, &cell, samples, channel_index, scratch);
                channel_index += 1;
            }
        }
    }

    for row in 0..rows {
        let cols_in_row = base_cols_per_row + usize::from(row < extra);
        if cols_in_row == 0 {
            continue;
        }

        let y0 = axis_bound(frame.height, rows, row);
        let y1 = axis_bound(frame.height, rows, row + 1);
        if row + 1 < rows {
            let rect = edge_rect([0.5, y1 - 0.5, frame.width as f32 - 0.5, y1 - 0.5]);
            scene.fill(Fill::NonZero, Affine::IDENTITY, color(GRID), None, &rect);
        }

        for col in 0..cols_in_row {
            let x0 = axis_bound(frame.width, cols_in_row, col);
            if col > 0 {
                let rect = edge_rect([x0 - 0.5, y0 - 0.5, x0 - 0.5, y1 - 0.5]);
                scene.fill(Fill::NonZero, Affine::IDENTITY, color(GRID), None, &rect);
            }
        }
    }

    if let Some(song_info) = frame.module.song_info.filter(|text| !text.is_empty()) {
        draw_song_info_scene(scene, frame.width, frame.height, song_info, TEXT);
    }
}

pub fn aa_config() -> AaConfig {
    AaConfig::Area
}

fn draw_cell_scene(
    scene: &mut Scene,
    frame: &FrameView<'_>,
    cell: &LayoutCell,
    samples: &[f32],
    index: usize,
    scratch: &mut RenderScratch,
) {
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
    render_scope_trace_scene(
        scene,
        samples,
        cursor_samples,
        frame.module.sample_rate,
        frame.max_history_samples,
        cell,
        inner_width,
        scratch,
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

fn render_scope_trace_scene(
    scene: &mut Scene,
    samples: &[f32],
    cursor_samples: usize,
    sample_rate: u32,
    max_history_samples: usize,
    cell: &LayoutCell,
    inner_width: usize,
    scratch: &mut RenderScratch,
) {
    let [x0, y0, x1, y1] = cell.rect;
    let inner_x0 = x0 + 4.0;
    let inner_x1 = x1 - 4.0;
    let center = (y0 + y1) * 0.5;
    let amplitude = ((y1 - y0) * 0.42).max(1.0);
    let cursor = cursor_samples.min(samples.len());
    scratch.trace_path.truncate(0);

    if cursor < 2 || inner_width < 2 || sample_rate == 0 {
        scratch.trace_path.move_to(Point::new(inner_x0 as f64, center as f64));
        scratch
            .trace_path
            .line_to(Point::new(inner_x1 as f64, center as f64));
        scene.stroke(
            &Stroke::new(1.5),
            Affine::IDENTITY,
            color(TRACE),
            None,
            &scratch.trace_path,
        );
        return;
    }

    let lookback = max_history_samples.min(cursor).max(128.min(cursor));
    let analysis_start = cursor.saturating_sub(lookback);
    let analysis = &samples[analysis_start..cursor];
    let dc = mean(analysis);
    let (period, last_trigger) = estimate_period(analysis, dc, sample_rate);
    let default_span = ((sample_rate as f32 * 0.020) as usize).max(inner_width * 2);
    let max_span = lookback.max(default_span);
    let min_span = (inner_width * 2).min(max_span);
    let span = period
        .map(|period| (period.saturating_mul(3)).clamp(min_span, max_span))
        .unwrap_or(default_span.clamp(min_span, max_span))
        .min(cursor.max(1));

    let end = match (last_trigger, period) {
        (Some(last_trigger), Some(period)) if period > 0 => {
            let anchor = analysis_start + last_trigger;
            let periods_since = (cursor - anchor) / period;
            let aligned = anchor + periods_since.saturating_mul(period);
            aligned.max(span).min(cursor)
        }
        _ => cursor,
    };
    let start = end.saturating_sub(span);
    let denominator = (inner_width - 1).max(1) as f32;
    scratch.trace_samples.clear();
    let mut max_amp = 0.0f32;
    let span_minus_one = span.saturating_sub(1) as f32;
    for x in 0..inner_width {
        let position = start as f32 + span_minus_one * (x as f32 / denominator);
        let sample = sample_at(samples, position);
        max_amp = max_amp.max(sample.abs());
        scratch.trace_samples.push(sample);
    }

    let gain = if max_amp > 0.0005 {
        (0.92 / max_amp).clamp(0.2, 8.0)
    } else {
        0.0
    };

    if let Some((&first, rest)) = scratch.trace_samples.split_first() {
        let y = center - (first * gain).clamp(-1.0, 1.0) * amplitude;
        scratch.trace_path.move_to(Point::new(inner_x0 as f64, y as f64));
        for (offset, sample) in rest.iter().enumerate() {
            let x = inner_x0 + (inner_x1 - inner_x0) * ((offset + 1) as f32 / denominator);
            let y = center - (*sample * gain).clamp(-1.0, 1.0) * amplitude;
            scratch.trace_path.line_to(Point::new(x as f64, y as f64));
        }
    }

    scene.stroke(
        &Stroke::new(1.5),
        Affine::IDENTITY,
        color(TRACE),
        None,
        &scratch.trace_path,
    );
}

#[cfg(test)]
fn layout_grid(frame: &FrameView<'_>) -> LayoutGrid {
    let count = frame.module.channels.len().max(1);
    let cols = ((count as f64 * frame.width as f64 / frame.height as f64)
        .sqrt()
        .ceil() as usize)
        .max(1);
    let rows = count.div_ceil(cols);
    let base_cols_per_row = count / rows;
    let extra = count % rows;
    let mut edges = Vec::with_capacity(count.saturating_sub(1));

    for row in 0..rows {
        let cols_in_row = base_cols_per_row + usize::from(row < extra);
        if cols_in_row == 0 {
            continue;
        }

        let y0 = axis_bound(frame.height, rows, row);
        let y1 = axis_bound(frame.height, rows, row + 1);
        let vertical_top = if y0 <= 0.0 { 0.5 } else { y0 - 0.5 };
        let vertical_bottom = y1 - 0.5;
        if row + 1 < rows {
            edges.push([0.5, vertical_bottom, frame.width as f32 - 0.5, vertical_bottom]);
        }

        for col in 0..cols_in_row {
            let x0 = axis_bound(frame.width, cols_in_row, col);
            if col > 0 {
                let x = x0 - 0.5;
                edges.push([x, vertical_top, x, vertical_bottom]);
            }
        }
    }

    LayoutGrid { edges }
}

fn draw_pan_marker_scene(scene: &mut Scene, cell: &LayoutCell, pan: f32) {
    let marker = pan_marker_rect(cell, pan);
    let rect = Rect::new(
        marker[0] as f64,
        marker[1] as f64,
        marker[2] as f64,
        marker[3] as f64,
    );
    scene.fill(Fill::NonZero, Affine::IDENTITY, color(TRACE), None, &rect);
}

fn draw_text_scene(scene: &mut Scene, cell: &LayoutCell, text: &str, rgba: [u8; 4]) {
    draw_bitmap_text_scene(scene, cell, text, rgba, text_origin(cell));
}

fn draw_bottom_text_scene(scene: &mut Scene, cell: &LayoutCell, text: &str, rgba: [u8; 4]) {
    let origin = bottom_text_origin(cell);
    draw_bitmap_text_scene(scene, cell, text, rgba, origin);
}

fn draw_song_info_scene(scene: &mut Scene, width: u32, height: u32, text: &str, rgba: [u8; 4]) {
    let origin = viewport_bottom_right_text_origin(width, height, text);
    draw_bitmap_text_scene_unbounded(scene, text, rgba, origin);
}

fn text_origin(cell: &LayoutCell) -> (f32, f32) {
    (cell.rect[0] + 6.0, cell.rect[1] + 6.0)
}

fn bottom_text_origin(cell: &LayoutCell) -> (f32, f32) {
    (cell.rect[0] + 6.0, cell.rect[3] - 14.0)
}

fn viewport_bottom_right_text_origin(width: u32, height: u32, text: &str) -> (f32, f32) {
    let text_width = text.chars().count() as f32 * 8.0;
    let x = (width as f32 - 8.0 - text_width).max(8.0);
    let y = height as f32 - 14.0;
    (x, y)
}

fn pan_marker_rect(cell: &LayoutCell, pan: f32) -> [f32; 4] {
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

fn axis_bound(length: u32, parts: usize, index: usize) -> f32 {
    ((length as f64 * index as f64 / parts as f64).round() as u32).min(length) as f32
}

fn vertical_crosshair_rect(cell: &LayoutCell) -> [f32; 4] {
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

fn estimate_period(
    analysis: &[f32],
    dc: f32,
    sample_rate: u32,
) -> (Option<usize>, Option<usize>) {
    if analysis.len() < 3 {
        return (None, None);
    }

    let min_period = (sample_rate as usize / 4_000).max(4);
    let max_period = (sample_rate as usize / 32).max(min_period + 1);
    let min_slope = 0.0005f32;
    let mut last_trigger = None;
    let mut periods = [0usize; 6];
    let mut period_count = 0usize;

    for index in 1..analysis.len() {
        let prev = analysis[index - 1] - dc;
        let curr = analysis[index] - dc;
        if prev <= 0.0 && curr > 0.0 && (curr - prev) >= min_slope {
            if let Some(prev_trigger) = last_trigger {
                let period = index - prev_trigger;
                if (min_period..=max_period).contains(&period) {
                    if period_count < periods.len() {
                        periods[period_count] = period;
                        period_count += 1;
                    } else {
                        periods.copy_within(1.., 0);
                        periods[periods.len() - 1] = period;
                    }
                }
            }
            last_trigger = Some(index);
        }
    }

    let period = if period_count == 0 {
        None
    } else {
        periods[..period_count].sort_unstable();
        Some(periods[period_count / 2])
    };

    (period, last_trigger)
}

fn mean(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        0.0
    } else {
        samples.iter().copied().sum::<f32>() / samples.len() as f32
    }
}

fn sample_at(samples: &[f32], position: f32) -> f32 {
    let left = position.floor() as usize;
    let right = position.ceil() as usize;
    if right >= samples.len() {
        return *samples.last().unwrap_or(&0.0);
    }
    if left == right {
        return samples[left];
    }
    let mix = position - left as f32;
    samples[left] * (1.0 - mix) + samples[right] * mix
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
    cell: &LayoutCell,
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
