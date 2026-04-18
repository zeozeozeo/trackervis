use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub struct SampleHistory {
    samples: VecDeque<f32>,
    max_len: usize,
}

impl SampleHistory {
    pub fn new(max_len: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(max_len),
            max_len: max_len.max(1),
        }
    }

    pub fn push(&mut self, sample: f32) {
        if self.samples.len() == self.max_len {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    pub fn push_mono_stereo_frames(&mut self, interleaved_stereo: &[f32]) {
        for frame in interleaved_stereo.chunks_exact(2) {
            self.push((frame[0] + frame[1]) * 0.5);
        }
    }

    pub fn snapshot(&self) -> Vec<f32> {
        self.samples.iter().copied().collect()
    }
}

#[derive(Debug, Clone)]
pub struct ScopeTrace {
    pub points: Vec<(f32, f32)>,
}

impl ScopeTrace {
    pub fn from_samples(
        samples: &[f32],
        cursor_samples: usize,
        sample_rate: u32,
        width: usize,
        max_lookback_samples: usize,
    ) -> Self {
        let cursor = cursor_samples.min(samples.len());
        if cursor < 2 || width < 2 || sample_rate == 0 {
            return Self {
                points: vec![(0.0, 0.0), (1.0, 0.0)],
            };
        }

        let lookback = max_lookback_samples.min(cursor).max(128.min(cursor));
        let analysis_start = cursor.saturating_sub(lookback);
        let analysis = &samples[analysis_start..cursor];
        let dc = mean(analysis);
        let trigger_indices = rising_zero_crossings(analysis, dc);
        let period = estimate_period_samples(&trigger_indices, sample_rate);
        let default_span = ((sample_rate as f32 * 0.020) as usize).max(width * 2);
        let max_span = lookback.max(default_span);
        let min_span = (width * 2).min(max_span);

        let span = period
            .map(|period| (period.saturating_mul(3)).clamp(min_span, max_span))
            .unwrap_or(default_span.clamp(min_span, max_span))
            .min(cursor.max(1));

        let end = match (trigger_indices.last().copied(), period) {
            (Some(last_trigger), Some(period)) if period > 0 => {
                let anchor = analysis_start + last_trigger;
                let periods_since = (cursor - anchor) / period;
                let aligned = anchor + periods_since.saturating_mul(period);
                aligned.max(span).min(cursor)
            }
            _ => cursor,
        };
        let start = end.saturating_sub(span);
        let denominator = (width - 1).max(1) as f32;
        let mut points = Vec::with_capacity(width);
        for x in 0..width {
            let position =
                start as f32 + (span.saturating_sub(1) as f32) * (x as f32 / denominator);
            let sample = sample_at(samples, position);
            points.push((x as f32 / denominator, sample));
        }

        if let Some(max_amp) = points
            .iter()
            .map(|(_, sample)| sample.abs())
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        {
            if max_amp > 0.0005 {
                let gain = (0.92 / max_amp).clamp(0.2, 8.0);
                for (_, sample) in &mut points {
                    *sample = (*sample * gain).clamp(-1.0, 1.0);
                }
            } else {
                for (_, sample) in &mut points {
                    *sample = 0.0;
                }
            }
        }

        Self { points }
    }
}

fn mean(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        0.0
    } else {
        samples.iter().copied().sum::<f32>() / samples.len() as f32
    }
}

fn rising_zero_crossings(samples: &[f32], dc: f32) -> Vec<usize> {
    let mut indices = Vec::new();
    let min_slope = 0.0005f32;
    for index in 1..samples.len() {
        let prev = samples[index - 1] - dc;
        let curr = samples[index] - dc;
        if prev <= 0.0 && curr > 0.0 && (curr - prev) >= min_slope {
            indices.push(index);
        }
    }
    indices
}

fn estimate_period_samples(trigger_indices: &[usize], sample_rate: u32) -> Option<usize> {
    if trigger_indices.len() < 3 {
        return None;
    }

    let min_period = (sample_rate as usize / 4_000).max(4);
    let max_period = (sample_rate as usize / 32).max(min_period + 1);
    let mut periods = Vec::new();
    for pair in trigger_indices.windows(2).rev().take(6) {
        let period = pair[1].saturating_sub(pair[0]);
        if (min_period..=max_period).contains(&period) {
            periods.push(period);
        }
    }

    if periods.is_empty() {
        None
    } else {
        periods.sort_unstable();
        Some(periods[periods.len() / 2])
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
