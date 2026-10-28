# Broadcast model support

Clearwave accepts both NVIDIA RTX Voice 0.5.12.6 and NVIDIA Broadcast
2.3.0.71988672 setup executables through **Settings → Installer…**. The same
importer is used by Linux, Windows, and the command line. These exact revisions
are supported; other Broadcast revisions fail explicitly.

## Import behavior

RTX Voice embeds its model bundle in the installer. Broadcast obtains its audio
model separately, so extracting the Broadcast EXE alone cannot provide it.
Clearwave first searches beside the installer, its `Models` subfolder, and the
local NVIDIA NGX cache. `CLEARWAVE_BROADCAST_MODELS` can explicitly select an
existing cache or folder. The two required files are:

```text
Models/
├── denoiser_48k.trtpkg
└── nvafxdenoiser.dll
```

If these files are unavailable, the Broadcast importer fetches approximately
33 MB from NVIDIA's HTTPS model service. Bounded archive-range requests retrieve
only those entries. They do not download the CUDA, cuBLAS, TensorRT, or FFT
runtime libraries. The supported audio artifacts are pinned to:

| Component | Model-cache revision |
| --- | --- |
| Denoising package | `nvbcast_afx_bnr_v0_9 / 2309 / 160_E658703` |
| Decoder data | `nvbcast / 2311 / 160_E658703` |

7-Zip reads the executable as an archive. `curl` performs any required downloads;
Windows uses its system `curl.exe`. Clearwave validates archive bounds, entry
checksums, the model/decoder SHA-256 identities, and graph dimensions before
committing the import. Neither the installer nor its DLLs are executed.

Imports run in the existing cancellable chooser helper, outside the UI thread.
The previous selection is retained until preparation succeeds. The final cache
is keyed by installer SHA-256 and contains a reusable `voice-weights.safetensors`
export. Subsequent selection of the same installer reuses that validated cache.
NVIDIA files and exported weights remain local and are excluded from releases.

## Shared graph

The Broadcast decoder reads the supported package as data, converts its stored
half-precision weights into F32 tensors, and maps them into Clearwave's existing
graph. It handles blocked convolution storage, folded normalization, GRU gate
ordering, separate candidate biases, duplicated gate biases, and the final
projection's orientation. Both model families have 32 named tensors and
15,613,337 parameters. Export metadata records the model family; existing exports
without this field retain their RTX Voice interpretation.

This mapping makes Broadcast's learned parameters available to the native
Rust/Vulkan engine. It does not establish identical floating-point execution,
feature conditioning, or waveform results.

## Broadcast waveform behavior

Broadcast uses a **periodic Hann** window; the legacy adapter uses Hamming.
Its seven-frame convolution context contains six preceding frames and the newest
frame. The noise GRU receives the **newest frame**, and the output mask applies to
that frame's spectrum. Treating this as centered lookahead caused excess speech
attenuation in earlier builds.

Broadcast input conditioning operates on four-hop blocks. It keeps bounded peak
candidates, activates internal normalization for quiet input after measured output
activity, and preserves each input hop's compensation factor until that hop is
reconstructed. Activity is measured before compensation and affects the next
block. Internal input scaling is undone on output; this adds no voice-enhancement
or automatic loudness control to the recording.

The slider uses Broadcast's nonlinear curve. Below 80%, the spectral strength is
`1 + log10(slider + 0.1)`, where the slider is in 0–1. Above that point it interpolates
from `1 + log10(0.9)` to 1. Every bin uses `1 - mapped_strength * noise_mask`.
The legacy strongest-noise-bin ranking is not applied to Broadcast. The native
stream has three spectral-delay hops and four-hop input cadence; device/transport
buffering adds latency separately. EOF pads a partial block and reconstructs the
finite tail without losing samples.

## Validation snapshot — 28 October 2026

Both original installers imported successfully on Linux. A fresh Broadcast
model download and the existing-cache path produced byte-identical exports.
All 32 tensors also matched an independent layout mapping byte for byte. Both
Linux and Windows use the corrected shared waveform and graph code. The Linux
suite passes all 54 tests. The Windows executable passes 54 tests under Wine,
with the optional screenshot-preview test skipped. Linux and Windows Clippy and
both release builds pass; the resident AMD operator/graph check passes 30/30.

The comparisons used two public speech references, clean and with controlled
clicks/hum/static, ordinary and −20 dB input levels, and slider values of 0%, 25%,
30%, 50%, 75%, 80%, 90%, and 100%. Echo reduction was off. The reference was the
NVIDIA Broadcast 2.3 NvAFX runtime on an RX 7900 XTX through Wine, ZLUDA, hipwine,
and translated compiled kernels. It is **not a native NVIDIA hardware reference**.
The fixed 3,360-sample reference delay was removed, with no gain normalization.

Acceptance limits were set before the expanded comparisons: output correlation
at least 0.9999, reference-to-candidate waveform SNR at least 40 dB, and voice-band
projection-gain differences no greater than 0.05 dB. **All 14 cases pass.**
Aggregate measurements and input hashes are in
[broadcast-comparison.json](broadcast-comparison.json). Across those cases:

| Measurement | Worst observed result |
| --- | ---: |
| Output correlation | 0.999995398 |
| Reference-to-Clearwave waveform SNR | 50.261 dB |
| Voice-band projection-gain difference | 0.007655 dB |

For the original 16.84-second noisy recording:

| Measurement | NVIDIA runtime on AMD | Before correction | Corrected Clearwave |
| --- | ---: | ---: | ---: |
| SNR against clean speech | 15.327 dB | 14.635 dB | 15.352 dB |
| Overall voice projection gain | −0.260 dB | −0.444 dB | −0.264 dB |
| Voice projection gain, 4–8 kHz | −2.825 dB | −4.383 dB | −2.832 dB |

Captured-feature comparison RMS error fell from 0.010119 to approximately
0.00000064 after correcting the window. With identical captured features, mask
RMS error fell from 0.034821 to approximately 0.000324 after correcting GRU frame
selection. These changes address the observed implementation differences.
Residual floating-point and approximation differences remain; the implementation
is not bitwise equivalent. Native NVIDIA hardware, other audio environments,
and live microphone/device behavior remain separate validation. Passing checks
do not increase implementation percentages.

## Repeating a comparison

From 0.7.1-dev.5, generate the neutral candidate with `denoise-reference` (and
echo reduction off). Regular `denoise` and desktop output add the automatic
[voice balance](VOICE_BALANCE.md) stage, which intentionally changes the result.
The measurements above describe the neutral model path in 0.7.1-dev.4.

The reusable comparison command reads existing local audio only:

```sh
python3 tools/compare_reference.py \
  --reference reference.wav --candidate clearwave.wav \
  --reference-delay 3360 --clean clean.wav --input noisy.wav \
  --reference-context "Describe the runtime and hardware used" \
  --min-output-snr-db 40 --min-correlation 0.9999 \
  --max-band-gain-difference-db 0.05 --json comparison.json
```

It requires Python, NumPy, and FFmpeg. It reuses the speech benchmark's
measurements and reports file hashes, overlap, delays, correlation, SNR, and
voice-band levels. Supplying acceptance limits adds a pass/fail result and returns
an unsuccessful exit status when they are missed. Reference/model files remain
external to the project.
