//! Decode the supported Broadcast 2.3 audio package as local data.
//! Recover named graph tensors from its validated TensorRT weight sections.
//! Vendor libraries are inspected for decoder data and are never executed.
use crate::{
    Error, Result, network, operators,
    package::{Model, ModelProfile, Tensor, decrypt_payload, hash},
    pe,
    storage::bounded_read,
};
use half::f16;
use std::path::Path;

const MAX_BYTES: usize = 128 * 1024 * 1024;
const MODEL_SHA256: &str = "0bfa204a01727c78984f13b7b298d97253d6ab9d1c52033c4ba3bfaa30596410";
const DECODER_SHA256: &str = "4b584d31acd3f1a281c968ac605c738e38953e7d2b1cdb60fca97edd5c83c124";
const KEY_TABLE_RVA: u32 = 0x5c630;
const KEY_BYTES: usize = 32;
const IV_OFFSET: usize = 92;
const IV_BYTES: usize = 16;
const PAYLOAD_SIZE_OFFSET: usize = 116;
const PAYLOAD_OFFSET: usize = 128;
const ENGINE_WEIGHT_OFFSET: usize = 64;
const ENGINE_WEIGHT_SIZE: usize = 72;
const CONVOLUTION_BYTES: usize = 43_648;
const RECURRENT_BYTES: usize = 31_183_360;
const HALF_BYTES: usize = 2;
const PADDED_INPUT_CHANNELS: usize = 8;
const PROJECTION_OFFSET: usize = 2_304;
const PROJECTION_BIAS_OFFSET: usize = 256;

struct ConvolutionLayout {
    weights: usize,
    bias: usize,
    scale: usize,
    shift: usize,
}
const CONVOLUTIONS: [ConvolutionLayout; 3] = [
    ConvolutionLayout {
        weights: 512,
        bias: 0,
        scale: 5_120,
        shift: 5_632,
    },
    ConvolutionLayout {
        weights: 6_656,
        bias: 6_144,
        scale: 18_944,
        shift: 19_456,
    },
    ConvolutionLayout {
        weights: 20_480,
        bias: 19_968,
        scale: CONVOLUTION_BYTES + 30_296_448,
        shift: CONVOLUTION_BYTES + 30_296_320,
    },
];
struct RecurrentLayout {
    input: usize,
    input_column_major: bool,
    recurrent_gates: usize,
    recurrent_candidate: usize,
    recurrent_column_major: bool,
    input_candidate_bias: usize,
    gate_bias: usize,
    recurrent_candidate_bias: usize,
}
const RECURRENT: [RecurrentLayout; 4] = [
    RecurrentLayout {
        input: 30_296_576,
        input_column_major: true,
        recurrent_gates: 30_921_216,
        recurrent_candidate: 30_790_144,
        recurrent_column_major: false,
        input_candidate_bias: 30_788_096,
        gate_bias: 30_788_608,
        recurrent_candidate_bias: 30_789_632,
    },
    RecurrentLayout {
        input: 13_779_200,
        input_column_major: false,
        recurrent_gates: 30_034_176,
        recurrent_candidate: 29_903_104,
        recurrent_column_major: false,
        input_candidate_bias: 29_901_056,
        gate_bias: 29_901_568,
        recurrent_candidate_bias: 29_902_592,
    },
    RecurrentLayout {
        input: 11_415_808,
        input_column_major: true,
        recurrent_gates: 12_205_312,
        recurrent_candidate: 13_254_912,
        recurrent_column_major: false,
        input_candidate_bias: 12_202_240,
        gate_bias: 12_203_264,
        recurrent_candidate_bias: 13_253_888,
    },
    RecurrentLayout {
        input: 1_970_432,
        input_column_major: false,
        recurrent_gates: 5_122_304,
        recurrent_candidate: 9_318_656,
        recurrent_column_major: true,
        input_candidate_bias: 5_116_160,
        gate_bias: 5_118_208,
        recurrent_candidate_bias: 9_316_608,
    },
];

pub fn load(package: &Path, decoder: &Path) -> Result<Model> {
    let package = bounded_read(package, MAX_BYTES)?;
    let decoder = bounded_read(decoder, MAX_BYTES)?;
    let package_sha256 = hash(&package);
    let runtime_sha256 = hash(&decoder);
    if package_sha256 != MODEL_SHA256 || runtime_sha256 != DECODER_SHA256 {
        return Err(Error::new(
            "Unsupported Broadcast audio model revision; expected the validated Broadcast 2.3 noise-removal package",
        ));
    }
    let table = pe::slice_at_rva(&decoder, KEY_TABLE_RVA, KEY_BYTES)?;
    let iv = region(&package, IV_OFFSET, IV_BYTES)?;
    let size = read_u32(&package, PAYLOAD_SIZE_OFFSET)? as usize;
    let payload = decrypt_payload(region(&package, PAYLOAD_OFFSET, size)?, iv, table)?;
    if region(&payload, 0, 4)? != b"ftrt" || read_u64(&payload, 16)? != payload.len() {
        return Err(Error::new("Invalid Broadcast TensorRT payload header"));
    }
    let start = read_u64(&payload, ENGINE_WEIGHT_OFFSET)?;
    let size = read_u64(&payload, ENGINE_WEIGHT_SIZE)?;
    if size != CONVOLUTION_BYTES + RECURRENT_BYTES {
        return Err(Error::new(
            "Unsupported Broadcast weight section dimensions",
        ));
    }
    let weights = region(&payload, start, size)?;
    let model = Model {
        profile: ModelProfile::Broadcast23,
        format: "NVnn v1 / Broadcast 2.3 TensorRT weights",
        tensors: tensors(weights)?,
        package_sha256,
        runtime_sha256: Some(runtime_sha256),
        payload_sha256: hash(&payload),
    };
    network::specification(&model)?;
    Ok(model)
}

fn region(bytes: &[u8], offset: usize, length: usize) -> Result<&[u8]> {
    let end = offset
        .checked_add(length)
        .ok_or_else(|| Error::new("Broadcast offset overflow"))?;
    bytes
        .get(offset..end)
        .ok_or_else(|| Error::new("Truncated Broadcast model data"))
}
fn read_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let value = region(bytes, offset, size_of::<u32>())?;
    Ok(u32::from_le_bytes(
        value
            .try_into()
            .map_err(|_| Error::new("Invalid model size"))?,
    ))
}
fn read_u64(bytes: &[u8], offset: usize) -> Result<usize> {
    let value = region(bytes, offset, size_of::<u64>())?;
    let value = u64::from_le_bytes(
        value
            .try_into()
            .map_err(|_| Error::new("Invalid model offset"))?,
    );
    usize::try_from(value).map_err(|_| Error::new("Model offset exceeds address space"))
}
fn values(bytes: &[u8], offset: usize, count: usize) -> Result<Vec<f32>> {
    let length = count
        .checked_mul(HALF_BYTES)
        .ok_or_else(|| Error::new("Model tensor size overflow"))?;
    let values: Vec<_> = region(bytes, offset, length)?
        .as_chunks::<HALF_BYTES>()
        .0
        .iter()
        .map(|value| f16::from_bits(u16::from_le_bytes(*value)).to_f32())
        .collect();
    if values.iter().any(|value| !value.is_finite()) {
        return Err(Error::new("Broadcast tensor contains non-finite values"));
    }
    Ok(values)
}
fn push(tensors: &mut Vec<Tensor>, name: String, values: Vec<f32>) {
    tensors.push(Tensor { name, values });
}
fn matrix(
    bytes: &[u8],
    offset: usize,
    rows: usize,
    columns: usize,
    column_major: bool,
) -> Result<Vec<f32>> {
    let values = values(bytes, offset, rows * columns)?;
    if column_major {
        operators::transpose_matrix(&values, columns, rows)
    } else {
        Ok(values)
    }
}
fn reset_update_order(values: &[f32], hidden: usize, width: usize) -> Vec<f32> {
    let block = hidden * width;
    let mut output = Vec::with_capacity(values.len());
    output.extend_from_slice(&values[block..2 * block]);
    output.extend_from_slice(&values[..block]);
    output.extend_from_slice(&values[2 * block..]);
    output
}
fn tensors(bytes: &[u8]) -> Result<Vec<Tensor>> {
    let mut tensors = Vec::with_capacity(network::SUPPORTED_TENSOR_COUNT);
    let mut input_channels = 1;
    for (index, layout) in CONVOLUTIONS.iter().enumerate() {
        let output_channels = network::CONVOLUTION_CHANNELS[index];
        let padded = input_channels.max(PADDED_INPUT_CHANNELS);
        let spatial = network::CONVOLUTION_HEIGHT * network::CONVOLUTION_WIDTH;
        let stored = values(bytes, layout.weights, output_channels * spatial * padded)?;
        let mut weights = Vec::with_capacity(output_channels * input_channels * spatial);
        for output in 0..output_channels {
            for input in 0..input_channels {
                for position in 0..spatial {
                    weights.push(stored[(output * spatial + position) * padded + input]);
                }
            }
        }
        let prefix = format!("conv2d_out{}_1", index + 1);
        push(&mut tensors, format!("{prefix}_weights"), weights);
        push(
            &mut tensors,
            format!("{prefix}_bias"),
            values(bytes, layout.bias, output_channels)?,
        );

        // TensorRT stores the folded normalization as channel-wise affine data.
        // Neutral mean/variance preserve that affine operation in the shared graph.
        // Computation uses the shared F32 engine; vendor rounding remains under validation.
        let prefix = format!("batch_normalization_{}", index + 1);
        for (suffix, values) in [
            ("gamma", values(bytes, layout.scale, output_channels)?),
            ("beta", values(bytes, layout.shift, output_channels)?),
            ("mean", vec![0.0; output_channels]),
            (
                "var",
                vec![1.0 - operators::NORMALIZATION_EPSILON as f32; output_channels],
            ),
        ] {
            push(&mut tensors, format!("{prefix}_{suffix}"), values);
        }
        input_channels = output_channels;
    }
    let recurrent = region(bytes, CONVOLUTION_BYTES, RECURRENT_BYTES)?;
    for (layout, (name, input, hidden)) in RECURRENT.iter().zip(network::GRUS) {
        let input_weights = matrix(
            recurrent,
            layout.input,
            operators::GRU_GATES * hidden,
            input,
            layout.input_column_major,
        )?;
        push(
            &mut tensors,
            format!("{name}_weights"),
            reset_update_order(&input_weights, hidden, input),
        );
        let mut recurrent_weights = matrix(
            recurrent,
            layout.recurrent_gates,
            2 * hidden,
            hidden,
            layout.recurrent_column_major,
        )?;
        recurrent_weights.extend(matrix(
            recurrent,
            layout.recurrent_candidate,
            hidden,
            hidden,
            false,
        )?);
        push(
            &mut tensors,
            format!("{name}_rweights"),
            reset_update_order(&recurrent_weights, hidden, hidden),
        );

        let gate_bias = values(recurrent, layout.gate_bias, 2 * hidden)?;
        let gate_bias = reset_update_order(&gate_bias, hidden, 1);
        let mut bias = Vec::with_capacity(operators::GRU_BIAS_BLOCKS * hidden);
        bias.extend_from_slice(&gate_bias);
        bias.extend(values(recurrent, layout.input_candidate_bias, hidden)?);
        bias.extend_from_slice(&gate_bias);
        bias.extend(values(recurrent, layout.recurrent_candidate_bias, hidden)?);
        push(&mut tensors, format!("{name}_bias"), bias);
    }
    push(
        &mut tensors,
        "noise_denoise_filter_1_weights".into(),
        matrix(
            recurrent,
            PROJECTION_OFFSET,
            network::PROJECTION_INPUTS,
            network::GAINS,
            true,
        )?,
    );
    push(
        &mut tensors,
        "noise_denoise_filter_1_bias".into(),
        values(recurrent, PROJECTION_BIAS_OFFSET, network::GAINS)?,
    );
    Ok(tensors)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounds_and_half_decoding_reject_invalid_data() {
        assert!(region(&[], usize::MAX, 2).is_err());
        assert!(values(&[0], 0, 1).is_err());
        assert!(values(&f16::NAN.to_bits().to_le_bytes(), 0, 1).is_err());
        let bytes: Vec<_> = [f16::from_f32(1.5), f16::from_f32(-0.25)]
            .into_iter()
            .flat_map(|value| value.to_bits().to_le_bytes())
            .collect();
        assert_eq!(values(&bytes, 0, 2).unwrap(), [1.5, -0.25]);
    }
    #[test]
    fn gate_permutation_preserves_candidate_and_swaps_update_reset() {
        assert_eq!(
            reset_update_order(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 2, 1),
            [3.0, 4.0, 1.0, 2.0, 5.0, 6.0]
        );
    }
}
