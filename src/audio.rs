//! Offline 48 kHz spectral adapter. The frame queue has constant memory usage.
use crate::{
    Error, Result,
    fft::{Complex, Fft},
    network::{CONTEXT, FEATURES, GAINS},
};
use std::collections::VecDeque;

pub const RATE: usize = 48000;
pub const TRANSFORM: usize = 1920;
pub const HOP: usize = 480;
const PREFIX: usize = TRANSFORM - HOP;

struct Frame {
    spectrum: Vec<Complex>,
    features: Vec<f32>,
}
pub struct Processor {
    fft: Fft,
    window: Vec<f32>,
    filters: Vec<Vec<(usize, f32)>>,
}
fn mel(f: f32) -> f32 {
    if f < 1000.0 {
        f / (200.0 / 3.0)
    } else {
        15.0 + (f / 1000.0).ln() / (6.4f32.ln() / 27.0)
    }
}
fn frequency(m: f32) -> f32 {
    if m < 15.0 {
        m * (200.0 / 3.0)
    } else {
        1000.0 * ((m - 15.0) * (6.4f32.ln() / 27.0)).exp()
    }
}
fn scale_power(power: f32) -> f32 {
    ((10.0 * power.max(2.0f32.powi(-24)).log10() + 40.0) / 88.0).clamp(0.0, 1.0)
}
pub fn validate_strength(strength: f32) -> Result<()> {
    if !strength.is_finite() || !(0.0..=1.0).contains(&strength) {
        return Err(Error::new(
            "Processing strength must be a number from 0 to 1",
        ));
    }
    Ok(())
}
impl Processor {
    pub fn new() -> Result<Self> {
        let window = (0..TRANSFORM)
            .map(|n| 0.54 - 0.46 * (std::f32::consts::TAU * n as f32 / TRANSFORM as f32).cos())
            .collect();
        let upper = mel(RATE as f32 / 2.0);
        let edges: Vec<_> = (0..FEATURES + 2)
            .map(|i| frequency(upper * i as f32 / (FEATURES + 1) as f32))
            .collect();
        let filters = (0..FEATURES)
            .map(|i| {
                let (left, center, right) = (edges[i], edges[i + 1], edges[i + 2]);
                (0..GAINS)
                    .filter_map(|bin| {
                        let f = bin as f32 * RATE as f32 / TRANSFORM as f32;
                        let weight = ((f - left) / (center - left))
                            .min((right - f) / (right - center))
                            .max(0.0)
                            * 2.0
                            / (right - left);
                        (weight > 0.0).then_some((bin, weight))
                    })
                    .collect()
            })
            .collect();
        Ok(Self {
            fft: Fft::new(TRANSFORM)?,
            window,
            filters,
        })
    }
    pub fn frame_count(samples: usize) -> usize {
        samples.div_ceil(HOP) + PREFIX / HOP
    }
    fn analyze(&self, samples: &[f32], frame: usize) -> Result<Frame> {
        let mut time = vec![Complex::default(); TRANSFORM];
        let start = frame as i64 * HOP as i64 - PREFIX as i64;
        for (i, value) in time.iter_mut().enumerate() {
            let pos = start + i as i64;
            if pos >= 0 && (pos as usize) < samples.len() {
                value.re = samples[pos as usize] * self.window[i];
            }
        }
        self.analyze_time(&time)
    }
    fn analyze_time(&self, time: &[Complex]) -> Result<Frame> {
        let spectrum = self.fft.transform(time, false)?;
        let power: Vec<_> = spectrum[..GAINS].iter().map(|v| v.power()).collect();
        let features = self
            .filters
            .iter()
            .map(|filter| scale_power(filter.iter().map(|(bin, w)| power[*bin] * w).sum()))
            .collect();
        Ok(Frame { spectrum, features })
    }
    fn synthesize(
        &self,
        spectrum: &[Complex],
        noise: &[f32],
        strength: f32,
    ) -> Result<Vec<Complex>> {
        if noise.len() != GAINS
            || noise
                .iter()
                .any(|n| !n.is_finite() || !(0.0..=1.0).contains(n))
        {
            return Err(Error::new(
                "Expected 961 finite noise-mask values from 0 to 1",
            ));
        }
        // Keep full suppression for the strongest predicted-noise bins. Ease
        // weaker bins with the selected strength to retain quieter speech detail.
        let top = (GAINS as f32 * 0.2 * strength) as usize;
        let threshold = if top == 0 {
            f32::MAX
        } else if strength == 1.0 {
            0.0
        } else {
            let mut power: Vec<f32> = spectrum[..GAINS]
                .iter()
                .zip(noise)
                .map(|(value, mask)| value.power() * mask)
                .collect();
            *power.select_nth_unstable_by(GAINS - top, f32::total_cmp).1
        };
        let gains: Vec<f32> = spectrum[..GAINS]
            .iter()
            .zip(noise)
            .map(|(value, mask)| {
                let factor = if value.power() * mask >= threshold {
                    1.0
                } else {
                    strength
                };
                1.0 - factor * mask
            })
            .collect();
        let mut spectrum = spectrum.to_vec();
        for (k, value) in spectrum.iter_mut().enumerate() {
            let bin = if k <= TRANSFORM / 2 { k } else { TRANSFORM - k };
            let gain = gains[bin];
            value.re *= gain;
            value.im *= gain;
        }
        self.fft.transform(&spectrum, true)
    }
    fn padded(&self, samples: &[f32], frame: i64, count: usize) -> Result<Frame> {
        if frame < 0 || frame as usize >= count {
            Ok(Frame {
                spectrum: Vec::new(),
                features: vec![0.0; FEATURES],
            })
        } else {
            self.analyze(samples, frame as usize)
        }
    }
    pub fn features(
        &self,
        samples: &[f32],
        mut emit: impl FnMut(&[f32]) -> Result<()>,
    ) -> Result<usize> {
        check_samples(samples)?;
        let count = Self::frame_count(samples.len());
        for frame in 0..count {
            emit(&self.analyze(samples, frame)?.features)?;
        }
        Ok(count)
    }
    pub fn process(
        &self,
        samples: &[f32],
        strength: f32,
        mut predict: impl FnMut(&[f32]) -> Result<Vec<f32>>,
    ) -> Result<Vec<f32>> {
        check_samples(samples)?;
        validate_strength(strength)?;
        let count = Self::frame_count(samples.len());
        let mut queue = VecDeque::with_capacity(CONTEXT);
        for i in -3..=3 {
            queue.push_back(self.padded(samples, i, count)?);
        }
        let mut output = vec![0.0; samples.len()];
        let mut normalization = vec![0.0; samples.len()];
        let mut context = Vec::with_capacity(CONTEXT * FEATURES);
        for frame in 0..count {
            context.clear();
            for entry in &queue {
                context.extend_from_slice(&entry.features);
            }
            let noise = predict(&context)?;
            let time = self.synthesize(&queue[3].spectrum, &noise, strength)?;
            let start = frame as i64 * HOP as i64 - PREFIX as i64;
            for (i, value) in time.iter().enumerate() {
                let pos = start + i as i64;
                if pos >= 0 && (pos as usize) < samples.len() {
                    let p = pos as usize;
                    output[p] += value.re * self.window[i];
                    normalization[p] += self.window[i] * self.window[i];
                }
            }
            queue.pop_front();
            if frame + 1 < count {
                queue.push_back(self.padded(samples, frame as i64 + 4, count)?);
            }
        }
        for (value, norm) in output.iter_mut().zip(normalization) {
            if norm <= 0.0 {
                return Err(Error::new("Audio reconstruction has an uncovered sample"));
            }
            *value /= norm;
            if !value.is_finite() {
                return Err(Error::new(
                    "Audio reconstruction produced a nonfinite sample",
                ));
            }
        }
        Ok(output)
    }
}

/// Incremental adapter with 480-sample hops and 2,880 samples of algorithmic delay.
/// Each push accepts at most one hop; finish flushes and crops the finite tail.
pub struct StreamProcessor {
    processor: Processor,
    history: VecDeque<f32>,
    pending: Vec<f32>,
    frames: VecDeque<Frame>,
    overlap: Vec<f32>,
    normalization: Vec<f32>,
    processed: u64,
    received: u64,
    emitted: u64,
    finished: bool,
}
impl StreamProcessor {
    pub fn new() -> Result<Self> {
        let frames = (0..3)
            .map(|_| Frame {
                spectrum: Vec::new(),
                features: vec![0.0; FEATURES],
            })
            .collect();
        Ok(Self {
            processor: Processor::new()?,
            history: vec![0.0; TRANSFORM].into(),
            pending: Vec::with_capacity(HOP * 2),
            frames,
            overlap: vec![0.0; TRANSFORM],
            normalization: vec![0.0; TRANSFORM],
            processed: 0,
            received: 0,
            emitted: 0,
            finished: false,
        })
    }
    pub fn samples(&self) -> u64 {
        self.received
    }
    pub fn push(
        &mut self,
        samples: &[f32],
        strength: f32,
        predict: &mut impl FnMut(&[f32]) -> Result<Vec<f32>>,
    ) -> Result<Vec<f32>> {
        if self.finished {
            return Err(Error::new("Audio stream is already finished"));
        }
        validate_strength(strength)?;
        if samples.len() > HOP || samples.iter().any(|v| !v.is_finite() || v.abs() > 16.0) {
            return Err(Error::new(
                "Stream chunks must contain at most 480 finite samples within ±16",
            ));
        }
        self.received = self
            .received
            .checked_add(samples.len() as u64)
            .ok_or_else(|| Error::new("Audio stream sample counter overflow"))?;
        self.pending.extend_from_slice(samples);
        if self.pending.len() < HOP {
            return Ok(Vec::new());
        }
        let hop: Vec<_> = self.pending.drain(..HOP).collect();
        let result = self.append_hop(&hop, strength, predict);
        if result.is_err() {
            self.finished = true;
        }
        result
    }
    pub fn finish(
        &mut self,
        strength: f32,
        predict: &mut impl FnMut(&[f32]) -> Result<Vec<f32>>,
    ) -> Result<Vec<f32>> {
        if self.finished {
            return Err(Error::new("Audio stream is already finished"));
        }
        validate_strength(strength)?;
        self.finished = true;
        if self.received == 0 {
            return Ok(Vec::new());
        }
        let mut output = Vec::with_capacity(HOP * 7);
        if !self.pending.is_empty() {
            let mut hop = std::mem::take(&mut self.pending);
            hop.resize(HOP, 0.0);
            output.extend(self.append_hop(&hop, strength, predict)?);
        }
        for _ in 0..3 {
            output.extend(self.append_hop(&[0.0; HOP], strength, predict)?);
        }
        for _ in 0..3 {
            output.extend(self.append_frame(
                Frame {
                    spectrum: Vec::new(),
                    features: vec![0.0; FEATURES],
                },
                strength,
                predict,
            )?);
        }
        if self.emitted != self.received {
            return Err(Error::new(
                "Audio stream flush produced an incorrect sample count",
            ));
        }
        Ok(output)
    }
    fn append_hop(
        &mut self,
        hop: &[f32],
        strength: f32,
        predict: &mut impl FnMut(&[f32]) -> Result<Vec<f32>>,
    ) -> Result<Vec<f32>> {
        self.history.drain(..HOP);
        self.history.extend(hop);
        let time: Vec<_> = self
            .history
            .iter()
            .zip(&self.processor.window)
            .map(|(value, window)| Complex {
                re: value * window,
                im: 0.0,
            })
            .collect();
        let frame = self.processor.analyze_time(&time)?;
        self.append_frame(frame, strength, predict)
    }
    fn append_frame(
        &mut self,
        frame: Frame,
        strength: f32,
        predict: &mut impl FnMut(&[f32]) -> Result<Vec<f32>>,
    ) -> Result<Vec<f32>> {
        self.frames.push_back(frame);
        if self.frames.len() < CONTEXT {
            return Ok(Vec::new());
        }
        let mut context = Vec::with_capacity(CONTEXT * FEATURES);
        for frame in &self.frames {
            context.extend_from_slice(&frame.features);
        }
        let noise = predict(&context)?;
        let time = self
            .processor
            .synthesize(&self.frames[3].spectrum, &noise, strength)?;
        for (i, sample) in time.iter().enumerate() {
            let w = self.processor.window[i];
            self.overlap[i] += sample.re * w;
            self.normalization[i] += w * w;
        }
        let mut output = Vec::with_capacity(HOP);
        if self.processed >= 3 {
            let count = (self.received - self.emitted).min(HOP as u64) as usize;
            for i in 0..count {
                let value = self.overlap[i] / self.normalization[i];
                if !value.is_finite() {
                    return Err(Error::new("Audio stream produced a nonfinite sample"));
                }
                output.push(value);
            }
            self.emitted += count as u64;
        }
        self.overlap.rotate_left(HOP);
        self.overlap[TRANSFORM - HOP..].fill(0.0);
        self.normalization.rotate_left(HOP);
        self.normalization[TRANSFORM - HOP..].fill(0.0);
        self.frames.pop_front();
        self.processed += 1;
        Ok(output)
    }
}
fn check_samples(samples: &[f32]) -> Result<()> {
    if samples.is_empty()
        || samples.len() > 32 * 1024 * 1024
        || samples.iter().any(|x| !x.is_finite() || x.abs() > 16.0)
    {
        return Err(Error::new(
            "Expected 1–33554432 finite audio samples within ±16",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streaming_matches_offline_with_ragged_chunks_and_tail() {
        for len in [1, 479, 480, 481, 4807] {
            let samples: Vec<_> = (0..len).map(|i| 0.2 * (i as f32 / 11.0).sin()).collect();
            let mut predict = |context: &[f32]| {
                Ok((0..GAINS)
                    .map(|k| context[3 * FEATURES + k % FEATURES] * 0.9)
                    .collect())
            };
            let offline = Processor::new()
                .unwrap()
                .process(&samples, 0.8, &mut predict)
                .unwrap();
            let mut stream = StreamProcessor::new().unwrap();
            let mut actual = Vec::new();
            let mut pos = 0;
            for size in [1, 47, 480, 13].into_iter().cycle() {
                let end = (pos + size).min(samples.len());
                actual.extend(stream.push(&samples[pos..end], 0.8, &mut predict).unwrap());
                pos = end;
                if pos == samples.len() {
                    break;
                }
            }
            actual.extend(stream.finish(0.8, &mut predict).unwrap());
            assert_eq!(actual.len(), offline.len());
            assert!(
                actual
                    .iter()
                    .zip(offline)
                    .all(|(a, b)| (a - b).abs() < 1e-7)
            );
            assert_eq!(stream.samples(), len as u64);
            assert!(stream.push(&[0.0], 0.8, &mut predict).is_err());
        }
    }
    #[test]
    fn streaming_delay_memory_bound_and_failure_lifecycle() {
        let mut stream = StreamProcessor::new().unwrap();
        let mut predict = |_: &[f32]| Ok(vec![0.0; GAINS]);
        for hop in 0..100 {
            let out = stream.push(&[0.1; HOP], 1.0, &mut predict).unwrap();
            assert_eq!(out.len(), if hop < 6 { 0 } else { HOP });
            assert_eq!(stream.history.len(), TRANSFORM);
            assert!(stream.frames.len() < CONTEXT);
            assert!(stream.pending.len() < HOP);
        }
        let mut failed = StreamProcessor::new().unwrap();
        assert!(failed.push(&[f32::NAN], 1.0, &mut predict).is_err());
        assert_eq!(failed.samples(), 0);
        let mut bad = |_: &[f32]| Err(Error::new("Compute failed"));
        for _ in 0..3 {
            failed.push(&[0.0; HOP], 1.0, &mut bad).unwrap();
        }
        assert!(failed.push(&[0.0; HOP], 1.0, &mut bad).is_err());
        assert!(failed.push(&[0.0; HOP], 1.0, &mut predict).is_err());
    }
    #[test]
    fn unity_reconstruction_covers_boundaries_and_partial_hops() {
        let processor = Processor::new().unwrap();
        for len in [1, 479, 480, 481, 4800] {
            let mut samples: Vec<_> = (0..len)
                .map(|i| ((i * 7919 % 65521) as f32 / 65521.0 - 0.5) * 0.5)
                .collect();
            samples[0] = 1.0;
            samples[len - 1] = -1.0;
            let out = processor
                .process(&samples, 1.0, |_| Ok(vec![0.0; GAINS]))
                .unwrap();
            assert_eq!(out.len(), len);
            for (a, b) in samples.iter().zip(out) {
                assert!((a - b).abs() < 2e-6, "{a} != {b}");
            }
        }
    }
    #[test]
    fn intensity_retains_full_suppression_in_strong_noise_bins() {
        let p = Processor::new().unwrap();
        let spectrum: Vec<_> = (0..TRANSFORM)
            .map(|k| {
                let bin = if k <= TRANSFORM / 2 { k } else { TRANSFORM - k };
                Complex {
                    re: (bin + 1) as f32 / GAINS as f32,
                    im: 0.0,
                }
            })
            .collect();
        let time = p.synthesize(&spectrum, &vec![0.8; GAINS], 0.5).unwrap();
        assert!(time.iter().all(|v| v.im.abs() < 1e-6));
        let actual = p.fft.transform(&time, false).unwrap();
        for (bin, gain) in [(10, 0.6), (900, 0.2)] {
            assert!((actual[bin].re - spectrum[bin].re * gain).abs() < 1e-5);
            assert!((actual[TRANSFORM - bin].re - actual[bin].re).abs() < 1e-5);
        }
    }
    #[test]
    fn noise_mask_inversion_strength_and_invalid_predictions() {
        let p = Processor::new().unwrap();
        let samples = vec![0.1; 500];
        let muted = p.process(&samples, 1.0, |_| Ok(vec![1.0; GAINS])).unwrap();
        assert!(muted.iter().all(|v| *v == 0.0));
        let bypass = p.process(&samples, 0.0, |_| Ok(vec![1.0; GAINS])).unwrap();
        assert!(bypass.iter().all(|v| (v - 0.1).abs() < 1e-6));
        assert!(
            p.process(&samples, 1.0, |_| Ok(vec![f32::NAN; GAINS]))
                .is_err()
        );
        assert!(p.process(&samples, 1.0, |_| Ok(vec![0.0; 2])).is_err());
        assert!(validate_strength(-0.1).is_err());
        assert!(validate_strength(f32::NAN).is_err());
    }
    #[test]
    fn feature_scaling_clips_and_context_advances() {
        assert_eq!(scale_power(0.0), 0.0);
        assert_eq!(scale_power(1e-4), 0.0);
        assert!((scale_power(1.0) - 40.0 / 88.0).abs() < 1e-7);
        assert_eq!(scale_power(1e10), 1.0);
        let p = Processor::new().unwrap();
        let samples: Vec<_> = (0..1500)
            .map(|i| 0.2 * (std::f32::consts::TAU * i as f32 / 48.0).sin())
            .collect();
        let mut features = Vec::new();
        p.features(&samples, |f| {
            features.extend_from_slice(f);
            Ok(())
        })
        .unwrap();
        let mut seen = 0;
        p.process(&samples, 0.0, |ctx| {
            for j in 0..CONTEXT {
                let idx = seen as i64 + j as i64 - 3;
                let expected = if idx >= 0 && (idx as usize) < Processor::frame_count(samples.len())
                {
                    &features[idx as usize * FEATURES..(idx as usize + 1) * FEATURES]
                } else {
                    &[0.0; FEATURES]
                };
                assert_eq!(&ctx[j * FEATURES..(j + 1) * FEATURES], expected);
            }
            seen += 1;
            Ok(vec![0.0; GAINS])
        })
        .unwrap();
        assert_eq!(seen, Processor::frame_count(samples.len()));
        assert!(features.iter().any(|v| *v > 0.0));
    }
}
