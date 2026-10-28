//! Broadcast input conditioning and matched output compensation.
//! Peak decisions use whole input blocks; output activity updates after each block.
//! Scaling is internal to inference and is undone using each output hop's input gain.
use crate::audio::HOP;
use std::collections::VecDeque;

pub const BLOCK_HOPS: usize = 4;
pub const BLOCK_SAMPLES: usize = BLOCK_HOPS * HOP;
const PEAK_CANDIDATES: usize = 4;
const LONG_PEAK_BLOCKS: usize = 60_000;
const RECENT_PEAK_BLOCKS: usize = 8_000;
const ACTIVITY_HOPS: usize = 15;
const ACTIVITY_THRESHOLD: f32 = 0.0001;
const QUIET_PEAK: f32 = 0.075;
const INFERENCE_PEAK: f32 = 0.3;

#[derive(Clone)]
struct Peak {
    value: f32,
    remaining: usize,
}
struct Peaks {
    window: usize,
    entries: Vec<Peak>,
}
impl Peaks {
    fn new(window: usize) -> Self {
        Self {
            window,
            entries: Vec::with_capacity(PEAK_CANDIDATES),
        }
    }
    fn update(&mut self, value: f32) -> f32 {
        let minimum = self
            .entries
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.value.total_cmp(&b.1.value))
            .map(|(index, _)| index);
        if self.entries.len() < PEAK_CANDIDATES {
            self.entries.push(Peak {
                value,
                remaining: self.window,
            });
        } else if let Some(index) = minimum
            && value > self.entries[index].value
        {
            self.entries.remove(index);
            self.entries.push(Peak {
                value,
                remaining: self.window,
            });
        }
        self.entries.retain(|peak| peak.remaining != 0);
        for peak in &mut self.entries {
            peak.remaining -= 1;
        }
        self.maximum()
    }
    fn maximum(&self) -> f32 {
        self.entries
            .iter()
            .map(|peak| peak.value)
            .reduce(f32::max)
            .unwrap_or(1.0)
    }
}

pub struct Conditioner {
    long: Peaks,
    recent: Peaks,
    active: bool,
    gate: bool,
    activity: VecDeque<f32>,
    sum: f32,
    pending_activity: Vec<f32>,
}
impl Default for Conditioner {
    fn default() -> Self {
        Self {
            long: Peaks::new(LONG_PEAK_BLOCKS),
            recent: Peaks::new(RECENT_PEAK_BLOCKS),
            active: false,
            gate: false,
            activity: vec![0.0; ACTIVITY_HOPS].into(),
            sum: 0.0,
            pending_activity: Vec::with_capacity(BLOCK_HOPS),
        }
    }
}
impl Conditioner {
    pub fn begin(&mut self, block: &mut [f32]) -> f32 {
        let peak = block.iter().map(|value| value.abs()).fold(0.0, f32::max);
        let mut maximum = self.long.update(peak);
        let recent = self.recent.update(peak);

        if !self.active && self.gate && (maximum < QUIET_PEAK || recent < QUIET_PEAK) {
            self.active = true;
            if maximum > QUIET_PEAK {
                self.long.entries.clone_from(&self.recent.entries);
                maximum = self.long.maximum();
            }
        } else if self.active && maximum >= QUIET_PEAK {
            self.active = false;
        }
        if maximum.abs() <= f32::EPSILON {
            self.active = false;
        }

        let gain = if self.active {
            INFERENCE_PEAK / maximum
        } else {
            1.0
        };
        let compensation = if self.active {
            maximum / INFERENCE_PEAK
        } else {
            1.0
        };
        for value in block {
            *value *= gain;
        }
        compensation
    }
    pub fn observe(&mut self, output: &[f32]) {
        self.pending_activity
            .push(output.iter().map(|value| value.abs()).sum());
    }
    pub fn end(&mut self) {
        for value in self.pending_activity.drain(..) {
            self.sum -= self.activity.pop_front().unwrap_or(0.0);
            self.activity.push_back(value);
            self.sum += value;
            self.gate = self.sum / (ACTIVITY_HOPS * HOP) as f32 > ACTIVITY_THRESHOLD;
            if !self.gate {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn peak_candidates_expire_and_keep_copied_lifetimes() {
        let mut peaks = Peaks::new(PEAK_CANDIDATES);
        for value in [1.0, 2.0, 3.0, 4.0] {
            peaks.update(value);
        }
        for _ in 0..PEAK_CANDIDATES - 1 {
            assert_eq!(peaks.update(0.0), 4.0);
        }
        assert_eq!(peaks.update(0.0), 0.0);
        let mut short = Peaks::new(2);
        short.update(2.0);
        peaks.entries.clone_from(&short.entries);
        assert_eq!(peaks.update(0.0), 2.0);
        assert_eq!(peaks.update(0.0), 0.0);
    }
    #[test]
    fn quiet_scaling_waits_for_output_activity_and_stops_for_loud_input() {
        let mut conditioner = Conditioner::default();
        let original = vec![0.01; BLOCK_SAMPLES];
        let mut block = original.clone();
        assert_eq!(conditioner.begin(&mut block), 1.0);
        conditioner.observe(&vec![0.01; HOP]);
        conditioner.end();
        let gain = conditioner.begin(&mut block);
        assert!(gain < 1.0);
        assert!(
            block
                .iter()
                .all(|value| (*value - INFERENCE_PEAK).abs() < 1e-6)
        );
        assert!(
            block
                .iter()
                .zip(original)
                .all(|(value, raw)| (*value * gain - raw).abs() < 1e-7)
        );
        assert_eq!(conditioner.begin(&mut vec![0.5; BLOCK_SAMPLES]), 1.0);
    }
}
