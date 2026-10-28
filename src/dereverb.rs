//! Optional single-channel late-reverberation reduction in the existing STFT.
//! Delayed weighted prediction adapts on raw spectra, independently of slider mix.
//! It has no playback reference and does not cancel speaker feedback or early echoes.
use crate::{
    Result,
    audio::{RATE, TRANSFORM},
    fft::Complex,
    network::GAINS,
};
use std::collections::VecDeque;

const PREDICTION_DELAY: usize = 5;
const PREDICTION_TAPS: usize = 8;
const HISTORY_FRAMES: usize = PREDICTION_DELAY + PREDICTION_TAPS - 1;
const FORGETTING: f32 = 0.995;
const POWER_SMOOTHING: f32 = 0.9;
const POWER_FLOOR: f32 = 1e-8;
const MAX_COVARIANCE: f32 = 1e6;
const MAX_SUBTRACTION: f32 = 0.7;
const ATTENUATION_SMOOTHING: f32 = 0.75;
const NEIGHBOUR_WEIGHT: f32 = 0.25;
const CENTRE_WEIGHT: f32 = 0.5;
const PROTECTED_VOICE_HZ: f32 = 250.0;
const FULL_CANCELLATION_HZ: f32 = 600.0;
const LOW_VOICE_CANCELLATION: f32 = 0.25;

fn cancellation_weight(bin: usize) -> f32 {
    let frequency = bin as f32 * RATE as f32 / TRANSFORM as f32;
    let transition = ((frequency - PROTECTED_VOICE_HZ)
        / (FULL_CANCELLATION_HZ - PROTECTED_VOICE_HZ))
        .clamp(0.0, 1.0);
    LOW_VOICE_CANCELLATION + (1.0 - LOW_VOICE_CANCELLATION) * transition
}

#[derive(Default)]
pub struct EchoReducer {
    amount: f32,
    state: Option<Box<State>>,
}
struct State {
    bins: Vec<Bin>,
    history: VecDeque<Vec<Complex>>,
}
struct Bin {
    weights: [Complex; PREDICTION_TAPS],
    inverse: [[Complex; PREDICTION_TAPS]; PREDICTION_TAPS],
    power: f32,
    attenuation: f32,
}
impl Default for Bin {
    fn default() -> Self {
        let mut inverse = [[Complex::default(); PREDICTION_TAPS]; PREDICTION_TAPS];
        for (i, row) in inverse.iter_mut().enumerate() {
            row[i].re = 1.0;
        }
        Self {
            weights: [Complex::default(); PREDICTION_TAPS],
            inverse,
            power: 0.0,
            attenuation: 0.0,
        }
    }
}
impl EchoReducer {
    pub fn set_amount(&mut self, amount: f32) -> Result<()> {
        crate::audio::validate_strength(amount)?;
        self.amount = amount;
        if amount == 0.0 {
            self.state = None;
        } else if self.state.is_none() {
            self.state = Some(Box::new(State {
                bins: (0..GAINS).map(|_| Bin::default()).collect(),
                history: VecDeque::with_capacity(HISTORY_FRAMES),
            }));
        }
        Ok(())
    }
    pub fn process(&mut self, spectrum: &mut [Complex]) {
        let Some(state) = &mut self.state else {
            return;
        };
        debug_assert_eq!(spectrum.len(), TRANSFORM);
        let raw = spectrum[..GAINS].to_vec();
        let mut targets = vec![0.0; GAINS];
        for (frequency, bin) in state.bins.iter_mut().enumerate() {
            let observed = raw[frequency];
            bin.power = POWER_SMOOTHING * bin.power + (1.0 - POWER_SMOOTHING) * observed.power();
            if state.history.len() < HISTORY_FRAMES || bin.power < POWER_FLOOR {
                continue;
            }
            let taps = std::array::from_fn(|tap| {
                state.history[HISTORY_FRAMES - PREDICTION_DELAY - tap][frequency]
            });
            let predicted = bin.predict_and_adapt(observed, &taps);

            let observed_magnitude = observed.power().sqrt();
            if observed_magnitude == 0.0 {
                continue;
            }
            let limit = MAX_SUBTRACTION * observed_magnitude;
            let magnitude = predicted.power().sqrt();
            let mix = if magnitude > limit {
                limit / magnitude
            } else {
                1.0
            };
            let residual = observed.subtract(predicted.scale(mix)).power().sqrt();
            targets[frequency] = (1.0 - residual / observed_magnitude).clamp(0.0, MAX_SUBTRACTION);
        }
        for (frequency, bin) in state.bins.iter_mut().enumerate() {
            let left = targets[frequency.saturating_sub(1)];
            let right = targets[(frequency + 1).min(GAINS - 1)];
            let target = CENTRE_WEIGHT * targets[frequency] + NEIGHBOUR_WEIGHT * (left + right);
            bin.attenuation =
                ATTENUATION_SMOOTHING * bin.attenuation + (1.0 - ATTENUATION_SMOOTHING) * target;

            // Convert the prediction into a smooth, attenuation-only mask.
            // Keep input phase: complex subtraction can rotate quiet noise bins
            // and create inconsistent phase across overlapping transform frames.
            let gain = 1.0 - self.amount * cancellation_weight(frequency) * bin.attenuation;
            let reduced = raw[frequency].scale(gain);
            spectrum[frequency] = reduced;
            if frequency > 0 && frequency < TRANSFORM / 2 {
                spectrum[TRANSFORM - frequency] = reduced.conjugate();
            }
        }
        if state.history.len() == HISTORY_FRAMES {
            state.history.pop_front();
        }
        state.history.push_back(raw);
    }
}
impl Bin {
    fn predict_and_adapt(
        &mut self,
        observed: Complex,
        taps: &[Complex; PREDICTION_TAPS],
    ) -> Complex {
        let predicted = self
            .weights
            .iter()
            .zip(taps)
            .fold(Complex::default(), |sum, (weight, sample)| {
                sum.add(weight.conjugate().multiply(*sample))
            });
        let product: [Complex; PREDICTION_TAPS] = std::array::from_fn(|i| {
            self.inverse[i]
                .iter()
                .zip(taps)
                .fold(Complex::default(), |sum, (value, sample)| {
                    sum.add(value.multiply(*sample))
                })
        });
        let energy = taps.iter().zip(product).fold(0.0, |sum, (sample, value)| {
            sum + sample.conjugate().multiply(value).re
        });
        let denominator = FORGETTING * self.power.max(POWER_FLOOR) + energy;
        if !denominator.is_finite() || denominator <= POWER_FLOOR {
            *self = Self::default();
            return Complex::default();
        }
        let error = observed.subtract(predicted).conjugate();
        for i in 0..PREDICTION_TAPS {
            self.weights[i] =
                self.weights[i].add(product[i].scale(1.0 / denominator).multiply(error));
            for j in 0..=i {
                let correction = product[i]
                    .multiply(product[j].conjugate())
                    .scale(1.0 / denominator);
                let value = self.inverse[i][j]
                    .subtract(correction)
                    .scale(1.0 / FORGETTING);
                self.inverse[i][j] = value;
                self.inverse[j][i] = value.conjugate();
            }
        }
        if self.weights.iter().any(|v| !v.power().is_finite())
            || (0..PREDICTION_TAPS).any(|i| {
                let diagonal = self.inverse[i][i].re;
                !diagonal.is_finite() || !(0.0..=MAX_COVARIANCE).contains(&diagonal)
            })
        {
            *self = Self::default();
            return Complex::default();
        }
        predicted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn echo_preserves_phase_and_never_amplifies_observed_bins() {
        const FRAMES: usize = 150;
        const TEST_BIN: usize = 40;
        const PHASE_STEP: f32 = 0.37;
        let mut echo = EchoReducer::default();
        echo.set_amount(1.0).unwrap();

        for frame in 0..FRAMES {
            let phase = (frame * frame) as f32 * PHASE_STEP;
            let mut spectrum = vec![Complex::default(); TRANSFORM];
            let observed = Complex {
                re: phase.cos(),
                im: phase.sin(),
            };
            spectrum[TEST_BIN] = observed;
            spectrum[TRANSFORM - TEST_BIN] = observed.conjugate();
            echo.process(&mut spectrum);

            let product = spectrum[TEST_BIN].multiply(observed.conjugate());
            assert!(
                product.im.abs() < 1e-6,
                "phase rotation at frame {frame}: {}",
                product.im
            );
            assert!(product.re >= 0.0);
            assert!(spectrum[TEST_BIN].power() <= observed.power() + 1e-6);
        }
    }

    #[test]
    fn sustained_low_voice_is_protected_when_the_predictor_learns_its_harmonics() {
        const LOW_BIN: usize = 7;
        const HIGH_BIN: usize = 40;
        const FRAMES: usize = 300;
        let mut echo = EchoReducer::default();
        echo.set_amount(1.0).unwrap();
        let mut spectrum = vec![Complex::default(); TRANSFORM];
        for _ in 0..FRAMES {
            spectrum.fill(Complex::default());
            for bin in [LOW_BIN, HIGH_BIN] {
                spectrum[bin].re = 1.0;
                spectrum[TRANSFORM - bin].re = 1.0;
            }
            echo.process(&mut spectrum);
            assert!(spectrum[LOW_BIN].re >= 0.825 - f32::EPSILON);
            assert_eq!(spectrum[LOW_BIN].re, spectrum[TRANSFORM - LOW_BIN].re);
        }
        assert!(spectrum[LOW_BIN].re > spectrum[HIGH_BIN].re);
    }

    #[test]
    fn slider_mix_changes_cancellation_and_off_discards_adaptation() {
        let mut half = EchoReducer::default();
        let mut full = EchoReducer::default();
        half.set_amount(0.5).unwrap();
        full.set_amount(1.0).unwrap();
        let mut off = EchoReducer::default();
        let mut differences = 0;
        const BIN: usize = 40;
        for frame in 0..200 {
            let mut spectrum = vec![Complex::default(); TRANSFORM];
            spectrum[BIN].re = if frame % 20 < 12 { 1.0 } else { 0.3 };
            spectrum[TRANSFORM - BIN] = spectrum[BIN].conjugate();
            let original = spectrum.clone();
            off.process(&mut spectrum);
            assert_eq!(spectrum[BIN].re, original[BIN].re);
            let mut a = spectrum.clone();
            half.process(&mut a);
            full.process(&mut spectrum);
            assert!((a[BIN].re - (original[BIN].re + spectrum[BIN].re) / 2.0).abs() < 1e-5);
            assert_eq!(a[BIN].im, -a[TRANSFORM - BIN].im);
            if (spectrum[BIN].re - original[BIN].re).abs() > 0.01 {
                differences += 1;
            }
        }
        assert!(differences > 0);
        full.set_amount(0.0).unwrap();
        assert!(full.state.is_none());
        assert!(full.set_amount(1.01).is_err());
    }
    #[test]
    fn learns_delayed_reflections_without_uniform_attenuation() {
        let mut bin = Bin::default();
        let mut history = VecDeque::from(vec![Complex::default(); HISTORY_FRAMES]);
        let mut seed = 1u32;
        let mut direct = Vec::new();
        let (mut before, mut after, mut onset_error) = (0.0, 0.0, 0.0);
        const FRAMES: usize = 4000;
        const CYCLE: usize = 60;
        const SPEECH_FRAMES: usize = 25;
        const REFLECTION_DELAY: usize = 7;
        for frame in 0..FRAMES {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let source = if frame % CYCLE < SPEECH_FRAMES {
                (seed as f32 / u32::MAX as f32 - 0.5) * 2.0
            } else {
                0.0
            };
            direct.push(source);
            let reflected = if frame >= REFLECTION_DELAY {
                0.6 * direct[frame - REFLECTION_DELAY]
            } else {
                0.0
            };
            let observed = Complex {
                re: source + reflected,
                im: 0.0,
            };
            bin.power = POWER_SMOOTHING * bin.power + (1.0 - POWER_SMOOTHING) * observed.power();
            let taps = std::array::from_fn(|tap| history[HISTORY_FRAMES - PREDICTION_DELAY - tap]);
            let predicted = bin.predict_and_adapt(observed, &taps);
            if frame > FRAMES / 2 {
                if (SPEECH_FRAMES..SPEECH_FRAMES + REFLECTION_DELAY).contains(&(frame % CYCLE)) {
                    before += observed.power();
                    after += observed.subtract(predicted).power();
                } else if frame % CYCLE < PREDICTION_DELAY {
                    onset_error += (observed.subtract(predicted).re - source).abs();
                }
            }
            history.pop_front();
            history.push_back(observed);
        }
        assert!(after < before * 0.5, "tail energy {after} vs {before}");
        assert_eq!(onset_error, 0.0);
    }
}
