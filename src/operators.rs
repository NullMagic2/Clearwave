//! CPU reference operators and explicit tensor layout contracts.
use crate::{Error, Result};

#[derive(Clone, Copy)]
pub struct ConvShape {
    pub channels: usize,
    pub height: usize,
    pub width: usize,
    pub output_channels: usize,
    pub kernel_height: usize,
    pub kernel_width: usize,
    pub relu: bool,
}
impl ConvShape {
    pub fn output_height(self) -> Result<usize> {
        if self.channels == 0
            || self.width == 0
            || self.output_channels == 0
            || self.kernel_height == 0
            || self.kernel_width == 0
            || self.kernel_width.is_multiple_of(2)
        {
            return Err(Error::new("Invalid convolution shape"));
        }
        self.height
            .checked_sub(self.kernel_height)
            .map(|v| v + 1)
            .ok_or_else(|| Error::new("Convolution context is too short"))
    }
    pub fn validate(self, input: &[f32], weights: &[f32], bias: &[f32]) -> Result<usize> {
        let out_height = self.output_height()?;
        if input.len() != self.channels * self.height * self.width
            || weights.len()
                != self.output_channels * self.channels * self.kernel_height * self.kernel_width
            || bias.len() != self.output_channels
        {
            return Err(Error::new("Convolution tensor size mismatch"));
        }
        Ok(out_height)
    }
}
pub fn convolution(
    input: &[f32],
    weights: &[f32],
    bias: &[f32],
    shape: ConvShape,
) -> Result<Vec<f32>> {
    let h = shape.validate(input, weights, bias)?;
    let mut out = vec![0.0; shape.output_channels * h * shape.width];
    for c in 0..shape.output_channels {
        for t in 0..h {
            for x in 0..shape.width {
                let mut sum = f64::from(bias[c]);
                for ci in 0..shape.channels {
                    for kt in 0..shape.kernel_height {
                        for kx in 0..shape.kernel_width {
                            let sx = x as isize + kx as isize - (shape.kernel_width / 2) as isize;
                            if (0..shape.width as isize).contains(&sx) {
                                let a = (ci * shape.height + t + kt) * shape.width + sx as usize;
                                let b = ((c * shape.channels + ci) * shape.kernel_height + kt)
                                    * shape.kernel_width
                                    + kx;
                                sum += f64::from(input[a]) * f64::from(weights[b]);
                            }
                        }
                    }
                }
                let value = sum as f32;
                out[(c * h + t) * shape.width + x] =
                    if shape.relu { value.max(0.0) } else { value };
            }
        }
    }
    Ok(out)
}
pub fn normalize(input: &[f32], parameters: &[f32], channels: usize) -> Result<Vec<f32>> {
    if channels == 0
        || input.is_empty()
        || !input.len().is_multiple_of(channels)
        || parameters.len() != channels * 4
        || parameters[3 * channels..]
            .iter()
            .any(|x| !x.is_finite() || *x < 0.0)
    {
        return Err(Error::new("Invalid batch-normalization tensors"));
    }
    let spatial = input.len() / channels;
    Ok(input
        .iter()
        .enumerate()
        .map(|(i, x)| {
            let c = i / spatial;
            ((f64::from(*x) - f64::from(parameters[2 * channels + c]))
                / (f64::from(parameters[3 * channels + c]) + 0.001).sqrt()
                * f64::from(parameters[c])
                + f64::from(parameters[channels + c])) as f32
        })
        .collect())
}
/// Convert a stored row-major matrix to its row-major transpose.
pub fn transpose_matrix(values: &[f32], rows: usize, columns: usize) -> Result<Vec<f32>> {
    if rows == 0 || columns == 0 || rows.checked_mul(columns) != Some(values.len()) {
        return Err(Error::new("Matrix transpose size mismatch"));
    }
    let mut output = vec![0.0; values.len()];
    for row in 0..rows {
        for column in 0..columns {
            output[column * rows + row] = values[row * columns + column];
        }
    }
    Ok(output)
}
pub fn matvec(weights: &[f32], input: &[f32], rows: usize) -> Result<Vec<f32>> {
    if rows == 0 || input.is_empty() || weights.len() != rows * input.len() {
        return Err(Error::new("Matrix size mismatch"));
    }
    Ok(weights
        .chunks_exact(input.len())
        .map(|r| {
            r.iter()
                .zip(input)
                .map(|(a, b)| f64::from(*a) * f64::from(*b))
                .sum::<f64>() as f32
        })
        .collect())
}
pub fn sigmoid(x: f32) -> f32 {
    if x >= 0.0 {
        1.0 / (1.0 + (-x).exp())
    } else {
        let e = x.exp();
        e / (1.0 + e)
    }
}
pub fn gru_update(
    input_projection: &[f32],
    recurrent_projection: &[f32],
    bias: &[f32],
    old: &[f32],
) -> Result<Vec<f32>> {
    let n = old.len();
    if n == 0
        || input_projection.len() != 3 * n
        || recurrent_projection.len() != 3 * n
        || bias.len() != 6 * n
    {
        return Err(Error::new("GRU tensor size mismatch"));
    }
    let mut out = vec![0.0; n];
    for i in 0..n {
        let r = sigmoid(input_projection[i] + bias[i] + recurrent_projection[i] + bias[3 * n + i]);
        let z = sigmoid(
            input_projection[n + i] + bias[n + i] + recurrent_projection[n + i] + bias[4 * n + i],
        );
        let candidate = (input_projection[2 * n + i]
            + bias[2 * n + i]
            + r * (recurrent_projection[2 * n + i] + bias[5 * n + i]))
            .tanh();
        out[i] = z * old[i] + (1.0 - z) * candidate;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn input_major_projection_preserves_output_columns() {
        // Two input neurons, three output bins: y = x * W, not W * x.
        let weights = transpose_matrix(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 2, 3).unwrap();
        assert_eq!(
            matvec(&weights, &[7.0, 11.0], 3).unwrap(),
            [51.0, 69.0, 87.0]
        );
        assert!(transpose_matrix(&[1.0], 0, 1).is_err());
        assert!(transpose_matrix(&[1.0], usize::MAX, 2).is_err());
    }
    #[test]
    fn convolution_padding_and_channel_order() {
        let s = ConvShape {
            channels: 1,
            height: 3,
            width: 3,
            output_channels: 1,
            kernel_height: 3,
            kernel_width: 3,
            relu: false,
        };
        assert_eq!(
            convolution(&[1.0; 9], &[1.0; 9], &[0.0], s).unwrap(),
            [6.0, 9.0, 6.0]
        );
        assert!(convolution(&[1.0; 8], &[1.0; 9], &[0.0], s).is_err());
    }
    #[test]
    fn normalization_uses_channel_planes() {
        let out = normalize(
            &[2.0, 4.0, 6.0, 8.0],
            &[1.0, 2.0, 0.0, 1.0, 0.0, 0.0, 0.999, 3.999],
            2,
        )
        .unwrap();
        assert_eq!(out, [2.0, 4.0, 7.0, 9.0]);
    }
    #[test]
    fn gru_reset_applies_to_recurrent_candidate_bias() {
        let out = gru_update(
            &[0.0, 0.0, 0.0],
            &[0.0, 0.0, 0.0],
            &[0.0, 0.0, 0.0, 0.0, 0.0, 2.0],
            &[0.0],
        )
        .unwrap();
        assert!((out[0] - 0.5 * 1.0f32.tanh()).abs() < 1e-7);
    }
    #[test]
    fn sigmoid_extremes_remain_finite() {
        assert_eq!(sigmoid(-1000.0), 0.0);
        assert_eq!(sigmoid(1000.0), 1.0);
    }
}
