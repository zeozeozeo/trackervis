#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

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
