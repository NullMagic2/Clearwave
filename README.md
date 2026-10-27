# Clearwave

## What is Clearwave?

Clearwave is an implementation which allows users to use NVIDIA RTX technology on AMD systems, allowing helping t o reduce background noise from your microphone, whether you’re recording, chatting with friends, or joining a video call. 
It processes audio locally using your AMD graphics card and provides a virtual microphone you can select in your other apps. 

## Why was it created?
NVIDIA’s noise reduction technology is impressive for recordings, but using it usually means having an NVIDIA setup. 
Clearwave was created to bring the same model to AMD-based Linux systems, without requiring NVIDIA drivers or running NVIDIA’s Windows application.


## Model files

For legal reasons, NVIDIA model files are not distributed with Clearwave.
They can be obtained by obtaining **NVIDIA RTX Voice** separately; see
[NVIDIA’s RTX Voice setup and download guide](https://www.nvidia.com/en-us/geforce/guides/nvidia-rtx-voice-setup-guide/).
Clearwave currently supports the model bundle from RTX Voice **0.5.12.6**.
The installer, model packages, NVIDIA DLLs, and exported weights are excluded
from this repository and its releases. NVIDIA’s files remain subject to NVIDIA’s
license; Clearwave’s MIT license does not apply to them.

## License

Clearwave’s original source code is licensed under the [MIT License](LICENSE).
Third-party Rust dependencies and flag artwork retain their own licenses and
notices. Flag artwork attribution is in [ui/flags/LICENSE](ui/flags/LICENSE);
binary packages also include Rust dependency notices.

## Desktop interface

```sh
./bin/clearwave
# Or select another bundle/settings file:
./bin/clearwave gui runtime/vendor --settings runtime/desktop.json
```

Choose a microphone, adjust **Noise removal**, and click **Start denoising**.
Select the resulting `<original microphone> – audio denoising` input in your
recording or calling application. **Use as system microphone** explicitly changes
the system default; apps with a fixed input still need their own selection.
The window opens with capture stopped. Closing it stops its owned audio stream.

The small flag in the header switches between European Portuguese (Portugal),
Brazilian Portuguese (Brazil), Greek, Spanish, and English (United Kingdom).
The interface updates immediately and remembers the selection in desktop settings.
The country flags come from [flag-icons](https://github.com/lipis/flag-icons),
licensed under MIT. Their original SVGs and attribution are in `ui/flags/`;
`./bin/clearwave licenses` also displays the embedded copyright notice.
The settings button opens model folder, AMD device, buffer, and preset controls. The model folder is detected from `runtime/vendor` beside the project.
Active settings are saved locally to `runtime/desktop.json`. Named presets can be
saved, loaded while stopped, and deleted. Microphone, model, device, and buffer
selection are locked while processing; strength changes apply without reconnecting
applications. The single signal bar reports denoised RMS in dBFS; hovering shows
its peak level.

The interface follows the installed GTK3 theme and uses a native folder chooser.
It requires GTK 3.24 or later and the GdkPixbuf SVG loader at runtime. It does not require a GUI OpenGL renderer.

## Debian, Ubuntu, and Linux Mint packages

[GitHub Releases](https://github.com/NullMagic2/Clearwave/releases) provides
model-free amd64 `.deb` packages for Debian 13, Ubuntu 24.04 LTS or newer, and
Linux Mint 22.x. The `releases/` folder contains the installation guide, manifest,
and checksums. Install
the matching file with `sudo apt install ./filename.deb`. Each package contains
the same executable linked against the Ubuntu 24.04 library baseline.

The installed app appears as **Clearwave** in the application menu. Settings
and optional local runtime assets use
`${XDG_DATA_HOME:-$HOME/.local/share}/clearwave/runtime/`. Choose a locally
supplied model folder in Settings. NVIDIA assets are excluded from every package.
Existing installed preferences under the previous application name are read when
Clearwave has no saved preferences yet, then saved under the new name.
The portable project build continues to use the project's own `runtime/` folder.
See `packaging/BUILDING.md` for reproducible package assembly and sysroot builds.

## Build

Requires Rust/Cargo 1.88+, a C linker, `pkg-config`, GTK3 development libraries,
and the dependencies in `Cargo.lock`. On Ubuntu, the GTK development package is
`libgtk-3-dev`.
The scripts use Cargo's offline mode so builds do not fetch or change dependencies.
On a new development machine, run `cargo fetch --locked` once first.

```sh
./build.sh
./bin/clearwave --help
./clean_build.sh
```

The clean build leaves only the executable in `bin/`; it preserves the source
project. Both scripts can be run from any working directory. `CARGO` can select a
different Cargo executable. Default output is a native Linux executable.

## Load the supplied model

7-Zip is needed only for the initial archive extraction. The destination must be
new; vendor assets stay external to the executable.

```sh
mkdir -p runtime
./bin/clearwave extract \
  /path/to/NVIDIA_RTX_Voice_v0.5.12.6_Updated.exe runtime/vendor
./bin/clearwave inspect runtime/vendor
./bin/clearwave export runtime/vendor runtime/voice-weights.safetensors
```

The original file names, package payload, tensor names, and F32 values are
preserved. Exported tensors retain flat stored order; reshaping requires the
network's layer specification. The output path must not already exist.

## AMD execution

Requires an accessible AMD Vulkan hardware adapter and Vulkan userspace driver.

```sh
./bin/clearwave gpu-info
./bin/clearwave gpu-check runtime/vendor
./bin/clearwave gpu-check runtime/vendor '7900'
```

The matrix check executes nine real model matrices and compares every result with
an F64 CPU calculation. It refuses software/CPU adapters and reports the hardware.

## Feature-level network

```sh
./bin/clearwave graph runtime/vendor
./bin/clearwave operator-check runtime/vendor '7900'
./bin/clearwave infer-features runtime/vendor input.f32 gains.f32 '7900'
./bin/clearwave infer-features-cpu runtime/vendor input.f32 reference.f32
```

Input is a sequence of feature frames, each containing 320 little-endian F32
values. Output contains 961 raw F32 noise-mask values per input frame. These
commands accept model features; use the WAV commands below for waveform samples.

The implementation forms a seven-frame context, pads sequence boundaries with
zero feature values, and preserves four GRU states across frames. A new command
starts a new sequence with zero recurrent state. Output files must be new. If an
I/O or compute error interrupts a run, the partial output remains for inspection.

`operator-check` compares convolution, batch normalization, three activations,
GRU updates, and the connected feature graph against the CPU implementation. It
also checks state persistence and reset. CPU/AMD agreement does not establish
parity with NVIDIA's complete audio output. See `docs/MODEL_CONTRACT.md`.

## Process a recording

```sh
./bin/clearwave denoise runtime/vendor input.wav cleaned.wav 1.0 '7900'
./bin/clearwave denoise-cpu runtime/vendor input.wav reference.wav 1.0
./bin/clearwave wav-features input.wav features.f32
./bin/clearwave audio-bypass input.wav reconstructed.wav
```

The native WAV reader supports 48 kHz PCM16, PCM24, PCM32, and IEEE float32,
with one or two channels. Stereo is averaged to mono. Output is mono float32 WAV
with exactly the input sample count, including partial final hops. Existing files
are refused. Input files are limited to 64 MiB. Other sample rates and compressed
formats need conversion first, for example:

```sh
ffmpeg -i recording.flac -ar 48000 -ac 1 -c:a pcm_s16le input.wav
```

Strength defaults to 1.0; 0.0 gives unity spectral gain and 0.5 applies half the
predicted removal. The feature commands return the raw network output; the audio
path applies `gain = 1 - strength * noise_mask`. This control does not duplicate
the vendor application's adaptive percentile control.

`wav-features` uses a periodic Hamming window, a 1,920-sample FFT, 480-sample hop,
and 320 area-normalized Slaney mel bands. Mel power is clipped between −40 and
48 dB and mapped to 0–1. Frame zero begins at sample −1,440; three tail frames
complete the overlap. Features outside the sequence are zero. Output is aligned
to input sample zero after inverse FFT and squared-window normalization.
See `docs/MODEL_CONTRACT.md` for the supported conventions and remaining parity
questions.

The CLI reports the actual adapter, execution mode, resident parameter bytes,
processing time, sample count, RMS levels,
and samples above full scale. It preserves float output without clipping or
automatic volume normalization. Lower output RMS alone does not prove speech
preservation or effective noise removal.

The AMD network keeps all 62,453,348 bytes of model parameters, recurrent states,
intermediate results, and bind groups resident on the GPU. Each feature step
uploads 8,960 bytes of context and reads back 3,844 bytes of mask data. GPU state
commits after successful output validation. The explicit CPU commands retain the
independent CPU path.

## Use the processed microphone across applications

Requires PipeWire/WirePlumber tools (`pw-dump`, `pw-loopback`, `pw-record`,
`pw-play`, `wpctl`), GNU `env`, and util-linux `setpriv`, alongside AMD Vulkan.

```sh
./bin/clearwave sources
./bin/clearwave live runtime/vendor \
  alsa_input.pci-0000_11_00.6.analog-stereo \
  --strength 1.0 --latency-ms 10 --adapter '7900'
```

Choose the exact source name, current object ID, or serial listed by `sources`.
