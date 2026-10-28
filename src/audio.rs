//! Convert waveform audio to model features and reconstruct masked spectra.
//! Offline and live paths share bounded context priming and spectral processing.
use crate::{
    Error, Result,
    conditioning::{BLOCK_SAMPLES, Conditioner},
    fft::{Complex, Fft},
    network::{CONTEXT, FEATURES, GAINS},
    package::ModelProfile,
};
use std::collections::VecDeque;

pub const RATE: usize = 48000;
pub const TRANSFORM: usize = 1920;
pub const HOP: usize = 480;
const PREFIX: usize = TRANSFORM - HOP;
const PREFIX_HOPS: usize = PREFIX / HOP;
pub const CONTEXT_RADIUS: usize = CONTEXT / 2;
pub const MAX_SAMPLE_AMPLITUDE: f32 = 16.0;
const MAX_SAMPLES: usize = 32 * 1024 * 1024;
const MEL_LINEAR_FREQUENCY: f32 = 1000.0;
const MEL_LINEAR_SCALE: f32 = 200.0 / 3.0;
const MEL_LINEAR_BANDS: f32 = 15.0;
const MEL_LOG_RANGE: f32 = 6.4;
const MEL_LOG_BANDS: f32 = 27.0;
const MIN_FEATURE_POWER: f32 = 1.0 / (1 << 24) as f32;
const FEATURE_POWER_OFFSET_DB: f32 = 40.0;
const FEATURE_POWER_RANGE_DB: f32 = 88.0;
const HAMMING_OFFSET: f32 = 0.54;
const HAMMING_COSINE: f32 = 0.46;
const HANN_COEFFICIENT: f32 = 0.5;
const TOP_NOISE_BIN_FRACTION: f32 = 0.2;
const BROADCAST_STRENGTH_KNEE: f64 = 0.8;
const BROADCAST_LOG_OFFSET: f64 = 0.1;
const BROADCAST_UPPER_INTERVAL: f64 = 0.2;
const BROADCAST_LOG_ANCHOR: f64 = 0.9;

fn spectral_strength(profile: ModelProfile, value: f32) -> f32 {
    if profile == ModelProfile::RtxVoice {
        return value;
    }
    let value = f64::from(value);
    let mapped = if value < BROADCAST_STRENGTH_KNEE {
        1.0 + (value + BROADCAST_LOG_OFFSET).log10()
    } else {
        let anchor = BROADCAST_LOG_ANCHOR.log10();
        1.0 + anchor - anchor * (value - BROADCAST_STRENGTH_KNEE) / BROADCAST_UPPER_INTERVAL
    };
    mapped.clamp(0.0, 1.0) as f32
}

pub fn lookahead_hops(profile: ModelProfile) -> usize {
    match profile {
        ModelProfile::RtxVoice => CONTEXT_RADIUS,
        ModelProfile::Broadcast23 => 0,
    }
}
pub fn algorithmic_delay_samples(profile: ModelProfile) -> usize {
    PREFIX + lookahead_hops(profile) * HOP
}

struct Frame {
    spectrum: Vec<Complex>,
    features: Vec<f32>,
}
struct Prediction {
    spectrum: Vec<Complex>,
    noise: Vec<f32>,
}
struct FrameContext {
    frames: VecDeque<Frame>,
    target: usize,
}
impl Frame {
    fn padding() -> Self {
        Self {
            spectrum: Vec::new(),
            features: vec![0.0; FEATURES],
        }
    }
}
impl FrameContext {
    fn new(profile: ModelProfile) -> Self {
        Self {
            target: CONTEXT - 1 - lookahead_hops(profile),
            frames: (0..CONTEXT - 1).map(|_| Frame::padding()).collect(),
        }
    }
    fn push(
        &mut self,
        frame: Frame,
        predict: &mut impl FnMut(&[f32]) -> Result<Vec<f32>>,
    ) -> Result<Option<Prediction>> {
        self.frames.push_back(frame);
        let mut context = Vec::with_capacity(CONTEXT * FEATURES);
        for frame in &self.frames {
            context.extend_from_slice(&frame.features);
        }
        let noise = predict(&context)?;

        // Legacy predictions target the centered spectrum after lookahead.
        // Broadcast predicts the incoming spectrum from causal feature history.
        // Discard only predictions whose target is a virtual padding frame.
        let spectrum = std::mem::take(&mut self.frames[self.target].spectrum);
        self.frames.pop_front();
        if spectrum.is_empty() {
            validate_mask(&noise)?;
            Ok(None)
        } else {
            Ok(Some(Prediction { spectrum, noise }))
        }
    }
}
pub struct Processor {
    fft: Fft,
    window: Vec<f32>,
    filters: Vec<Vec<(usize, f32)>>,
    profile: ModelProfile,
}
fn mel(f: f32) -> f32 {
    if f < MEL_LINEAR_FREQUENCY {
        f / MEL_LINEAR_SCALE
    } else {
        MEL_LINEAR_BANDS + (f / MEL_LINEAR_FREQUENCY).ln() / (MEL_LOG_RANGE.ln() / MEL_LOG_BANDS)
    }
}
fn frequency(m: f32) -> f32 {
    if m < MEL_LINEAR_BANDS {
        m * MEL_LINEAR_SCALE
    } else {
        MEL_LINEAR_FREQUENCY * ((m - MEL_LINEAR_BANDS) * (MEL_LOG_RANGE.ln() / MEL_LOG_BANDS)).exp()
    }
}
fn scale_power(power: f32) -> f32 {
    ((10.0 * power.max(MIN_FEATURE_POWER).log10() + FEATURE_POWER_OFFSET_DB)
        / FEATURE_POWER_RANGE_DB)
        .clamp(0.0, 1.0)
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
        Self::for_profile(ModelProfile::RtxVoice)
    }
    pub fn for_profile(profile: ModelProfile) -> Result<Self> {
        let (offset, cosine) = match profile {
            ModelProfile::RtxVoice => (HAMMING_OFFSET, HAMMING_COSINE),
            ModelProfile::Broadcast23 => (HANN_COEFFICIENT, HANN_COEFFICIENT),
        };
        let window = (0..TRANSFORM)
            .map(|n| offset - cosine * (std::f32::consts::TAU * n as f32 / TRANSFORM as f32).cos())
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
            profile,
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
        validate_mask(noise)?;
        // Legacy intensity preserves the strongest predicted-noise bins.
        // Broadcast instead blends every bin using its nonlinear slider curve.
        // Full strength and bypass retain their exact endpoint behavior.
        let strength = spectral_strength(self.profile, strength);
        let top = if self.profile == ModelProfile::RtxVoice {
            (GAINS as f32 * TOP_NOISE_BIN_FRACTION * strength) as usize
        } else {
            0
        };
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
        self.process_with_echo(samples, strength, 0.0, &mut predict)
    }
    pub fn process_with_echo(
        &self,
        samples: &[f32],
        strength: f32,
        echo_reduction: f32,
        mut predict: impl FnMut(&[f32]) -> Result<Vec<f32>>,
    ) -> Result<Vec<f32>> {
        check_samples(samples)?;
        validate_strength(strength)?;
        if self.profile == ModelProfile::Broadcast23 {
            let mut stream = StreamProcessor::new(self.profile)?;
            stream.set_echo_reduction(echo_reduction)?;
            let mut output = Vec::with_capacity(samples.len());
            for chunk in samples.chunks(HOP) {
                output.extend(stream.push(chunk, strength, &mut predict)?);
            }
            output.extend(stream.finish(strength, &mut predict)?);
            return Ok(output);
        }
        let mut echo = crate::dereverb::EchoReducer::default();
        echo.set_amount(echo_reduction)?;
        let count = Self::frame_count(samples.len());
        let mut context = FrameContext::new(self.profile);
        let mut output = vec![0.0; samples.len()];
        let mut normalization = vec![0.0; samples.len()];
        let mut processed = 0;

        for frame in 0..count + lookahead_hops(self.profile) {
            let frame = if frame < count {
                self.analyze(samples, frame)?
            } else {
                Frame::padding()
            };
            let Some(mut prediction) = context.push(frame, &mut predict)? else {
                continue;
            };
            echo.process(&mut prediction.spectrum);
            let time = self.synthesize(&prediction.spectrum, &prediction.noise, strength)?;
            let start = processed as i64 * HOP as i64 - PREFIX as i64;
            for (i, value) in time.iter().enumerate() {
                let pos = start + i as i64;
                if pos >= 0 && (pos as usize) < samples.len() {
                    let p = pos as usize;
                    output[p] += value.re * self.window[i];
                    normalization[p] += self.window[i] * self.window[i];
                }
            }
            processed += 1;
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

/// Incremental adapter accepts one model hop per push.
/// Context lookahead and window overlap determine the fixed algorithmic delay.
/// Finishing flushes the finite tail and preserves the original sample count.
pub struct StreamProcessor {
    processor: Processor,
    history: VecDeque<f32>,
    pending: Vec<f32>,
    context: FrameContext,
    overlap: Vec<f32>,
    normalization: Vec<f32>,
    processed: u64,
    received: u64,
    emitted: u64,
    finished: bool,
    echo: crate::dereverb::EchoReducer,
    conditioner: Option<Conditioner>,
    gains: VecDeque<f32>,
}
impl StreamProcessor {
    pub fn new(profile: ModelProfile) -> Result<Self> {
        Ok(Self {
            processor: Processor::for_profile(profile)?,
            history: vec![0.0; TRANSFORM].into(),
            pending: Vec::with_capacity(if profile == ModelProfile::Broadcast23 {
                BLOCK_SAMPLES
            } else {
                HOP
            }),
            context: FrameContext::new(profile),
            overlap: vec![0.0; TRANSFORM],
            normalization: vec![0.0; TRANSFORM],
            processed: 0,
            received: 0,
            emitted: 0,
            finished: false,
            echo: crate::dereverb::EchoReducer::default(),
            conditioner: (profile == ModelProfile::Broadcast23).then(Conditioner::default),
            gains: VecDeque::with_capacity(CONTEXT),
        })
    }
    pub fn set_echo_reduction(&mut self, amount: f32) -> Result<()> {
        self.echo.set_amount(amount)
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
        if samples.len() > HOP
            || samples
                .iter()
                .any(|v| !v.is_finite() || v.abs() > MAX_SAMPLE_AMPLITUDE)
        {
            return Err(Error::new(
                "Stream chunks must contain at most 480 finite samples within ±16",
            ));
        }
        self.received = self
            .received
            .checked_add(samples.len() as u64)
            .ok_or_else(|| Error::new("Audio stream sample counter overflow"))?;
        self.pending.extend_from_slice(samples);
        let block_samples = self.block_samples();
        if self.pending.len() < block_samples {
            return Ok(Vec::new());
        }
        let mut block: Vec<_> = self.pending.drain(..block_samples).collect();
        let result = self.append_block(&mut block, strength, predict);
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
        let mut output = Vec::with_capacity(HOP * CONTEXT);
        if !self.pending.is_empty() {
            let mut block = std::mem::take(&mut self.pending);
            block.resize(self.block_samples(), 0.0);
            output.extend(self.append_block(&mut block, strength, predict)?);
        }
        for _ in 0..PREFIX_HOPS {
            self.gains.push_back(1.0);
            output.extend(self.append_hop(&[0.0; HOP], strength, predict)?);
            if let Some(conditioner) = &mut self.conditioner {
                conditioner.end();
            }
        }
        for _ in 0..lookahead_hops(self.processor.profile) {
            output.extend(self.append_frame(Frame::padding(), strength, predict)?);
        }
        if self.emitted != self.received {
            return Err(Error::new(
                "Audio stream flush produced an incorrect sample count",
            ));
        }
        Ok(output)
    }
    fn block_samples(&self) -> usize {
        if self.conditioner.is_some() {
            BLOCK_SAMPLES
        } else {
            HOP
        }
    }
    fn append_block(
        &mut self,
        block: &mut [f32],
        strength: f32,
        predict: &mut impl FnMut(&[f32]) -> Result<Vec<f32>>,
    ) -> Result<Vec<f32>> {
        let gain = self
            .conditioner
            .as_mut()
            .map_or(1.0, |conditioner| conditioner.begin(block));
        let mut output = Vec::with_capacity(block.len());
        for hop in block.chunks(HOP) {
            self.gains.push_back(gain);
            output.extend(self.append_hop(hop, strength, predict)?);
        }
        if let Some(conditioner) = &mut self.conditioner {
            conditioner.end();
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
        let Some(mut prediction) = self.context.push(frame, predict)? else {
            return Ok(Vec::new());
        };
        self.echo.process(&mut prediction.spectrum);
        let time = self
            .processor
            .synthesize(&prediction.spectrum, &prediction.noise, strength)?;
        for (i, sample) in time.iter().enumerate() {
            let w = self.processor.window[i];
            self.overlap[i] += sample.re * w;
            self.normalization[i] += w * w;
        }
        let mut output = Vec::with_capacity(HOP);
        if self.processed >= PREFIX_HOPS as u64 {
            let count = (self.received - self.emitted).min(HOP as u64) as usize;
            for i in 0..count {
                let value = self.overlap[i] / self.normalization[i];
                if !value.is_finite() {
                    return Err(Error::new("Audio stream produced a nonfinite sample"));
                }
                output.push(value);
            }
            if let Some(conditioner) = &mut self.conditioner {
                conditioner.observe(&output);
            }
            let gain = self
                .gains
                .pop_front()
                .ok_or_else(|| Error::new("Missing input gain for reconstructed audio"))?;
            for value in &mut output {
                *value *= gain;
            }
            self.emitted += count as u64;
        } else if let Some(conditioner) = &mut self.conditioner {
            conditioner.observe(&[0.0; HOP]);
        }
        self.overlap.rotate_left(HOP);
        self.overlap[TRANSFORM - HOP..].fill(0.0);
        self.normalization.rotate_left(HOP);
        self.normalization[TRANSFORM - HOP..].fill(0.0);
        self.processed += 1;
        Ok(output)
    }
}
fn validate_mask(noise: &[f32]) -> Result<()> {
    if noise.len() != GAINS
        || noise
            .iter()
            .any(|n| !n.is_finite() || !(0.0..=1.0).contains(n))
    {
        return Err(Error::new(
            "Expected 961 finite noise-mask values from 0 to 1",
        ));
    }
    Ok(())
}
fn check_samples(samples: &[f32]) -> Result<()> {
    if samples.is_empty()
        || samples.len() > MAX_SAMPLES
        || samples
            .iter()
            .any(|x| !x.is_finite() || x.abs() > MAX_SAMPLE_AMPLITUDE)
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
    fn broadcast_masks_target_incoming_spectra_with_causal_history() {
        let mut queue = FrameContext::new(ModelProfile::Broadcast23);
        for frame in 0..CONTEXT * 2 {
            let value = (frame + 1) as f32;
            let prediction = queue
                .push(
                    Frame {
                        spectrum: vec![Complex { re: value, im: 0.0 }],
                        features: vec![value; FEATURES],
                    },
                    &mut |context| {
                        for offset in 0..CONTEXT {
                            let source = frame as isize + offset as isize - (CONTEXT - 1) as isize;
                            let expected = if source < 0 { 0.0 } else { (source + 1) as f32 };
                            assert!(
                                context[offset * FEATURES..(offset + 1) * FEATURES]
                                    .iter()
                                    .all(|v| *v == expected)
                            );
                        }
                        Ok(vec![
                            context[(CONTEXT - 1) * FEATURES] / (CONTEXT * 2) as f32;
                            GAINS
                        ])
                    },
                )
                .unwrap()
                .unwrap();
            assert_eq!(prediction.spectrum[0].re, value);
            assert_eq!(prediction.noise[0], value / (CONTEXT * 2) as f32);
        }
    }

    #[test]
    fn broadcast_window_and_streaming_match_offline_for_changing_masks() {
        let profile = ModelProfile::Broadcast23;
        let processor = Processor::for_profile(profile).unwrap();
        assert_eq!(processor.window[0], 0.0);
        assert!((processor.window[TRANSFORM / 2] - 1.0).abs() < f32::EPSILON);
        assert_eq!(algorithmic_delay_samples(profile), PREFIX);
        let samples: Vec<_> = (0..HOP * 20 + 13)
            .map(|i| 0.2 * (std::f32::consts::TAU * i as f32 / 79.0).sin())
            .collect();
        let mut predict = |context: &[f32]| Ok(vec![context[(CONTEXT - 1) * FEATURES]; GAINS]);
        let offline = processor.process(&samples, 0.8, &mut predict).unwrap();
        let mut stream = StreamProcessor::new(profile).unwrap();
        let mut actual = Vec::new();
        for chunk in samples.chunks(HOP / 3) {
            actual.extend(stream.push(chunk, 0.8, &mut predict).unwrap());
        }
        actual.extend(stream.finish(0.8, &mut predict).unwrap());
        assert_eq!(actual.len(), samples.len());
        assert!(
            actual
                .iter()
                .zip(offline)
                .all(|(a, b)| (a - b).abs() < 1e-7)
        );
        let mut stream = StreamProcessor::new(profile).unwrap();
        for hop in 0..crate::conditioning::BLOCK_HOPS * 3 {
            let expected = if (hop + 1) % crate::conditioning::BLOCK_HOPS != 0 {
                0
            } else if hop < crate::conditioning::BLOCK_HOPS {
                HOP
            } else {
                BLOCK_SAMPLES
            };
            assert_eq!(
                stream.push(&[0.1; HOP], 1.0, &mut predict).unwrap().len(),
                expected
            );
        }
    }

    #[test]
    fn broadcast_slider_blends_every_bin_and_bypass_preserves_gain_changes() {
        let processor = Processor::for_profile(ModelProfile::Broadcast23).unwrap();
        let spectrum: Vec<_> = (0..TRANSFORM)
            .map(|k| {
                let bin = if k <= TRANSFORM / 2 { k } else { TRANSFORM - k };
                Complex {
                    re: (bin + 1) as f32 / GAINS as f32,
                    im: 0.0,
                }
            })
            .collect();
        for (slider, mapped) in [
            (0.0, 0.0),
            (0.25, 0.54406804),
            (0.5, 0.7781513),
            (0.8, 0.9542425),
            (0.9, 0.97712123),
            (1.0, 1.0),
        ] {
            let time = processor
                .synthesize(&spectrum, &vec![0.8; GAINS], slider)
                .unwrap();
            let actual = processor.fft.transform(&time, false).unwrap();
            for bin in [10, 900] {
                assert!((actual[bin].re - spectrum[bin].re * (1.0 - mapped * 0.8)).abs() < 1e-5);
            }
        }
        let samples: Vec<_> = (0..BLOCK_SAMPLES * 5 + 13)
            .map(|i| {
                let amplitude = if i < BLOCK_SAMPLES * 3 { 0.01 } else { 0.5 };
                amplitude * (std::f32::consts::TAU * i as f32 / 79.0).sin()
            })
            .collect();
        let output = processor
            .process(&samples, 0.0, |_| Ok(vec![0.8; GAINS]))
            .unwrap();
        assert_eq!(output.len(), samples.len());
        assert!(
            output
                .iter()
                .zip(&samples)
                .all(|(a, b)| (a - b).abs() < 2e-6)
        );
        for length in [
            1,
            HOP - 1,
            HOP,
            HOP + 1,
            BLOCK_SAMPLES - 1,
            BLOCK_SAMPLES,
            BLOCK_SAMPLES + 1,
        ] {
            let output = processor
                .process(&samples[..length], 1.0, |_| Ok(vec![0.0; GAINS]))
                .unwrap();
            assert_eq!(output.len(), length);
            assert!(
                output
                    .iter()
                    .zip(&samples)
                    .all(|(a, b)| (a - b).abs() < 2e-6)
            );
        }
    }

    #[test]
    fn echo_off_is_exact_and_streaming_matches_offline_when_enabled() {
        let samples: Vec<_> = (0..HOP * 25 + 13)
            .map(|i| 0.2 * (std::f32::consts::TAU * i as f32 / 79.0).sin())
            .collect();
        let processor = Processor::new().unwrap();
        let mut predict = |_: &[f32]| Ok(vec![0.0; GAINS]);
        let baseline = processor.process(&samples, 0.0, &mut predict).unwrap();
        let off = processor
            .process_with_echo(&samples, 0.0, 0.0, &mut predict)
            .unwrap();
        assert_eq!(off, baseline);
        let expected = processor
            .process_with_echo(&samples, 0.0, 0.6, &mut predict)
            .unwrap();
        let mut stream = StreamProcessor::new(ModelProfile::RtxVoice).unwrap();
        stream.set_echo_reduction(0.6).unwrap();
        let mut actual = Vec::new();
        for chunk in samples.chunks(HOP / 3) {
            actual.extend(stream.push(chunk, 0.0, &mut predict).unwrap());
        }
        actual.extend(stream.finish(0.0, &mut predict).unwrap());
        assert_eq!(actual.len(), samples.len());
        assert!(
            actual
                .iter()
                .zip(&expected)
                .all(|(a, b)| (a - b).abs() < 1e-5)
        );
        assert!(actual.iter().all(|v| v.is_finite()));
        assert!(stream.set_echo_reduction(f32::NAN).is_err());
    }
    #[test]
    fn context_primes_state_before_the_first_waveform_mask() {
        const ARRIVAL_MARKERS: [f32; 4] = [1.0, 2.0, 3.0, 4.0];
        const EXPECTED_FIRST_MASK: f32 = 0.1;
        const STATE_SCALE: f32 = 100.0;
        let mut queue = FrameContext::new(ModelProfile::RtxVoice);
        let mut accumulated = 0.0;
        let mut calls = 0;

        for marker in ARRIVAL_MARKERS {
            let prediction = queue
                .push(
                    Frame {
                        spectrum: vec![
                            Complex {
                                re: marker,
                                im: 0.0
                            };
                            TRANSFORM
                        ],
                        features: vec![marker; FEATURES],
                    },
                    &mut |context| {
                        calls += 1;
                        assert_eq!(context.len(), CONTEXT * FEATURES);
                        assert_eq!(context[(CONTEXT - 1) * FEATURES], marker);
                        accumulated += marker;
                        Ok(vec![accumulated / STATE_SCALE; GAINS])
                    },
                )
                .unwrap();
            if marker < ARRIVAL_MARKERS[ARRIVAL_MARKERS.len() - 1] {
                assert!(prediction.is_none());
            } else {
                let prediction = prediction.unwrap();
                assert_eq!(prediction.spectrum[0].re, ARRIVAL_MARKERS[0]);
                assert_eq!(prediction.noise[0], EXPECTED_FIRST_MASK);
            }
            assert_eq!(queue.frames.len(), CONTEXT - 1);
        }
        assert_eq!(calls, ARRIVAL_MARKERS.len());
    }

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
            let mut stream = StreamProcessor::new(ModelProfile::RtxVoice).unwrap();
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
        let mut stream = StreamProcessor::new(ModelProfile::RtxVoice).unwrap();
        let mut predict = |_: &[f32]| Ok(vec![0.0; GAINS]);
        for hop in 0..100 {
            let out = stream.push(&[0.1; HOP], 1.0, &mut predict).unwrap();
            assert_eq!(out.len(), if hop < 6 { 0 } else { HOP });
            assert_eq!(stream.history.len(), TRANSFORM);
            assert!(stream.context.frames.len() < CONTEXT);
            assert!(stream.pending.len() < HOP);
        }
        let mut failed = StreamProcessor::new(ModelProfile::RtxVoice).unwrap();
        assert!(failed.push(&[f32::NAN], 1.0, &mut predict).is_err());
        assert_eq!(failed.samples(), 0);
        let mut bad = |_: &[f32]| Err(Error::new("Compute failed"));
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
                let idx = seen as i64 + j as i64 - (CONTEXT - 1) as i64;
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
        assert_eq!(seen, Processor::frame_count(samples.len()) + CONTEXT_RADIUS);
        assert!(features.iter().any(|v| *v > 0.0));
    }
}
