//! Portable Vulkan compute. Hardware selection is explicit; no silent CPU fallback.
use crate::{
    Error, Result,
    operators::{GRU_BIAS_BLOCKS, GRU_GATES},
    package::Model,
};
use serde::{Deserialize, Serialize};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

const AMD_VENDOR_ID: u32 = 0x1002;
const MAX_OPERATOR_VALUES: u32 = 64 * 1024 * 1024;
pub(crate) const OPERATOR_GROUP_SIZE: u32 = 64;
const READBACK_TIMEOUT: Duration = Duration::from_secs(30);
const CHECK_ERROR_TOLERANCE: f64 = 0.0001;
use wgpu::util::DeviceExt;

#[derive(Clone, Copy)]
#[repr(u32)]
pub(crate) enum Operator {
    Convolution,
    Normalize,
    Activate,
    Gru,
    Concatenate,
    Project,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AdapterInfo {
    pub name: String,
    pub vendor: u32,
    pub device_type: String,
    pub backend: String,
    pub driver: String,
}
impl From<wgpu::AdapterInfo> for AdapterInfo {
    fn from(i: wgpu::AdapterInfo) -> Self {
        Self {
            name: i.name,
            vendor: i.vendor,
            device_type: format!("{:?}", i.device_type),
            backend: format!("{:?}", i.backend),
            driver: format!("{} {}", i.driver, i.driver_info),
        }
    }
}

pub fn adapters() -> Vec<AdapterInfo> {
    instance()
        .enumerate_adapters(wgpu::Backends::VULKAN)
        .into_iter()
        .map(|a| a.get_info().into())
        .collect()
}
fn instance() -> wgpu::Instance {
    wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    })
}

#[derive(Serialize)]
pub struct MatrixCheck {
    pub tensor: String,
    pub rows: usize,
    pub columns: usize,
    pub max_absolute_error: f64,
    pub max_scaled_error: f64,
    pub dispatch_and_readback_ms: f64,
    pub passed: bool,
}
#[derive(Serialize)]
pub struct ComputeReport {
    pub scope: &'static str,
    pub adapter: AdapterInfo,
    pub checks: Vec<MatrixCheck>,
    pub passed: bool,
}

pub fn check_model(model: &Model, selector: Option<&str>) -> Result<ComputeReport> {
    let gpu = Gpu::new(selector)?;
    let mut checks = Vec::new();
    // Matrix checks deliberately operate on flat stored values.
    // Their agreement tests arithmetic and transfer accuracy only;
    // the complete network establishes its own gate and channel layout.
    for (name, rows, cols) in [
        ("noise_gru_1_weights", 768, 320),
        ("noise_gru_1_rweights", 768, 256),
        ("noise_denoise_gru_1_1_weights", 768, 10496),
        ("noise_denoise_gru_1_1_rweights", 768, 256),
        ("noise_denoise_gru_2_1_weights", 1536, 256),
        ("noise_denoise_gru_2_1_rweights", 1536, 512),
        ("noise_denoise_gru_3_1_weights", 3072, 512),
        ("noise_denoise_gru_3_1_rweights", 3072, 1024),
        ("noise_denoise_filter_1_weights", 961, 1024),
    ] {
        let tensor = model.get(name)?;
        if tensor.values.len() != rows * cols {
            return Err(Error::new(format!("Unexpected tensor size: {name}")));
        }
        let x: Vec<f32> = (0..cols)
            .map(|i| (((i * 37 % 101) as f32) - 50.0) / 100.0)
            .collect();
        let start = Instant::now();
        let actual = gpu.matvec(&tensor.values, &x, rows, cols)?;
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        let mut absolute = 0.0f64;
        let mut scaled = 0.0f64;
        let mut finite = true;
        for (row, actual) in tensor.values.chunks_exact(cols).zip(actual) {
            let expected: f64 = row
                .iter()
                .zip(&x)
                .map(|(a, b)| f64::from(*a) * f64::from(*b))
                .sum();
            let err = (f64::from(actual) - expected).abs();
            finite &= actual.is_finite();
            absolute = absolute.max(err);
            scaled = scaled.max(err / (1.0 + expected.abs()));
        }
        checks.push(MatrixCheck {
            tensor: name.into(),
            rows,
            columns: cols,
            max_absolute_error: absolute,
            max_scaled_error: scaled,
            dispatch_and_readback_ms: ms,
            passed: finite && scaled < CHECK_ERROR_TOLERANCE,
        });
    }
    let passed = checks.iter().all(|c| c.passed);
    Ok(ComputeReport {
        scope: "Isolated stored weight-matrix operations; graph and waveform quality are checked separately",
        adapter: gpu.info,
        checks,
        passed,
    })
}

pub(crate) struct Gpu {
    pub(crate) info: AdapterInfo,
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    pub(crate) pipeline: wgpu::ComputePipeline,
    pub(crate) operators: wgpu::ComputePipeline,
}
impl Gpu {
    pub(crate) fn new(selector: Option<&str>) -> Result<Self> {
        let mut adapters = instance().enumerate_adapters(wgpu::Backends::VULKAN);
        adapters.sort_by_key(|a| match a.get_info().device_type {
            wgpu::DeviceType::DiscreteGpu => 0,
            wgpu::DeviceType::IntegratedGpu => 1,
            _ => 2,
        });
        let adapter=adapters.into_iter().find(|a| {
            let i=a.get_info();
            i.vendor==AMD_VENDOR_ID && matches!(i.device_type,wgpu::DeviceType::DiscreteGpu|wgpu::DeviceType::IntegratedGpu)
                && selector.is_none_or(|s| i.name.to_lowercase().contains(&s.to_lowercase()))
        }).ok_or_else(|| Error::new("No matching AMD Vulkan hardware adapter is accessible. Run gpu-info on the host; software Vulkan is deliberately excluded."))?;
        let info = adapter.get_info().into();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("clearwave compute"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .map_err(|e| Error::new(format!("Vulkan device creation failed: {e}")))?;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("weight matrix vector"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/matvec.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("weight matrix vector"),
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("voice operators"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/operators.wgsl").into()),
        });
        let operators = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("voice operators"),
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Ok(Self {
            info,
            device,
            queue,
            pipeline,
            operators,
        })
    }

    pub(crate) fn matvec(
        &self,
        matrix: &[f32],
        x: &[f32],
        rows: usize,
        cols: usize,
    ) -> Result<Vec<f32>> {
        if rows == 0 || cols == 0 || matrix.len() != rows * cols || x.len() != cols {
            return Err(Error::new("Invalid matrix dimensions"));
        }
        let data = |label, contents, usage| {
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some(label),
                    contents,
                    usage,
                })
        };
        let matrix = data(
            "matrix",
            bytemuck::cast_slice(matrix),
            wgpu::BufferUsages::STORAGE,
        );
        let vector = data(
            "input",
            bytemuck::cast_slice(x),
            wgpu::BufferUsages::STORAGE,
        );
        let shape = [rows as u32, cols as u32, 0, 0];
        let shape = data(
            "shape",
            bytemuck::cast_slice(&shape),
            wgpu::BufferUsages::UNIFORM,
        );
        self.dispatch_readback(
            &self.pipeline,
            &[&matrix, &vector, &shape],
            2,
            rows as u32,
            rows,
        )
    }

    pub(crate) fn convolution(
        &self,
        input: &[f32],
        weights: &[f32],
        bias: &[f32],
        shape: crate::operators::ConvShape,
    ) -> Result<Vec<f32>> {
        let out_height = shape.validate(input, weights, bias)?;
        let count = shape.output_channels * out_height * shape.width;
        let mut packed = weights.to_vec();
        packed.extend(bias);
        self.operator(
            [
                Operator::Convolution as u32,
                count as u32,
                shape.channels as u32,
                shape.height as u32,
                shape.width as u32,
                shape.output_channels as u32,
                out_height as u32,
                shape.kernel_height as u32,
                shape.kernel_width as u32,
                (shape.kernel_width / 2) as u32,
                u32::from(shape.relu),
                0,
            ],
            &packed,
            input,
            &[0.0],
        )
    }
    pub(crate) fn normalize(
        &self,
        input: &[f32],
        parameters: &[f32],
        channels: usize,
    ) -> Result<Vec<f32>> {
        let spatial = crate::operators::normalization_spatial(input, parameters, channels)?;
        self.operator(
            [
                Operator::Normalize as u32,
                input.len() as u32,
                channels as u32,
                1,
                spatial as u32,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
            parameters,
            input,
            &[0.0],
        )
    }
    pub(crate) fn activation(&self, input: &[f32], mode: u32) -> Result<Vec<f32>> {
        if input.is_empty() || mode > 2 {
            return Err(Error::new("Invalid activation input or mode"));
        }
        self.operator(
            [
                Operator::Activate as u32,
                input.len() as u32,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                mode,
                0,
            ],
            &[0.0],
            input,
            &[0.0],
        )
    }
    pub(crate) fn gru_update(
        &self,
        x: &[f32],
        h: &[f32],
        bias: &[f32],
        old: &[f32],
    ) -> Result<Vec<f32>> {
        let n = old.len();
        if n == 0
            || x.len() != GRU_GATES * n
            || h.len() != GRU_GATES * n
            || bias.len() != GRU_BIAS_BLOCKS * n
        {
            return Err(Error::new("GRU tensor size mismatch"));
        }
        let mut packed = bias.to_vec();
        packed.extend(old);
        self.operator(
            [Operator::Gru as u32, n as u32, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            &packed,
            x,
            h,
        )
    }
    pub(crate) fn concatenate_features(
        &self,
        noise: &[f32],
        convolution: &[f32],
        channels: usize,
        frequencies: usize,
    ) -> Result<Vec<f32>> {
        if noise.is_empty()
            || channels == 0
            || frequencies == 0
            || channels.checked_mul(frequencies) != Some(convolution.len())
        {
            return Err(Error::new("Concatenation tensor size mismatch"));
        }
        self.operator(
            [
                Operator::Concatenate as u32,
                (noise.len() + convolution.len()) as u32,
                channels as u32,
                noise.len() as u32,
                frequencies as u32,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
            &[0.0],
            noise,
            convolution,
        )
    }
    fn operator(
        &self,
        params: [u32; 12],
        weights: &[f32],
        input: &[f32],
        auxiliary: &[f32],
    ) -> Result<Vec<f32>> {
        let count = params[1];
        if count == 0 || count > MAX_OPERATOR_VALUES {
            return Err(Error::new("Invalid operator output size"));
        }
        let data = |label, contents, usage| {
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some(label),
                    contents,
                    usage,
                })
        };
        let weights = data(
            "operator weights",
            bytemuck::cast_slice(weights),
            wgpu::BufferUsages::STORAGE,
        );
        let input = data(
            "operator input",
            bytemuck::cast_slice(input),
            wgpu::BufferUsages::STORAGE,
        );
        let auxiliary = data(
            "operator auxiliary",
            bytemuck::cast_slice(auxiliary),
            wgpu::BufferUsages::STORAGE,
        );
        let params = data(
            "operator shape",
            bytemuck::cast_slice(&params),
            wgpu::BufferUsages::UNIFORM,
        );
        self.dispatch_readback(
            &self.operators,
            &[&weights, &input, &auxiliary, &params],
            3,
            count.div_ceil(OPERATOR_GROUP_SIZE),
            count as usize,
        )
    }
    fn dispatch_readback(
        &self,
        pipeline: &wgpu::ComputePipeline,
        inputs: &[&wgpu::Buffer],
        output_binding: usize,
        groups: u32,
        count: usize,
    ) -> Result<Vec<f32>> {
        let size = (count * std::mem::size_of::<f32>()) as u64;
        let output = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("compute output"),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("compute readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut buffers = inputs.to_vec();
        buffers.insert(output_binding, &output);
        let entries: Vec<_> = buffers
            .iter()
            .enumerate()
            .map(|(index, buffer)| wgpu::BindGroupEntry {
                binding: index as u32,
                resource: buffer.as_entire_binding(),
            })
            .collect();
        let binding = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("compute bindings"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &entries,
        });

        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("compute dispatch"),
                timestamp_writes: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &binding, &[]);
            pass.dispatch_workgroups(groups, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&output, 0, &staging, 0, size);
        self.queue.submit([encoder.finish()]);
        self.readback(&staging)
    }
    pub(crate) fn readback(&self, staging: &wgpu::Buffer) -> Result<Vec<f32>> {
        let (sender, receiver) = mpsc::channel();
        staging
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        let result = self
            .device
            .poll(wgpu::PollType::Wait)
            .map_err(|error| Error::new(format!("Vulkan poll failed: {error}")))
            .and_then(|_| {
                receiver
                    .recv_timeout(READBACK_TIMEOUT)
                    .map_err(|error| Error::new(format!("GPU readback timed out: {error}")))?
                    .map_err(|error| Error::new(format!("GPU readback failed: {error}")))
            });
        if let Err(error) = result {
            staging.unmap();
            return Err(error);
        }

        let view = staging.slice(..).get_mapped_range();
        let values = bytemuck::cast_slice::<u8, f32>(&view).to_vec();
        drop(view);
        staging.unmap();
        Ok(values)
    }
}
