//! Persistent GPU feature graph: one context upload and one mask readback per step.
use crate::{
    Error, Result,
    gpu::Gpu,
    network::{CONTEXT, FEATURES, GAINS},
    package::Model,
};
use wgpu::util::DeviceExt;

enum Step {
    Copy {
        source: usize,
        destination: usize,
        source_offset: u64,
        destination_offset: u64,
        bytes: u64,
    },
    Dispatch {
        matrix: bool,
        binding: wgpu::BindGroup,
        groups: u32,
    },
}
struct Builder<'a> {
    gpu: &'a Gpu,
    model: &'a Model,
    buffers: Vec<wgpu::Buffer>,
    steps: Vec<Step>,
    states: Vec<(usize, usize)>,
    weight_bytes: usize,
}
impl Builder<'_> {
    fn data(&mut self, label: &str, values: &[f32], weights: bool) -> usize {
        if weights {
            self.weight_bytes += values.len() * 4;
        }
        let buffer = self
            .gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(values),
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
            });
        let index = self.buffers.len();
        self.buffers.push(buffer);
        index
    }
    fn empty(&mut self, label: &str, count: usize) -> usize {
        self.data(label, &vec![0.0; count], false)
    }
    fn weight(&mut self, name: &str) -> Result<usize> {
        let values = &self.model.get(name)?.values;
        Ok(self.data(name, values, true))
    }
    fn uniform(&mut self, values: &[u32]) -> usize {
        let buffer = self
            .gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("resident operation dimensions"),
                contents: bytemuck::cast_slice(values),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let index = self.buffers.len();
        self.buffers.push(buffer);
        index
    }
    fn dispatch(&mut self, matrix: bool, buffers: &[usize], groups: u32) {
        let pipeline = if matrix {
            &self.gpu.pipeline
        } else {
            &self.gpu.operators
        };
        let entries: Vec<_> = buffers
            .iter()
            .enumerate()
            .map(|(binding, index)| wgpu::BindGroupEntry {
                binding: binding as u32,
                resource: self.buffers[*index].as_entire_binding(),
            })
            .collect();
        let binding = self
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("resident graph bindings"),
                layout: &pipeline.get_bind_group_layout(0),
                entries: &entries,
            });
        self.steps.push(Step::Dispatch {
            matrix,
            binding,
            groups,
        });
    }
    fn matvec(&mut self, name: &str, input: usize, rows: usize, columns: usize) -> Result<usize> {
        let weights = self.weight(name)?;
        Ok(self.matvec_buffer(weights, input, rows, columns))
    }
    fn matvec_buffer(
        &mut self,
        weights: usize,
        input: usize,
        rows: usize,
        columns: usize,
    ) -> usize {
        let output = self.empty("resident matrix result", rows);
        let shape = self.uniform(&[rows as u32, columns as u32, 0, 0]);
        self.dispatch(true, &[weights, input, output, shape], rows as u32);
        output
    }
    fn operator(
        &mut self,
        params: [u32; 12],
        weights: usize,
        input: usize,
        auxiliary: usize,
    ) -> usize {
        let output = self.empty("resident operator result", params[1] as usize);
        let shape = self.uniform(&params);
        self.dispatch(
            false,
            &[weights, input, auxiliary, output, shape],
            params[1].div_ceil(64),
        );
        output
    }
    fn gru(&mut self, name: &str, input: usize, width: usize, hidden: usize) -> Result<usize> {
        let state = self.empty("resident recurrent state", hidden);
        let projected = self.matvec(&format!("{name}_weights"), input, hidden * 3, width)?;
        let recurrent = self.matvec(&format!("{name}_rweights"), state, hidden * 3, hidden)?;
        let mut packed = self.model.get(&format!("{name}_bias"))?.values.clone();
        packed.resize(hidden * 7, 0.0);
        let parameters = self.data("resident GRU bias and previous state", &packed, true);
        self.weight_bytes -= hidden * 4;
        self.steps.push(Step::Copy {
            source: state,
            destination: parameters,
            source_offset: 0,
            destination_offset: (hidden * 6 * 4) as u64,
            bytes: (hidden * 4) as u64,
        });
        let output = self.operator(
            [3, hidden as u32, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            parameters,
            projected,
            recurrent,
        );
        self.states.push((state, output));
        Ok(output)
    }
}

pub struct ResidentGraph {
    buffers: Vec<wgpu::Buffer>,
    steps: Vec<Step>,
    states: Vec<(usize, usize)>,
    input: usize,
    output: usize,
    staging: wgpu::Buffer,
    pub weight_bytes: usize,
}
impl ResidentGraph {
    // Network::new validates every model shape before this graph is constructed.
    pub fn new(gpu: &Gpu, model: &Model) -> Result<Self> {
        let mut b = Builder {
            gpu,
            model,
            buffers: Vec::new(),
            steps: Vec::new(),
            states: Vec::new(),
            weight_bytes: 0,
        };
        let dummy = b.data("unused operator input", &[0.0], false);
        let input = b.empty("resident feature context", CONTEXT * FEATURES);
        let center = b.empty("resident center frame", FEATURES);
        b.steps.push(Step::Copy {
            source: input,
            destination: center,
            source_offset: (3 * FEATURES * 4) as u64,
            destination_offset: 0,
            bytes: (FEATURES * 4) as u64,
        });
        let (mut x, mut channels, mut height) = (input, 1, CONTEXT);
        for (i, output_channels) in [16, 24, 32].into_iter().enumerate() {
            let mut packed = model
                .get(&format!("conv2d_out{}_1_weights", i + 1))?
                .values
                .clone();
            packed.extend_from_slice(&model.get(&format!("conv2d_out{}_1_bias", i + 1))?.values);
            let weights = b.data("resident convolution weights and bias", &packed, true);
            let count = output_channels * (height - 2) * FEATURES;
            x = b.operator(
                [
                    0,
                    count as u32,
                    channels as u32,
                    height as u32,
                    FEATURES as u32,
                    output_channels as u32,
                    (height - 2) as u32,
                    3,
                    5,
                    2,
                    1,
                    0,
                ],
                weights,
                x,
                dummy,
            );
            let mut parameters = Vec::new();
            for suffix in ["gamma", "beta", "mean", "var"] {
                parameters.extend_from_slice(
                    &model
                        .get(&format!("batch_normalization_{}_{suffix}", i + 1))?
                        .values,
                );
            }
            let parameters = b.data("resident normalization parameters", &parameters, true);
            x = b.operator(
                [
                    1,
                    count as u32,
                    output_channels as u32,
                    (height - 2) as u32,
                    FEATURES as u32,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                ],
                parameters,
                x,
                dummy,
            );
            channels = output_channels;
            height -= 2;
        }
        let noise = b.gru("noise_gru_1", center, 320, 256)?;
        let combined = b.operator([4, 10496, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], dummy, noise, x);
        x = combined;
        for (name, width, hidden) in [
            ("noise_denoise_gru_1_1", 10496, 256),
            ("noise_denoise_gru_2_1", 256, 512),
            ("noise_denoise_gru_3_1", 512, 1024),
        ] {
            x = b.gru(name, x, width, hidden)?;
        }
        let projection = crate::network::projection_weights(model)?;
        let weights = b.data("output projection in execution order", &projection, true);
        let logits = b.matvec_buffer(weights, x, GAINS, 1024);
        let bias = b.weight("noise_denoise_filter_1_bias")?;
        let output = b.operator(
            [5, GAINS as u32, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            bias,
            logits,
            dummy,
        );
        let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("resident mask readback"),
            size: (GAINS * 4) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Ok(Self {
            buffers: b.buffers,
            steps: b.steps,
            states: b.states,
            input,
            output,
            staging,
            weight_bytes: b.weight_bytes,
        })
    }
    pub fn step(&self, gpu: &Gpu, context: &[f32]) -> Result<Vec<f32>> {
        gpu.queue
            .write_buffer(&self.buffers[self.input], 0, bytemuck::cast_slice(context));
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        for step in &self.steps {
            match step {
                Step::Copy {
                    source,
                    destination,
                    source_offset,
                    destination_offset,
                    bytes,
                } => encoder.copy_buffer_to_buffer(
                    &self.buffers[*source],
                    *source_offset,
                    &self.buffers[*destination],
                    *destination_offset,
                    *bytes,
                ),
                Step::Dispatch {
                    matrix,
                    binding,
                    groups,
                } => {
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("resident graph operation"),
                        timestamp_writes: None,
                    });
                    pass.set_pipeline(if *matrix {
                        &gpu.pipeline
                    } else {
                        &gpu.operators
                    });
                    pass.set_bind_group(0, binding, &[]);
                    pass.dispatch_workgroups(*groups, 1, 1);
                }
            }
        }
        encoder.copy_buffer_to_buffer(
            &self.buffers[self.output],
            0,
            &self.staging,
            0,
            (GAINS * 4) as u64,
        );
        gpu.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        self.staging
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        let result = gpu
            .device
            .poll(wgpu::PollType::Wait)
            .map_err(|e| Error::new(format!("Resident GPU poll failed: {e}")))
            .and_then(|_| {
                rx.recv_timeout(std::time::Duration::from_secs(30))
                    .map_err(|e| Error::new(format!("Resident mask readback timed out: {e}")))?
                    .map_err(|e| Error::new(format!("Resident mask readback failed: {e}")))
            });
        if let Err(error) = result {
            self.staging.unmap();
            return Err(error);
        }
        let view = self.staging.slice(..).get_mapped_range();
        let mask = bytemuck::cast_slice::<u8, f32>(&view).to_vec();
        drop(view);
        self.staging.unmap();
        if mask
            .iter()
            .any(|x| !x.is_finite() || !(0.0..=1.0).contains(x))
        {
            return Err(Error::new("Resident graph produced an invalid noise mask"));
        }
        // Commit only after successful readback and validation; failed steps retain old state.
        let mut commit = gpu.device.create_command_encoder(&Default::default());
        for (state, candidate) in &self.states {
            commit.copy_buffer_to_buffer(
                &self.buffers[*candidate],
                0,
                &self.buffers[*state],
                0,
                self.buffers[*state].size(),
            );
        }
        gpu.queue.submit([commit.finish()]);
        Ok(mask)
    }
    pub fn reset(&self, gpu: &Gpu) {
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        for (state, _) in &self.states {
            encoder.clear_buffer(&self.buffers[*state], 0, None);
        }
        gpu.queue.submit([encoder.finish()]);
    }
}
