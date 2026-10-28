# Feature-level model contract

The application implements this graph over the original named weights and
provides an offline waveform adapter. Equivalence to the vendor's audio output
remains pending. CPU/AMD numerical agreement validates numerical consistency;
speech preservation needs a separate quality reference.

The supported feature graph is fully mapped and implemented. Its layer calls,
stored tensor layouts, recurrent conventions, and connection evidence are
documented in [MODEL_GRAPH.md](MODEL_GRAPH.md). The generated
`model-graph.json` includes explicit source connections and all 32 stored tensor
shapes. Completion of this feature graph is separate from waveform equivalence.

## Dimensions and execution order

Input context: seven consecutive feature frames of 320 F32 values, oldest first.
Convolution tensors use `[channels, time, frequency]` memory order. Filter tensors
use `[output_channels, input_channels, kernel_time, kernel_frequency]` order.

| Stage | Input | Output | Operation |
| --- | --- | --- | --- |
| Convolution 1 | 1 × 7 × 320 | 16 × 5 × 320 | 3 × 5, stride 1, time padding 0, frequency padding 2, bias, ReLU |
| Normalization 1 | 16 × 5 × 320 | Same | Per-channel gamma, beta, mean, variance; epsilon 0.001 |
| Convolution 2 | 16 × 5 × 320 | 24 × 3 × 320 | Same kernel/padding/activation rules |
| Normalization 2 | 24 × 3 × 320 | Same | Per-channel, epsilon 0.001 |
| Convolution 3 | 24 × 3 × 320 | 32 × 1 × 320 | Same kernel/padding/activation rules |
| Normalization 3 | 32 × 1 × 320 | Same | Per-channel, epsilon 0.001 |
| Noise GRU | Center input frame, 320 | 256 | Stateful |
| Concatenation | 256 + 10,240 | 10,496 | Noise output first, then frequency-major convolution output with channels innermost |
| Denoise GRU 1 | 10,496 | 256 | Stateful |
| Denoise GRU 2 | 256 | 512 | Stateful |
| Denoise GRU 3 | 512 | 1,024 | Stateful |
| Output projection | 1,024 | 961 | Stored `[1024, 961]` input-major matrix, dense, bias, sigmoid |

The output projection is stored as `[input neuron, frequency bin]`, unlike the
row-major output-by-input matrices consumed by execution kernels. It is
transposed once at network setup, without changing exported tensor order. Both
CPU and resident Vulkan execution use this conversion. Reading the stored buffer
directly as `[961, 1024]` scrambles the projection and produces incorrect masks.

All 32 tensor lengths are checked before network creation. The center frame is
context index 3. The feature file interface zero-pads the first and last three
contexts and returns one raw noise-mask frame for each input frame.

## GRU convention

Input weights are `[3H, I]`; recurrent weights are `[3H, H]`. Gate blocks follow
reset, update, new. Bias storage contains six blocks: input reset/update/new,
then recurrent reset/update/new. The update uses the reset-after convention:

```text
r = sigmoid(Wr x + br + Rr h + bRr)
z = sigmoid(Wz x + bz + Rz h + bRz)
n = tanh(Wn x + bn + r * (Rn h + bRn))
h_next = z * h + (1 - z) * n
```

The reset is applied to the recurrent candidate projection and its bias. Each
layer preserves its own hidden vector. State commits only after the entire
feature step succeeds. `reset()` clears all four vectors and the step counter.

The gate convention follows the [cuDNN GRU linear-layer IDs](https://docs.nvidia.com/deeplearning/cudnn/backend/v9.18.1/api/cudnn-adv-library.html)
and the reset-after formulation described by [PyTorch's GRU documentation](https://docs.pytorch.org/docs/main/generated/torch.nn.GRU.html).

## Offline audio contract

The 48 kHz processing configuration uses a 1,920-sample transform, a 480-sample
hop, 961 nonnegative-frequency bins, and 320 model features per frame. The
native adapter implements these stages:

1. Decode 48 kHz mono/stereo WAV. Average stereo channels before processing.
2. Apply periodic Hamming `w[n] = 0.54 - 0.46*cos(2π*n/1920)`.
3. Compute an unnormalized forward FFT and `power[k] = re² + im²`.
4. Project power onto 320 Slaney mel triangles spanning 0–24,000 Hz, each scaled
   by `2/(right_edge-left_edge)`. The frequency-to-mel map is linear below
   1,000 Hz (`f/(200/3)`); above that it is `15 + ln(f/1000)/(ln(6.4)/27)`.
5. Scale each feature with
   `clamp((10*log10(max(power,2^-24)) + 40)/88, 0, 1)`.
6. Run the stateful feature graph and apply the inverted raw prediction:
   At full strength, `gain[k] = 1 - noise_mask[k]`. At lower strength, rank
   `noise_power[k] = power[k]*noise_mask[k]`, retain full mask subtraction in
   the strongest `floor(961*0.2*strength)` bins, and use
   `gain[k] = 1 - strength*noise_mask[k]` in the remaining bins. When this count
   is zero, scale every bin by strength. Mirror the computed gains across
   negative-frequency bins to retain conjugate symmetry. No square root is
   applied to the mask.
7. Normalize the inverse FFT by 1,920, multiply by the synthesis Hamming window,
   overlap-add, and divide by the sum of overlapping squared windows.

The supplied runtime's DSP constants and embedded magnitude, power-to-dB, and
inverted-mask kernel instructions support these feature and gain conventions.
This is static evidence, not a waveform comparison against the running vendor
implementation. Version 0.6.6 adds the conditional percentile-dependent mask
scaling; earlier versions used direct linear blending at every frequency.

Frame `t` starts at sample `480*t - 1440`. The offline sequence has
`ceil(input_samples/480) + 3` spectral frames. Three future feature frames supply
the network context; missing feature frames are zeros. The waveform adapter runs
inference from the first arriving feature frame. Its first three predictions
target virtual spectra before frame zero; their masks are discarded while their
recurrent-state updates are retained. The first reconstructed frame therefore
uses the fourth prediction. See [AUDIO_ALIGNMENT.md](AUDIO_ALIGNMENT.md). First and final partial
waveform frames are zero-padded. The cropped output has exactly the input sample
count. These explicit file boundaries give unity reconstruction at both ends;
complete startup/flush agreement and audio-output parity remain open. The
reference input-level conditioner is still absent; directly boosting input level
was not a reliable quality improvement in the exploratory comparison.

## GPU and stream execution

The Vulkan graph retains weights and intermediate tensors between frames. Only
the seven-frame input context and final 961-value mask cross the host boundary.
GPU concatenation preserves the same frequency-major order as the CPU graph.
Candidate states commit after valid mask readback; reset clears the four committed
GPU states before the next queued step.

The raw float32 streaming adapter uses the same framing, features, context,
masking, and reconstruction rules as the offline path. It emits the first output
hop after receiving seven input hops, giving a six-hop / 60 ms algorithmic delay
relative to the corresponding input hop. Device and scheduling delays are extra.
EOF generates the same three trailing spectral frames and zero feature context,
and the output is cropped to the exact input length. Streaming buffers are bounded
independently of stream duration.

Waveform timings include CPU spectral processing, graph submission, and one GPU
mask readback per frame; they exclude initial model loading and GPU setup. A file
throughput measurement does not establish live audio routing latency or deadlines.
