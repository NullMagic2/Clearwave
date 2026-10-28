//! Final microphone voicing shared by offline, Linux and Windows audio paths.
//! A modest peaking EQ adds body after noise removal without boosting DC.
//! A soft peak guard bounds loud output; model features and masks stay untouched.
use crate::{Error, Result, audio::RATE};

pub const PROFILE: &str = "balanced-v1";
const CENTRE_HZ: f64 = 180.0;
const BOOST_DB: f64 = 2.5;
const QUALITY: f64 = 0.8;
const DB_AMPLITUDE_DIVISOR: f64 = 40.0;
const PEAK_KNEE: f64 = 0.98;
const FULL_SCALE: f64 = 1.0;

pub struct VoiceBalance {
    numerator: [f64; 3],
    denominator: [f64; 2],
    delay: [f64; 2],
}
impl Default for VoiceBalance {
    fn default() -> Self {
        let amplitude = 10.0_f64.powf(BOOST_DB / DB_AMPLITUDE_DIVISOR);
        let omega = std::f64::consts::TAU * CENTRE_HZ / RATE as f64;
        let alpha = omega.sin() / (2.0 * QUALITY);
        let cosine = -2.0 * omega.cos();
        let normalization = 1.0 + alpha / amplitude;

        Self {
            numerator: [
                (1.0 + alpha * amplitude) / normalization,
                cosine / normalization,
                (1.0 - alpha * amplitude) / normalization,
            ],
            denominator: [
                cosine / normalization,
                (1.0 - alpha / amplitude) / normalization,
            ],
            delay: [0.0; 2],
        }
    }
}
impl VoiceBalance {
    pub fn process(&mut self, samples: &mut [f32]) -> Result<()> {
        for sample in samples {
            if !sample.is_finite() {
                return Err(Error::new("Voice balance received a nonfinite sample"));
            }
            let input = f64::from(*sample);
            let output = self.numerator[0] * input + self.delay[0];
            self.delay[0] =
                self.numerator[1] * input - self.denominator[0] * output + self.delay[1];
            self.delay[1] = self.numerator[2] * input - self.denominator[1] * output;

            // The EQ uses double-precision state and introduces no lookahead.
            // Preserve ordinary samples; round only peaks near full scale.
            // Keep guard distortion out of the filter's own feedback state.
            let guarded = if output.abs() > PEAK_KNEE {
                let room = FULL_SCALE - PEAK_KNEE;
                output.signum() * (PEAK_KNEE + room * ((output.abs() - PEAK_KNEE) / room).tanh())
            } else {
                output
            };
            *sample = guarded as f32;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_gain(frequency: f64) -> f64 {
        let mut samples: Vec<_> = (0..RATE)
            .map(|i| {
                (0.1 * (std::f64::consts::TAU * frequency * i as f64 / RATE as f64).sin()) as f32
            })
            .collect();
        let input = samples.clone();
        VoiceBalance::default().process(&mut samples).unwrap();
        let energy = |values: &[f32]| {
            values[RATE / 2..]
                .iter()
                .map(|v| f64::from(*v).powi(2))
                .sum::<f64>()
        };
        10.0 * (energy(&samples) / energy(&input)).log10()
    }

    #[test]
    fn adds_voice_body_without_broadband_gain_or_strong_rumble_boost() {
        assert!((tone_gain(CENTRE_HZ) - BOOST_DB).abs() < 0.001);
        assert!(tone_gain(1000.0).abs() < 0.15);
        assert!(tone_gain(8000.0).abs() < 0.01);
        assert!(tone_gain(20.0).abs() < 0.06);
    }

    #[test]
    fn chunking_is_exact_and_guard_handles_loud_input_and_silence() {
        let input: Vec<_> = (0..RATE)
            .map(|i| {
                (2.0 * (std::f64::consts::TAU * CENTRE_HZ * i as f64 / RATE as f64).sin()) as f32
            })
            .collect();
        let mut whole = input.clone();
        VoiceBalance::default().process(&mut whole).unwrap();
        let mut chunked = input;
        let mut processor = VoiceBalance::default();
        for chunk in chunked.chunks_mut(173) {
            processor.process(chunk).unwrap();
        }
        assert_eq!(chunked, whole);
        assert!(whole.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
        let mut silence = vec![0.0; RATE];
        VoiceBalance::default().process(&mut silence).unwrap();
        assert!(silence.iter().all(|v| *v == 0.0));
        assert!(VoiceBalance::default().process(&mut [f32::NAN]).is_err());
    }
}
