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
const GRUS: [(&str, usize, usize); 4] = [
    ("noise_gru_1", 320, 256),
    ("noise_denoise_gru_1_1", 10496, 256),
    ("noise_denoise_gru_2_1", 256, 512),
    ("noise_denoise_gru_3_1", 512, 1024),
];

#[derive(Serialize)]
pub struct LayerSpec {
    pub name: String,
    pub input: Vec<usize>,
    pub output: Vec<usize>,
    pub operation: &'static str,
}
#[derive(Serialize)]
pub struct GraphSpec {
    pub input_context: usize,
    pub feature_bins: usize,
    pub output_gains: usize,
    pub layers: Vec<LayerSpec>,
    pub layout: &'static str,
    pub vendor_output_parity: &'static str,
}
pub fn specification(model: &Model) -> Result<GraphSpec> {
    let mut layers = Vec::new();
    let mut channels = 1;
    let mut height = 7;
    for (i, output) in [16, 24, 32].into_iter().enumerate() {
        let prefix = format!("conv2d_out{}_1", i + 1);
        check_size(
            model,
            &format!("{prefix}_weights"),
            output * channels * 3 * 5,
        )?;
        check_size(model, &format!("{prefix}_bias"), output)?;
        for suffix in ["gamma", "beta", "mean", "var"] {
            check_size(
                model,
                &format!("batch_normalization_{}_{suffix}", i + 1),
                output,
            )?;
        }
        layers.push(LayerSpec{name:prefix,input:vec![channels,height,FEATURES],output:vec![output,height-2,FEATURES],operation:"3x5 cross-correlation, frequency padding 2, ReLU, batch normalization epsilon 0.001"});
        channels = output;
        height -= 2;
    }
    for (name, input, hidden) in GRUS {
        check_size(model, &format!("{name}_weights"), 3 * hidden * input)?;
        check_size(model, &format!("{name}_rweights"), 3 * hidden * hidden)?;
        check_size(model, &format!("{name}_bias"), 6 * hidden)?;
        layers.push(LayerSpec{name:name.into(),input:vec![input],output:vec![hidden],operation:"stateful reset-after GRU, reset/update/new gates, separate input/recurrent bias"});
    }
    check_size(model, "noise_denoise_filter_1_weights", GAINS * 1024)?;
    check_size(model, "noise_denoise_filter_1_bias", GAINS)?;
    layers.push(LayerSpec {
        name: "noise_denoise_filter_1".into(),
        input: vec![1024],
        output: vec![GAINS],
        operation: "input-major stored projection, transposed once for matvec, dense + sigmoid",
    });
    Ok(GraphSpec {
        input_context: CONTEXT,
        feature_bins: FEATURES,
        output_gains: GAINS,
        layers,
        layout: "Convolutions NCHW/OIHW; recurrent input is center frame; concatenate recurrent output then frequency-major convolution features",
        vendor_output_parity: "not established; offline waveform adapter implemented with documented boundaries",
    })
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
/// Execution kernels consume [output row, input column]; convert once at setup.
pub(crate) fn projection_weights(model: &Model) -> Result<Vec<f32>> {
    operators::transpose_matrix(
        &model.get("noise_denoise_filter_1_weights")?.values,
        1024,
        GAINS,
    )
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
    state: [Vec<f32>; 4],
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
        // State is committed only after every operation has succeeded.
        let mut state = self.state.clone();
        let mut x = context.to_vec();
        let mut channels = 1;
        let mut height = CONTEXT;
        for (i, output_channels) in [16, 24, 32].into_iter().enumerate() {
            let shape = ConvShape {
                channels,
                height,
                width: FEATURES,
                output_channels,
                kernel_height: 3,
                kernel_width: 5,
                relu: true,
            };
            x = self.backend.convolution(
                &x,
                self.values(&format!("conv2d_out{}_1_weights", i + 1))?,
                self.values(&format!("conv2d_out{}_1_bias", i + 1))?,
                shape,
            )?;
            let mut p = Vec::with_capacity(output_channels * 4);
            for suffix in ["gamma", "beta", "mean", "var"] {
                p.extend(self.values(&format!("batch_normalization_{}_{suffix}", i + 1))?);
            }
            x = self.backend.normalize(&x, &p, output_channels)?;
            channels = output_channels;
            height -= 2;
        }
        let mut combined = self.gru(0, &context[3 * FEATURES..4 * FEATURES], &mut state)?;
        // The final convolution has one time row; flatten with channels innermost.
        for f in 0..FEATURES {
            for c in 0..32 {
                combined.push(x[c * FEATURES + f]);
            }
        }
        let mut x = combined;
        for i in 1..4 {
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
    fn gru(&self, index: usize, x: &[f32], state: &mut [Vec<f32>; 4]) -> Result<Vec<f32>> {
        let (name, input, hidden) = GRUS[index];
        if x.len() != input {
            return Err(Error::new(format!("Incorrect input width for {name}")));
        }
        let a = self
            .backend
            .matvec(self.values(&format!("{name}_weights"))?, x, 3 * hidden)?;
        let b = self.backend.matvec(
            self.values(&format!("{name}_rweights"))?,
            &state[index],
            3 * hidden,
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
    let mut channels = 1;
    let mut height = 7;
    let mut x: Vec<f32> = (0..CONTEXT * FEATURES)
        .map(|i| (i % 97) as f32 / 9700.0)
        .collect();
    for (i, output_channels) in [16, 24, 32].into_iter().enumerate() {
        let shape = ConvShape {
            channels,
            height,
            width: FEATURES,
            output_channels,
            kernel_height: 3,
            kernel_width: 5,
            relu: true,
        };
        let w = &model.get(&format!("conv2d_out{}_1_weights", i + 1))?.values;
        let b = &model.get(&format!("conv2d_out{}_1_bias", i + 1))?.values;
        let a = gpu.convolution(&x, w, b, shape)?;
        let e = cpu.convolution(&x, w, b, shape)?;
        checks.push(compare(format!("convolution_{}", i + 1), &a, &e));
        let mut p = Vec::new();
        for suffix in ["gamma", "beta", "mean", "var"] {
            p.extend(
                &model
                    .get(&format!("batch_normalization_{}_{suffix}", i + 1))?
                    .values,
            );
        }
        let a = gpu.normalize(&e, &p, output_channels)?;
        let e = cpu.normalize(&e, &p, output_channels)?;
        checks.push(compare(format!("batch_normalization_{}", i + 1), &a, &e));
        x = e;
        channels = output_channels;
        height -= 2;
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
            let a = cpu.matvec(w, &x, hidden * 3)?;
            let b = cpu.matvec(rw, &state, hidden * 3)?;
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
