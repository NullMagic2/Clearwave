//! Feature-level network execution producing raw noise-mask values.
use crate::{
    Error, Result,
    gpu::Gpu,
    operators::{self, ConvShape},
    package::Model,
    resident::ResidentGraph,
};
use serde::Serialize;

pub const FEATURES: usize = 320;
pub const CONTEXT: usize = 7;
pub const GAINS: usize = 961;
pub(crate) const SUPPORTED_TENSOR_COUNT: usize = 32;
pub(crate) const CENTER_FRAME: usize = CONTEXT / 2;
pub(crate) const CONVOLUTION_CHANNELS: [usize; 3] = [16, 24, 32];
pub(crate) const CONVOLUTION_HEIGHT: usize = 3;
pub(crate) const CONVOLUTION_WIDTH: usize = 5;
pub(crate) const PROJECTION_INPUTS: usize = 1024;
pub(crate) const NOISE_HIDDEN: usize = 256;
pub(crate) const CONVOLUTION_FEATURES: usize = CONVOLUTION_CHANNELS[2] * FEATURES;
pub(crate) const COMBINED_FEATURES: usize = NOISE_HIDDEN + CONVOLUTION_FEATURES;
pub(crate) const GRUS: [(&str, usize, usize); 4] = [
    ("noise_gru_1", FEATURES, NOISE_HIDDEN),
    ("noise_denoise_gru_1_1", COMBINED_FEATURES, 256),
    ("noise_denoise_gru_2_1", 256, 512),
    ("noise_denoise_gru_3_1", 512, PROJECTION_INPUTS),
];

#[derive(Serialize)]
pub struct LayerSpec {
    pub name: String,
    pub sources: Vec<String>,
    pub input: Vec<usize>,
    pub output: Vec<usize>,
    pub operation: &'static str,
}
#[derive(Serialize)]
pub struct TensorSpec {
    pub name: String,
    pub stored_shape: Vec<usize>,
    pub layout: &'static str,
}
#[derive(Serialize)]
pub struct GraphSpec {
    pub input_context: usize,
    pub feature_bins: usize,
    pub output_gains: usize,
    pub layers: Vec<LayerSpec>,
    pub tensors: Vec<TensorSpec>,
    pub layout: &'static str,
    pub vendor_output_parity: &'static str,
}
pub fn specification(model: &Model) -> Result<GraphSpec> {
    let graph = definition();
    for tensor in &graph.tensors {
        check_size(model, &tensor.name, tensor.stored_shape.iter().product())?;
    }
    Ok(graph)
}

fn definition() -> GraphSpec {
    let mut layers = Vec::new();
    let mut tensors = Vec::new();
    let mut channels = 1;
    let mut height = CONTEXT;
    let mut source = "feature_context".to_owned();

    for (i, output) in CONVOLUTION_CHANNELS.into_iter().enumerate() {
        let prefix = format!("conv2d_out{}_1", i + 1);
        tensors.push(TensorSpec {
            name: format!("{prefix}_weights"),
            stored_shape: vec![output, channels, CONVOLUTION_HEIGHT, CONVOLUTION_WIDTH],
            layout: "output, input, time, frequency",
        });
        tensors.push(TensorSpec {
            name: format!("{prefix}_bias"),
            stored_shape: vec![output],
            layout: "output channel",
        });

        for suffix in ["gamma", "beta", "mean", "var"] {
            tensors.push(TensorSpec {
                name: format!("batch_normalization_{}_{suffix}", i + 1),
                stored_shape: vec![output],
                layout: "channel",
            });
        }
        height -= CONVOLUTION_HEIGHT - 1;
        let dimensions = vec![output, height, FEATURES];
        layers.push(LayerSpec {
            name: prefix.clone(),
            sources: vec![source],
            input: vec![channels, height + CONVOLUTION_HEIGHT - 1, FEATURES],
            output: dimensions.clone(),
            operation: "3x5 cross-correlation, stride 1, time padding 0, frequency padding 2, bias, ReLU",
        });
        source = format!("batch_normalization_{}", i + 1);
        layers.push(LayerSpec {
            name: source.clone(),
            sources: vec![prefix],
            input: dimensions.clone(),
            output: dimensions,
            operation: "per-channel inference normalization, gamma/beta/mean/variance, epsilon 0.001; no following ReLU",
        });
        channels = output;
    }
    layers.push(LayerSpec {
        name: "convolution_features".into(),
        sources: vec![source],
        input: vec![channels, height, FEATURES],
        output: vec![CONVOLUTION_FEATURES],
        operation: "permute channel/time/frequency to time/frequency/channel, then flatten",
    });
    layers.push(LayerSpec {
        name: "center_frame".into(),
        sources: vec!["feature_context".into()],
        input: vec![CONTEXT, FEATURES],
        output: vec![FEATURES],
        operation: "select context index 3, aligned with the center of the convolution receptive field",
    });

    for (index, (name, input, hidden)) in GRUS.into_iter().enumerate() {
        if index == 1 {
            layers.push(LayerSpec {
                name: "combined_features".into(),
                sources: vec![GRUS[0].0.into(), "convolution_features".into()],
                input: vec![NOISE_HIDDEN, CONVOLUTION_FEATURES],
                output: vec![COMBINED_FEATURES],
                operation: "concatenate noise hidden output first, then frequency-major convolution features",
            });
        }
        for (suffix, shape, layout) in [
            (
                "weights",
                vec![operators::GRU_GATES, hidden, input],
                "reset/update/new, output, input",
            ),
            (
                "rweights",
                vec![operators::GRU_GATES, hidden, hidden],
                "reset/update/new, output, previous hidden",
            ),
            (
                "bias",
                vec![2, operators::GRU_GATES, hidden],
                "input/recurrent, reset/update/new, hidden",
            ),
        ] {
            tensors.push(TensorSpec {
                name: format!("{name}_{suffix}"),
                stored_shape: shape,
                layout,
            });
        }
        let source = match index {
            0 => "center_frame",
            1 => "combined_features",
            _ => GRUS[index - 1].0,
        };
        layers.push(LayerSpec {
            name: name.into(),
            sources: vec![source.into()],
            input: vec![input],
            output: vec![hidden],
            operation: "stateful reset-after GRU, reset/update/new gates, separate input/recurrent bias, zero initial hidden",
        });
    }
    tensors.push(TensorSpec {
        name: "noise_denoise_filter_1_weights".into(),
        stored_shape: vec![PROJECTION_INPUTS, GAINS],
        layout: "input neuron, output frequency; transpose once for execution",
    });
    tensors.push(TensorSpec {
        name: "noise_denoise_filter_1_bias".into(),
        stored_shape: vec![GAINS],
        layout: "output frequency",
    });
    layers.push(LayerSpec {
        name: "noise_denoise_filter_1".into(),
        sources: vec![GRUS[GRUS.len() - 1].0.into()],
        input: vec![PROJECTION_INPUTS],
        output: vec![GAINS],
        operation: "input-major stored projection, transposed once for matvec, dense + sigmoid",
    });
    GraphSpec {
        input_context: CONTEXT,
        feature_bins: FEATURES,
        output_gains: GAINS,
        layers,
        tensors,
        layout: "Convolutions NCHW/OIHW; recurrent input is center frame; concatenate recurrent output then frequency-major convolution features",
        vendor_output_parity: "not established; offline waveform adapter implemented with documented boundaries",
    }
}
fn check_size(model: &Model, name: &str, size: usize) -> Result<()> {
    let t = model.get(name)?;
    if t.values.len() != size {
        return Err(Error::new(format!(
            "Unexpected shape for {name}: {} values, expected {size}",
            t.values.len()
        )));
    }
    Ok(())
}

/// The stored output projection is [input neuron, frequency bin].
/// Execution kernels consume [output row, input column].
/// Convert once at setup so CPU and GPU share the same matrix contract.
pub(crate) fn projection_weights(model: &Model) -> Result<Vec<f32>> {
    operators::transpose_matrix(
        &model.get("noise_denoise_filter_1_weights")?.values,
        PROJECTION_INPUTS,
        GAINS,
    )
}

pub(crate) fn normalization_parameters(model: &Model, layer: usize) -> Result<Vec<f32>> {
    let mut parameters = Vec::new();
    for suffix in ["gamma", "beta", "mean", "var"] {
        parameters.extend_from_slice(
            &model
                .get(&format!("batch_normalization_{}_{suffix}", layer + 1))?
                .values,
        );
    }
    Ok(parameters)
}

pub enum Backend {
    Cpu,
    Gpu(Box<Gpu>),
}
impl Backend {
    pub fn amd(selector: Option<&str>) -> Result<Self> {
        Ok(Self::Gpu(Box::new(Gpu::new(selector)?)))
    }
    pub fn name(&self) -> String {
        match self {
            Self::Cpu => "CPU reference".into(),
            Self::Gpu(g) => g.info.name.clone(),
        }
    }
    pub fn matvec(&self, w: &[f32], x: &[f32], rows: usize) -> Result<Vec<f32>> {
        match self {
            Self::Cpu => operators::matvec(w, x, rows),
            Self::Gpu(g) => g.matvec(w, x, rows, x.len()),
        }
    }
    pub fn convolution(&self, x: &[f32], w: &[f32], b: &[f32], s: ConvShape) -> Result<Vec<f32>> {
        match self {
            Self::Cpu => operators::convolution(x, w, b, s),
            Self::Gpu(g) => g.convolution(x, w, b, s),
        }
    }
    pub fn normalize(&self, x: &[f32], p: &[f32], c: usize) -> Result<Vec<f32>> {
        match self {
            Self::Cpu => operators::normalize(x, p, c),
            Self::Gpu(g) => g.normalize(x, p, c),
        }
    }
    pub fn activation(&self, x: &[f32], mode: u32) -> Result<Vec<f32>> {
        match self {
            Self::Cpu => Ok(x
                .iter()
                .map(|x| match mode {
                    0 => x.max(0.0),
                    1 => operators::sigmoid(*x),
                    _ => x.tanh(),
                })
                .collect()),
            Self::Gpu(g) => g.activation(x, mode),
        }
    }
    pub fn gru_update(&self, x: &[f32], h: &[f32], b: &[f32], s: &[f32]) -> Result<Vec<f32>> {
        match self {
            Self::Cpu => operators::gru_update(x, h, b, s),
            Self::Gpu(g) => g.gru_update(x, h, b, s),
        }
    }
}

pub struct Network<'a> {
    model: &'a Model,
    pub backend: Backend,
    state: [Vec<f32>; GRUS.len()],
    resident: Option<ResidentGraph>,
    projection: Vec<f32>,
    steps: usize,
}
impl<'a> Network<'a> {
    pub fn new(model: &'a Model, backend: Backend) -> Result<Self> {
        specification(model)?;
        let resident = match &backend {
            Backend::Gpu(gpu) => Some(ResidentGraph::new(gpu, model)?),
            Backend::Cpu => None,
        };
        let projection = if resident.is_none() {
            projection_weights(model)?
        } else {
            Vec::new()
        };
        Ok(Self {
            model,
            backend,
            state: GRUS.map(|(_, _, h)| vec![0.0; h]),
            resident,
            projection,
            steps: 0,
        })
    }
    pub fn reset(&mut self) {
        if let (Some(graph), Backend::Gpu(gpu)) = (&self.resident, &self.backend) {
            graph.reset(gpu);
        }
        for state in &mut self.state {
            state.fill(0.0);
        }
        self.steps = 0;
    }
    pub fn steps(&self) -> usize {
        self.steps
    }
    pub fn execution_mode(&self) -> &'static str {
        if self.resident.is_some() {
            "resident Vulkan graph"
        } else {
            "CPU reference"
        }
    }
    pub fn resident_parameter_bytes(&self) -> usize {
        self.resident.as_ref().map_or(0, |graph| graph.weight_bytes)
    }
    pub fn step(&mut self, context: &[f32]) -> Result<Vec<f32>> {
        if context.len() != CONTEXT * FEATURES || context.iter().any(|x| !x.is_finite()) {
            return Err(Error::new(
                "Expected seven finite feature frames of 320 values",
            ));
        }
        if let (Some(graph), Backend::Gpu(gpu)) = (&self.resident, &self.backend) {
            let output = graph.step(gpu, context)?;
            self.steps += 1;
            return Ok(output);
        }
        // Stage candidate states until every operator and output check succeeds.
        // Failures must preserve the previous context for deterministic retries.
        // The resident GPU graph follows the same commit boundary.
        let mut state = self.state.clone();
        let mut x = context.to_vec();
        let mut channels = 1;
        let mut height = CONTEXT;
        for (i, output_channels) in CONVOLUTION_CHANNELS.into_iter().enumerate() {
            let shape = ConvShape {
                channels,
                height,
                width: FEATURES,
                output_channels,
                kernel_height: CONVOLUTION_HEIGHT,
                kernel_width: CONVOLUTION_WIDTH,
                relu: true,
            };
            x = self.backend.convolution(
                &x,
                self.values(&format!("conv2d_out{}_1_weights", i + 1))?,
                self.values(&format!("conv2d_out{}_1_bias", i + 1))?,
                shape,
            )?;
            let p = normalization_parameters(self.model, i)?;
            x = self.backend.normalize(&x, &p, output_channels)?;
            channels = output_channels;
            height -= CONVOLUTION_HEIGHT - 1;
        }
        let center = &context[CENTER_FRAME * FEATURES..(CENTER_FRAME + 1) * FEATURES];
        let noise = self.gru(0, center, &mut state)?;
        let combined = operators::concatenate_features(&noise, &x, channels, FEATURES)?;
        let mut x = combined;
        for i in 1..GRUS.len() {
            x = self.gru(i, &x, &mut state)?;
        }
        let mut logits = self.backend.matvec(&self.projection, &x, GAINS)?;
        for (v, b) in logits
            .iter_mut()
            .zip(self.values("noise_denoise_filter_1_bias")?)
        {
            *v += b;
        }
        let gains = self.backend.activation(&logits, 1)?;
        if gains
            .iter()
            .any(|x| !x.is_finite() || !(0.0..=1.0).contains(x))
        {
            return Err(Error::new("Network produced invalid gains"));
        }
        self.state = state;
        self.steps += 1;
        Ok(gains)
    }
    fn values(&self, name: &str) -> Result<&[f32]> {
        Ok(&self.model.get(name)?.values)
    }
    fn gru(&self, index: usize, x: &[f32], state: &mut [Vec<f32>; GRUS.len()]) -> Result<Vec<f32>> {
        let (name, input, hidden) = GRUS[index];
        if x.len() != input {
            return Err(Error::new(format!("Incorrect input width for {name}")));
        }
        let a = self.backend.matvec(
            self.values(&format!("{name}_weights"))?,
            x,
            operators::GRU_GATES * hidden,
        )?;
        let b = self.backend.matvec(
            self.values(&format!("{name}_rweights"))?,
            &state[index],
            operators::GRU_GATES * hidden,
        )?;
        let out = self.backend.gru_update(
            &a,
            &b,
            self.values(&format!("{name}_bias"))?,
            &state[index],
        )?;
        state[index] = out.clone();
        Ok(out)
    }
}

#[derive(Serialize)]
pub struct Check {
    pub name: String,
    pub values: usize,
    pub max_scaled_error: f64,
    pub passed: bool,
}
#[derive(Serialize)]
pub struct OperatorReport {
    pub adapter: String,
    pub execution_mode: &'static str,
    pub checks: Vec<Check>,
    pub passed: bool,
    pub scope: &'static str,
}
fn compare(name: impl Into<String>, actual: &[f32], expected: &[f32]) -> Check {
    let mut error = 0.0f64;
    let finite = actual.len() == expected.len() && actual.iter().all(|x| x.is_finite());
    for (a, b) in actual.iter().zip(expected) {
        error = error.max((f64::from(*a) - f64::from(*b)).abs() / (1.0 + f64::from(*b).abs()));
    }
    Check {
        name: name.into(),
        values: actual.len(),
        max_scaled_error: error,
        passed: finite && error < 0.0001,
    }
}
pub fn check_operators(model: &Model, selector: Option<&str>) -> Result<OperatorReport> {
    specification(model)?;
    let gpu = Backend::amd(selector)?;
    let cpu = Backend::Cpu;
    let mut checks = Vec::new();

    if let Backend::Gpu(device) = &gpu {
        let noise = [90.0, 91.0];
        let convolution = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        checks.push(compare(
            "asymmetric_concatenation_layout",
            &device.concatenate_features(&noise, &convolution, 2, 3)?,
            &[90.0, 91.0, 1.0, 4.0, 2.0, 5.0, 3.0, 6.0],
        ));
    }
    let activated = gpu.convolution(
        &[-3.0, 2.0],
        &[1.0],
        &[0.0],
        ConvShape {
            channels: 1,
            height: 1,
            width: 2,
            output_channels: 1,
            kernel_height: 1,
            kernel_width: 1,
            relu: true,
        },
    )?;
    checks.push(compare(
        "relu_before_normalization_preserves_negative_normalized_values",
        &gpu.normalize(&activated, &[2.0, -1.0, 1.0, 0.999], 1)?,
        &[-3.0, 1.0],
    ));
    let mut channels = 1;
    let mut height = CONTEXT;
    let mut x: Vec<f32> = (0..CONTEXT * FEATURES)
        .map(|i| (i % 97) as f32 / 9700.0)
        .collect();
    for (i, output_channels) in CONVOLUTION_CHANNELS.into_iter().enumerate() {
        let shape = ConvShape {
            channels,
            height,
            width: FEATURES,
            output_channels,
            kernel_height: CONVOLUTION_HEIGHT,
            kernel_width: CONVOLUTION_WIDTH,
            relu: true,
        };
        let w = &model.get(&format!("conv2d_out{}_1_weights", i + 1))?.values;
        let b = &model.get(&format!("conv2d_out{}_1_bias", i + 1))?.values;
        let a = gpu.convolution(&x, w, b, shape)?;
        let e = cpu.convolution(&x, w, b, shape)?;
        checks.push(compare(format!("convolution_{}", i + 1), &a, &e));
        let p = normalization_parameters(model, i)?;
        let a = gpu.normalize(&e, &p, output_channels)?;
        let e = cpu.normalize(&e, &p, output_channels)?;
        checks.push(compare(format!("batch_normalization_{}", i + 1), &a, &e));
        x = e;
        channels = output_channels;
        height -= CONVOLUTION_HEIGHT - 1;
    }
    for mode in 0..3 {
        let x = [-1000.0, -40.0, -1.0, 0.0, 1.0, 40.0, 1000.0];
        checks.push(compare(
            format!("activation_{mode}"),
            &gpu.activation(&x, mode)?,
            &cpu.activation(&x, mode)?,
        ));
    }
    for (name, input, hidden) in GRUS {
        let w = &model.get(&format!("{name}_weights"))?.values;
        let rw = &model.get(&format!("{name}_rweights"))?.values;
        let bias = &model.get(&format!("{name}_bias"))?.values;
        let mut state = vec![0.0; hidden];
        for step in 0..3 {
            let x: Vec<f32> = (0..input)
                .map(|i| ((i * 17 + step * 7) % 71) as f32 / 7100.0)
                .collect();
            let a = cpu.matvec(w, &x, hidden * operators::GRU_GATES)?;
            let b = cpu.matvec(rw, &state, hidden * operators::GRU_GATES)?;
            let expected = cpu.gru_update(&a, &b, bias, &state)?;
            let actual = gpu.gru_update(&a, &b, bias, &state)?;
            checks.push(compare(format!("{name}_update_{step}"), &actual, &expected));
            state = expected;
        }
    }
    let mut reference = Network::new(model, Backend::Cpu)?;
    let mut actual = Network::new(model, gpu)?;
    for step in 0..3 {
        let context: Vec<f32> = (0..CONTEXT * FEATURES)
            .map(|i| ((i * 19 + step * 13) % 83) as f32 / 8300.0)
            .collect();
        checks.push(compare(
            format!("feature_graph_frame_{step}"),
            &actual.step(&context)?,
            &reference.step(&context)?,
        ));
    }
    let fresh = actual.step(&[0.0; CONTEXT * FEATURES])?;
    actual.reset();
    let reset = actual.step(&[0.0; CONTEXT * FEATURES])?;
    let mut clean = Network::new(model, Backend::Cpu)?;
    checks.push(compare(
        "state_reset",
        &reset,
        &clean.step(&[0.0; CONTEXT * FEATURES])?,
    ));
    let state_changed = fresh.iter().zip(&reset).any(|(a, b)| (a - b).abs() > 1e-6);
    checks.push(Check {
        name: "state_changes_across_frames".into(),
        values: GAINS,
        max_scaled_error: 0.0,
        passed: state_changed && actual.steps() == 1,
    });
    let before = actual.steps();
    let invalid_rejected = actual.step(&[0.0; FEATURES]).is_err()
        && actual.step(&[f32::NAN; CONTEXT * FEATURES]).is_err();
    checks.push(Check {
        name: "invalid_context_retains_state".into(),
        values: 2,
        max_scaled_error: 0.0,
        passed: invalid_rejected && actual.steps() == before,
    });
    checks.push(compare(
        "state_after_rejected_context",
        &actual.step(&[0.0; CONTEXT * FEATURES])?,
        &clean.step(&[0.0; CONTEXT * FEATURES])?,
    ));
    let passed = checks.iter().all(|c| c.passed);
    Ok(OperatorReport {
        adapter: actual.backend.name(),
        execution_mode: actual.execution_mode(),
        checks,
        passed,
        scope: "CPU/AMD agreement for operators and feature graph; vendor audio-output parity is pending",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn graph_defines_every_connection_and_stored_tensor() {
        let graph = definition();
        let mut available: HashSet<&str> = HashSet::from(["feature_context"]);
        for layer in &graph.layers {
            assert!(!layer.sources.is_empty());
            assert!(
                layer
                    .sources
                    .iter()
                    .all(|source| available.contains(source.as_str()))
            );
            assert!(available.insert(&layer.name));
        }
        assert_eq!(graph.tensors.len(), SUPPORTED_TENSOR_COUNT);
        let unique: HashSet<_> = graph.tensors.iter().map(|tensor| &tensor.name).collect();
        assert_eq!(unique.len(), graph.tensors.len());

        let projection = graph
            .tensors
            .iter()
            .find(|tensor| tensor.name == "noise_denoise_filter_1_weights")
            .unwrap();
        assert_eq!(projection.stored_shape, [PROJECTION_INPUTS, GAINS]);
        let combination = graph
            .layers
            .iter()
            .find(|layer| layer.name == "combined_features")
            .unwrap();
        assert_eq!(combination.sources, [GRUS[0].0, "convolution_features"]);
        assert_eq!(combination.output, [GRUS[1].1]);
        assert_eq!(GRUS[GRUS.len() - 1].2, PROJECTION_INPUTS);
        assert_eq!(
            CONTEXT - CONVOLUTION_CHANNELS.len() * (CONVOLUTION_HEIGHT - 1),
            1
        );
    }
}
