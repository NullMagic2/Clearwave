//! Mixed-radix complex FFT with reusable twiddles and bounded workspace.
use crate::{Error, Result};

const MAX_TRANSFORM_LENGTH: usize = 65536;
const RADIX_FACTORS: [usize; 3] = [2, 3, 5];

#[derive(Clone, Copy, Default)]
pub struct Complex {
    pub re: f32,
    pub im: f32,
}
impl Complex {
    pub fn multiply(self, other: Self) -> Self {
        Self {
            re: self.re * other.re - self.im * other.im,
            im: self.re * other.im + self.im * other.re,
        }
    }
    pub fn conjugate(self) -> Self {
        Self {
            re: self.re,
            im: -self.im,
        }
    }
    pub fn scale(self, factor: f32) -> Self {
        Self {
            re: self.re * factor,
            im: self.im * factor,
        }
    }
    pub fn add(self, other: Self) -> Self {
        Self {
            re: self.re + other.re,
            im: self.im + other.im,
        }
    }
    pub fn subtract(self, other: Self) -> Self {
        self.add(other.scale(-1.0))
    }
    pub fn power(self) -> f32 {
        self.re * self.re + self.im * self.im
    }
}

pub struct Fft {
    roots: Vec<Complex>,
}
impl Fft {
    pub fn new(len: usize) -> Result<Self> {
        let mut remaining = len;
        for factor in RADIX_FACTORS {
            while remaining > 1 && remaining.is_multiple_of(factor) {
                remaining /= factor;
            }
        }
        if remaining != 1 || len == 0 || len > MAX_TRANSFORM_LENGTH {
            return Err(Error::new(
                "FFT length must use factors 2, 3, 5 and be at most 65536",
            ));
        }
        let roots = (0..len)
            .map(|i| {
                let angle = -std::f64::consts::TAU * i as f64 / len as f64;
                Complex {
                    re: angle.cos() as f32,
                    im: angle.sin() as f32,
                }
            })
            .collect();
        Ok(Self { roots })
    }
    pub fn transform(&self, input: &[Complex], inverse: bool) -> Result<Vec<Complex>> {
        if input.len() != self.roots.len() {
            return Err(Error::new("Incorrect FFT input length"));
        }
        let mut output = vec![Complex::default(); input.len()];
        let mut scratch = output.clone();
        self.stage(input, 1, &mut output, &mut scratch, inverse);
        if inverse {
            for value in &mut output {
                value.re /= input.len() as f32;
                value.im /= input.len() as f32;
            }
        }
        Ok(output)
    }
    fn stage(
        &self,
        input: &[Complex],
        stride: usize,
        output: &mut [Complex],
        scratch: &mut [Complex],
        inverse: bool,
    ) {
        let n = output.len();
        if n == 1 {
            output[0] = input[0];
            return;
        }
        let radix = if n.is_multiple_of(2) {
            2
        } else if n.is_multiple_of(3) {
            3
        } else {
            5
        };
        let m = n / radix;
        for j in 0..radix {
            self.stage(
                &input[j * stride..],
                stride * radix,
                &mut output[j * m..(j + 1) * m],
                &mut scratch[j * m..(j + 1) * m],
                inverse,
            );
        }
        let root_step = self.roots.len() / n;
        for (k, value) in scratch.iter_mut().enumerate() {
            *value = Complex::default();
            for j in 0..radix {
                let mut root = self.roots[(j * k * root_step) % self.roots.len()];
                if inverse {
                    root.im = -root.im;
                }
                let term = output[j * m + k % m].multiply(root);
                value.re += term.re;
                value.im += term.im;
            }
        }
        output.copy_from_slice(scratch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mixed_radices_match_direct_dft() {
        for n in [1, 8, 15, 30] {
            let x: Vec<_> = (0..n)
                .map(|i| Complex {
                    re: (i * 7 % 13) as f32 / 13.0,
                    im: i as f32 / 50.0,
                })
                .collect();
            let y = Fft::new(n).unwrap().transform(&x, false).unwrap();
            for (k, value) in y.iter().enumerate() {
                let (mut re, mut im) = (0.0, 0.0);
                for (j, x) in x.iter().enumerate() {
                    let angle = -std::f64::consts::TAU * (j * k) as f64 / n as f64;
                    re += x.re as f64 * angle.cos() - x.im as f64 * angle.sin();
                    im += x.re as f64 * angle.sin() + x.im as f64 * angle.cos();
                }
                assert!((value.re as f64 - re).abs() < 2e-5);
                assert!((value.im as f64 - im).abs() < 2e-5);
            }
        }
    }
    #[test]
    fn model_transform_round_trip_and_tone_amplitude() {
        let fft = Fft::new(1920).unwrap();
        let x: Vec<_> = (0..1920)
            .map(|i| Complex {
                re: (std::f32::consts::TAU * 40.0 * i as f32 / 1920.0).cos(),
                im: 0.0,
            })
            .collect();
        let spectrum = fft.transform(&x, false).unwrap();
        assert!((spectrum[40].re - 960.0).abs() < 0.01);
        let back = fft.transform(&spectrum, true).unwrap();
        for (a, b) in x.iter().zip(back) {
            assert!((a.re - b.re).abs() < 1e-5);
            assert!(b.im.abs() < 1e-5);
        }
        assert!(Fft::new(7).is_err());
    }
}
