# Architecture

This is a native Linux and Windows Rust 2024 model-loading and AMD-compute application.
The current binary loads NVIDIA RTX Voice 0.5.12.6 and pinned Broadcast 2.3 audio packages,
exports named tensors, and processes WAV recordings through a connected network
on AMD Vulkan hardware. Its waveform adapter runs on the CPU. Vendor audio-output
parity, speech quality, and device latency remain unverified. A live PipeWire
controller connects the streaming adapter to a temporary virtual microphone.
Windows uses native windows-rs controls and an in-process capture APO. A bounded
local pipe carries microphone samples to a separate GPU worker and returns them
to the original capture endpoint. There is no speaker-output or virtual-cable
route. This is a development implementation: native audio-host loading, OEM
compatibility, production signing and installation remain unverified. See
[Windows APO](docs/WINDOWS_APO.md).

## Components

| Component | Responsibility | State |
| --- | --- | --- |
| `main.rs` | CLI dispatch, bundle extraction, JSON reporting | Implemented |
| `model_source.rs` | Shared model discovery and validated DLL-free Safetensors loading | Implemented |
| `model_import.rs` | Cancellable dual-installer import, atomic staging and validated cache | Implemented |
| `broadcast_import.rs` | Local NGX cache discovery and bounded downloads from NVIDIA | Implemented for the pinned Broadcast 2.3 revision |
| `broadcast.rs` | Data-only TensorRT weight mapping into the shared graph | Implemented for the pinned Broadcast 2.3 revision |
| `pe.rs` | Bounded PE section/RVA access | Implemented |
| `package.rs` | Package decoding, tensor parsing, hashes, inventory | Implemented for supplied package variant |
| `export.rs` | Safetensors export retaining flat stored order | Implemented |
| `gpu.rs` | AMD Vulkan selection, buffers, dispatch, readback | Implemented |
| `shaders/matvec.wgsl` | Parallel matrix-vector multiplication | Implemented |
| `shaders/operators.wgsl` | Convolution, normalization, activations, GRU update | Implemented |
| `operators.rs` | CPU reference operators and shape contracts | Implemented |
| `network.rs` | Shape checks, CPU reference graph, backend dispatch and recurrent state | Implemented; vendor audio-output parity pending |
| `resident.rs` | Persistent Vulkan weights, graph buffers, bindings, dispatch and state commits | Implemented |
| `wav.rs` | Bounded RIFF parsing, PCM decoding, stereo downmix, float WAV output | Implemented for 48 kHz mono/stereo |
| `fft.rs` | Mixed-radix complex transform and normalized inverse | Implemented for factors 2/3/5 |
| `audio.rs` | Model-specific windows, mask alignment/strength, overlap reconstruction, bounded streaming | Implemented; boundary conventions documented |
| `conditioning.rs` | Broadcast block peaks, activity gating, input normalization and matched output compensation | Implemented |
| `voice_balance.rs` | Shared final microphone EQ and soft peak protection, outside the neural model | Implemented; automatic in desktop output |
| `pipewire.rs` | Source enumeration, temporary virtual microphone, stream lifecycle, latency requests | Implemented |
| `desktop.rs` | Native GTK3 window/settings dialog, device selection, child lifecycle, a measured denoised-signal bar | Implemented |
| `i18n.rs`, `ui/flags/` | Five UI languages and embedded country flags | Implemented |
| `windows/desktop.rs` | Native Windows controls, DPI/fonts/flag resources, bounded worker events | Implemented; physical Windows validation pending |
| `windows/audio.rs` | Capture endpoint discovery, local APO pipe server, shared AMD worker | Implemented; physical Windows validation pending |
| `apo/` | COM capture effect, bounded realtime bridge, local pipe protocol | Development preview; native host validation pending |
| `windows/mod.rs` | Thread-bound COM, folder picker helper, owned Windows jobs, atomic preference replacement | Implemented |
| `storage.rs` | Bounded file reads with limits on bytes actually read | Implemented |
| `controls.rs` | Validated local preferences, atomic live strength updates, RMS/peak aggregation | Implemented |

The final projection retains input-major order in model files. CPU setup and
resident GPU upload transpose it once to the execution matrix layout. This
conversion is local to inference; export preserves original stored values.

## Data path

Microphone output applies the shared `balanced-v1` voicing profile after waveform
reconstruction and input-gain compensation. A +2.5 dB peaking EQ at 180 Hz adds
voice body, with near-unity DC/treble response and a soft guard above 0.98 full
scale. Linux, Windows and normal offline denoising share this implementation.
A saved Bass slider controls the 180 Hz EQ from -6 to +6 dB, defaulting to +2.5 dB.
Live changes interpolate coefficients over 20 ms in the audio worker. Older
preferences/control files retain the default gain. Low-frequency echo prediction
is reduced to protect sustained voice harmonics. Reference commands omit the
final EQ/guard; neural features, weights and masks are unchanged.
See [voice balance](docs/VOICE_BALANCE.md) for scope and validation.

The preferred path loads a Safetensors file directly into named F32 tensors and
the compiled graph. The diagram below shows the optional original-package import
and export path as well.

```text
Local installer --7-Zip--> local bundle
                              |
Model package + runtime data --+--> validated payload --> named F32 tensors
                                                        |             |
                                                   Safetensors    Vulkan buffers
                                                                      |
                                                       feature-level network
                                                               |
                                                       961 noise-mask values
                                                               |
48 kHz WAV --> Model window / FFT --> mel power / dB features ---------+
                    |                                          |
                    +----> apply (1 - strength * noise mask) <--+
                                      |
                         inverse FFT / weighted overlap --> mono WAV
```

The installer is read as an archive. Its executable entry point and virtual-audio
driver are never invoked. The compatibility importer reads version-specific data from the supported DLL
when decoding the original encrypted package; it never executes that DLL.
The preferred Safetensors path bypasses the DLL completely and validates the
32 named F32 tensors against the compiled graph. The Rust/WGSL engine performs
all inference in both cases.

The legacy payload is `weights_v0`: a 10-byte signature followed by repeated little-endian
name length, UTF-8 name, element count, and IEEE-754 F32 data. Names and dimensions
are validated before GPU submission. The exported tensor shapes are one-dimensional
to preserve their stored order. The full execution shapes and connections are
recorded separately in `docs/model-graph.json`, with mapping evidence in
`docs/MODEL_GRAPH.md`. The feature graph milestone is complete; waveform feature
alignment and vendor audio-output parity remain open.

Broadcast imports map its blocked convolution matrices, folded normalization,
GRU gate ordering and biases, and output projection into the same 32-tensor graph.
Safetensors metadata records the source model profile. Existing exports without
this metadata retain their RTX Voice interpretation. Broadcast archive requests
use bounded HTTPS ranges and verify entry CRCs plus pinned model/decoder hashes.
The large SDK libraries are not downloaded. These operations run in the existing
cancellable helper, with parent-death handling on Linux and owned Windows jobs.
See [Broadcast model support](docs/BROADCAST_MODEL.md) for formats and current
output-comparison limitations.

## GPU execution contract

The current backend is `wgpu` over Vulkan. It selects an AMD hardware adapter,
preferring discrete GPUs. A substring argument can choose another AMD adapter.
Software Vulkan implementations are excluded from `gpu-check`. This backend uses
no CUDA, cuDNN, ROCm, or Wine runtime.

Each matrix check uses actual model values with a deterministic input vector. A
256-lane WGSL workgroup computes each row and reduces its partial sums. Results
are copied to the host and compared with an F64 CPU dot product. The report states
that these are isolated operations, not complete denoiser outputs. The measured
time includes buffer setup, dispatch, synchronization, and readback; it is not an
audio latency or steady-state inference benchmark.

## Boundaries

- Original-package import is limited to the supplied single-segment package
  variant and compatible 0.5.12.6 decoder-data layout, plus the pinned Broadcast
  2.3 package/decoder pair documented above. DLL-free loading accepts
  the matching graph exported as Safetensors. Other graphs fail explicitly.
- The raw tensor table contains 32 tensors and 15,613,337 F32 parameters.
- The feature graph uses seven 320-value frames, three convolution/normalization
  stages, four stateful GRUs, and a 961-output sigmoid projection. It exposes
  explicit CPU and AMD paths. See `docs/MODEL_CONTRACT.md` for layout details.
- The audio adapter selects periodic Hamming for the legacy model and periodic
  Hann for Broadcast. Both use a sparse Slaney filterbank, clipped log power,
  and normalized overlap addition. The shared seven-frame context has bounded
  memory. Legacy predictions target the centered spectrum and discard three
  virtual predictions while retaining state. Broadcast feeds the newest feature
  into the noise GRU and applies each mask to the newest spectrum, with causal
  history and no lookahead-mask discard.
- Broadcast uses four-hop input blocks for peak/activity decisions. Its bounded
  conditioner scales quiet input internally, stores the gain per input hop, and
  compensates the corresponding reconstructed output hop. Output activity is
  measured before compensation and affects the next block. Its intensity curve
  maps the slider in F64 before a uniform F32 spectral blend. Legacy intensity
  ranking remains separate. Both desktop platforms use this shared processor.
- The waveform file path pads 1,440 samples before the first input sample and
  flushes three trailing frames. It crops the reconstruction to the original
  sample count. Broadcast instead uses the causal alignment and block conditioner
  above. Fourteen fixed-criteria comparisons against the NVIDIA runtime on AMD pass;
  native NVIDIA hardware and general output equivalence remain unverified.
- The Vulkan graph uploads all model parameters at construction and allocates
  intermediates and bind groups once. Per-step transfers are an 8,960-byte feature
  context and 3,844-byte output mask. Concatenation, channel reordering, dense bias,
  and sigmoid stay on the GPU. Isolated operator checks retain their separate
  upload/readback path for diagnostics.
- Four candidate recurrent states are copied to committed state only after the
  mask readback succeeds and all outputs are finite and in range. Rejected input
  does not advance state. Reset queues GPU buffer clears in submission order.
- Raw audio streaming uses a 1,920-sample history, a seven-frame context, a
  1,920-sample overlap buffer, and a matching normalization buffer. It emits hops
  after six hops of legacy algorithmic delay. Broadcast uses three spectral-delay
  hops and batches four input hops, adding up to three hops of input wait.
  EOF pads a partial block, flushes three audio hops plus the model's lookahead,
  and crops to the original sample count. Compute errors
  end the stream; it must be restarted. The stream creates no audio routes.
- Vendor model/runtime assets remain external. The built executable embeds only
  this project's code and WGSL shaders; GTK3 remains a system library.
- Existing exports are never overwritten. Inspection commands preserve audio
  settings. `live` captures an explicitly selected source, creates temporary
  loopback nodes, and feeds them through the native stream child process.
  `make-default-mic` explicitly selects the virtual source through WirePlumber.
- The controller owns capture, processor, playback, and loopback children.
  Scoped signalfd handling catches SIGINT/SIGTERM without native unsafe code.
  Child signal masks are restored with GNU env; util-linux setpriv installs a
  parent-death signal. Normal exits, source loss, child failure, and controller
  death remove the temporary nodes. The source is explicitly targeted and cannot
  be this application's own virtual input. No service or driver is installed.
- Virtual node names are stable internal routing identifiers. The user-visible
  description is the selected physical source description followed by the en-dash
  suffix ` – Clearwave`.

## Linux desktop lifecycle

The default command opens the GTK3 window with capture stopped. GTK widgets stay
on the main thread. Worker threads enumerate devices and read the live child's
JSON events through a bounded channel; a 100 ms main-loop timer updates the UI.
The desktop owns a separate `live` child and terminates it on Stop or window close.
Child lifetime safeguards also apply if the desktop exits unexpectedly.

The main window contains source selection, a strength slider, one denoised signal
bar, the virtual source description, and explicit start/default-input buttons. A non-modal
settings dialog provides an isolated GTK folder chooser, AMD device selection,
and buffering options. Configuration that requires reconnecting is disabled
while running. Strength is published atomically to a validated control file and
read every ten audio hops; changing it preserves the stream and recurrent state.

The header's flag menu applies one of five built-in UI translations immediately.
Language is a global desktop preference; older JSON
files default to British English. Country flags are original `flag-icons` SVG assets, pinned to the revision in
`ui/flags/SOURCE.json`, embedded and rendered at 24×18 pixels by GdkPixbuf. The MIT
notice is stored beside the assets and embedded in the `licenses` CLI command. Device descriptions and the established virtual-source suffix keep
their original text. Technical backend diagnostics and platform-owned chooser
content retain their own wording.

Preferences have bounded, versioned JSON storage. Active settings are saved after
a short debounce. Invalid existing preferences are preserved until an explicit
save. Schema version 2 preserves active settings and language from version 1 while
discarding its removed named-preset list.
Meter events contain RMS and peak values computed from real input/output samples;
stale readings clear after 600 ms. No signal is inferred from node existence.

## Build lifecycle

`build.sh` builds with a locked dependency set into `.build/`, then publishes the
executable in `bin/`. `clean_build.sh` builds from scratch in a temporary directory,
publishes the executable, and removes the temporary directory and this project's
`.build/` and `target/` directories. It preserves source, documentation, external
runtime assets, and the existing binary if compilation fails.

`roadmap.html` is the implementation ledger. Its weighted percentages count
completed functionality only. Validation has its own status and contributes zero
to implementation progress.

## Distribution packages

The model-free amd64 `.deb` packages share one ELF executable linked against an
Ubuntu 24.04 sysroot. The build uses isolated pkg-config paths and a GCC sysroot
wrapper; packaging rejects glibc symbol requirements above 2.39 and any RPATH.
Debian 13 and Ubuntu 24.04 runtime libraries are checked separately. Mint 22.x
uses the Ubuntu 24.04 compatibility baseline.

`packaging/package.py` copies an explicit allowlist into the package: executable,
desktop entry, icon, documentation, lockfile, and component notices. It never
copies the project's runtime directory or model weights. The packages install no
services or audio configuration. `/usr/bin` and `/usr/local/bin` executions use
per-user XDG data storage; portable builds retain project-relative runtime paths.

## Folder picker isolation

GTK widgets remain on the main thread of their own process. Model-folder
browsing uses a dedicated GTK helper process, supervised by the
`model-folder-picker` worker thread. The application polls a bounded result
channel without waiting on the helper. Local filesystem browsing stays in the
helper, so a blocked dialog cannot block the main window. The Settings Browse
button becomes Cancel while a picker is active. Cancel, hiding Settings, or
closing Clearwave signals the worker to kill and reap the owned helper; parent
death also kills the helper. Settings is non-modal so the main window remains closable. Returned paths use
a bounded JSON protocol.

## Model locations

`model_source.rs` is shared by GUI processing and all model CLI commands.
It prefers `voice-weights.safetensors` or `model.safetensors` in the selected
folder; CLI callers may pass a Safetensors file directly. The compatibility
path accepts `denoiser_48k.wpkg` plus `NVAudioEffects.dll` together in that
folder, or their original `NvAFX/models` and `NvAFX/nv` locations.
An invalid preferred export fails without falling back to a proprietary file.

The official Rust Safetensors reader validates the container. Clearwave adds
128 MiB file and 1 MiB header limits, F32-only finite tensors, name limits,
exact tensor count, and graph-shape checks. Safetensors `runtime_sha256` is
null and `runtime_required` is false in inspection output. Its package hash
identifies the exported container, and its payload hash identifies the raw
tensor-data buffer. The original importer retains its original hash meanings.
Model discovery and reads happen in the live child process, keeping the GTK
thread free of model I/O. The graph JSON remains documentation, not a required
model asset.

## Windows frontend and audio boundary

GTK, nix, and PipeWire dependencies are Linux-only. On Windows, the `windows`
module owns all native API interoperability; the shared DSP and model engine
remain safe Rust. The common-controls manifest enables system theming and
per-monitor DPI. Widget/GDI access and the message pump stay on one thread.
Device discovery communicates through bounded channels. Model loading and Vulkan
processing run in a separate owned audio process.

The Windows capture APO and worker communicate through bounded local pipe
packets. The existing microphone keeps its name; no playback endpoint or virtual
cable is used. The APO callback never waits for inference and uses timestamped dry
fallback when the worker cannot supply an on-time result. Its fixed 120 ms delay
is reported to the audio host. See [Windows APO](docs/WINDOWS_APO.md) for supported
formats, setup/removal, signing limits and native-host validation gaps.

The setup worker monitors the elevated installer without blocking the UI. It
checks a request-matched result, process exit code and endpoint registration.
Start offers setup when registration is missing, then resumes after success.
Closing the UI cancels monitoring without interrupting registry rollback.

Discovery, picker and inference children belong to kill-on-close Windows jobs.
The UI never joins audio or GPU threads. Active settings are saved atomically to
`%LOCALAPPDATA%\Clearwave\runtime\desktop.json`; the legacy render preference
is ignored and cleared when capture devices are refreshed.

Cross-compilation and compatibility-layer checks do not establish Windows device
behavior, real-time deadlines, or waveform parity.

## Local installer import

`model_import.rs` shares allowlisted 7-Zip extraction between the CLI, model
loader, and native chooser helpers. Helpers own the import off the UI thread;
Windows jobs and Linux parent-death signals bound child archiver lifetimes.
An installer content hash selects a local cache. Imports validate the supported
graph and export weights before publishing their directory; failed imports keep
the prior selected model. Cached models are reused, and a changed installer is
rejected before cache publication. Vendor executables are never launched.

Windows discovery is isolated by `windows/task.rs`: separate audio and graphics
commands return bounded JSON with cancellation and a deadline. Job termination,
process waits, and control-file removal run only on reaper threads. See
`docs/WINDOWS_LIFECYCLE.md` for the maintenance preview's scope.

## Optional room reverberation reduction

`dereverb.rs` shares the existing spectral pipeline between offline, PipeWire,
and WASAPI processing. It implements a bounded frame-online WPE predictor,
controlled independently by the persisted `echo_reduction` fraction. Both native
main windows expose a 0–100% slider directly below noise removal, defaulting to off. Model features
and recurrent state receive the original input. Processing stays in audio workers;
UI changes use the shared atomic `ProcessingControl` file. See
[echo reduction](docs/ECHO_REDUCTION.md) for parameters, safeguards and limitations.
