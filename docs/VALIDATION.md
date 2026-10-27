# Validation snapshot

27 October 2026. These results contribute zero to implementation progress.

| Check | Result |
| --- | --- |
| Native Rust build | Passed |
| Clean release build | Passed; only one ELF executable in `bin/`; intermediate directories removed |
| Rust unit checks | 25 passed, including legacy language-preference migration, saved presets, validated live controls, measured levels, DSP, bounded streaming, source-selection safeguards, option validation, and existing network checks |
| Rust formatting | Passed |
| Clippy | Not run: the installed toolchain lacks the Clippy component |
| Supplied model loading | 32 tensors, 15,613,337 parameters |
| Minimal archive extraction | Matches the full bundle's inventory and hashes |
| Safetensors export | All 32 tensors match the decoded payload byte-for-byte |
| AMD Vulkan matrix operations | 9 passed on RX 7900 XTX; maximum scaled error below 6.6e-7 |
| AMD network operators and resident feature graph | 28 passed; maximum scaled error below 8.6e-7; state reset and rejected-input persistence verified |
| Feature file interface | 4 frames / 3,844 gains agree between CPU and AMD; maximum absolute error below 9.9e-7 |
| Independent PyTorch CPU graph | 4 frames agree with native CPU; maximum absolute error below 3e-6 |
| Independent NumPy audio features | 14 frames / 320 bands; maximum absolute error 7.64e-6 |
| Unity audio reconstruction | 4,807 samples including a partial hop; maximum absolute error 2.99e-8 |
| Independent NumPy mask reconstruction | Real model predictions reconstructed with NumPy FFT/IFFT; maximum absolute error 1.87e-8 |
| Complete CPU/AMD WAV path | 4,807 output samples agree within 1.87e-8; RX 7900 XTX |
| WAV interoperability | FFprobe identifies mono 48 kHz float32 WAV; native float output can be read back; existing output refused unchanged |
| Existing static recording | 130,404 samples / 275 frames processed on RX 7900 XTX; finite output, no clipping; RMS change −6.73 dB |
| Resident GPU feature sequence | 14 frames match CPU within 1.38e-6; all 62,453,348 parameter bytes uploaded once |
| Offline processing time | 2.717 seconds of static input processed in 0.236 seconds; 22.1× faster than v0.3.0; output samples identical |
| Bounded raw stream | CPU and AMD outputs match their offline counterparts exactly, including a partial final hop; invalid raw samples rejected |
| Paced raw transport | 24,000 samples delivered in 50 hops; first output before EOF; first output 63.6 ms after first input hop; this excludes audio devices |
| PipeWire virtual microphone | Synthetic source → AMD processor → virtual source → 24,000 recorded samples; finite nonzero audio; original description plus en-dash suffix verified |
| PipeWire lifecycle | Duration limit, SIGTERM, controller death, and source disappearance remove owned nodes; default-source metadata unchanged |
| Desktop interface | Native GTK3 window and settings dialog rendered; synthetic-source start/stop, live strength, a measured denoised-signal bar, preset save/load/delete, and reopening checked |
| Language selector | Five embedded flags and live translations checked; Brazilian Portuguese persisted across reopening; virtual-source serial unchanged during Spanish-to-Greek switch |
| Roadmap implementation | 100% of the current functionality ledger; GTK3 desktop controls implemented; validation contributes zero |
| Vendor audio-output parity | Pending vendor waveform reference; startup/flush and adaptive strength equivalence open |
| Recording quality and device latency | Speech preservation and physical-microphone-to-application latency unverified |

`gpu-check.json` contains the adapter identity and per-operation measurements.
`operator-check.json` covers the new operators, multi-frame graph, state changes,
and reset. `feature-file-check.json` covers the CLI feature sequence path;
`torch-reference.json` records a separate PyTorch implementation of the documented
graph. This reference uses the original weights with PyTorch convolution,
normalization, GRU-cell, and dense operators. It does not provide the vendor's
waveform output.

`audio-check.json` preserves the v0.3.0 native WAV baseline. An independent NumPy
float64 calculation provides the mel-feature and mask-reconstruction references.
The static recording was converted locally from 44.1 kHz FLAC to 48 kHz PCM16
before processing. Its RMS reduction is a signal-level observation; it does not
establish improved speech intelligibility or speech preservation. Audio fixtures
and user recordings remain outside the deliverable source tree.

`resident-check.json` records the GPU residency changes, comparison against the
earlier waveform, raw streaming agreement, and paced pipe transport. Its timings
exclude model loading and initial GPU setup. File throughput and paced transport
do not establish device-to-application latency or scheduling deadlines.

`pipewire-check.json` records the live routing test. It used a generated tone
source, not the physical microphone. The sample-limited `pw-record` client returned
status 1 after delivering all 24,000 requested samples with empty stderr; the
captured byte count and finite, nonzero waveform were checked directly. The
explicit `make-default-mic` operation was not run; defaults remained unchanged.

`model-inventory.json` contains tensor names, sizes, ranges, and
input/payload hashes. `model-graph.json` contains layer dimensions and conventions.
The roadmap percentages are checked against its task ledger.

`controls-check.json` records measured RMS/peak reporting and atomic strength
updates on a paced synthetic stream. Invalid updates were rejected while the
previous strength remained active. `desktop-check.json` records native GTK3 UI
checks on an isolated X11 display using a generated PipeWire source. The UI tests
did not capture the physical microphone or change system defaults. The native
folder chooser and settings dialog were exercised; active preferences and named
presets persisted across reopening. Closing during processing removed owned nodes
and the live control file. These checks establish interface behavior, not speech
quality or vendor waveform equivalence.

`language-check.json` records the five-language flag menu, saved language choice,
legacy-settings compatibility, and preservation of the active synthetic audio
route while switching languages. Technical backend diagnostics and platform-owned
chooser content are not part of the application translation catalog.

`flag-assets-check.json` records the five upstream `flag-icons` SVGs decoded at
24×18 pixels. The original files are pinned and hashed in `ui/flags/SOURCE.json`;
the MIT notice is included beside the files and in the binary's `licenses` command.

## Model-free distribution packages — 0.6.1

The amd64 executable was rebuilt against Ubuntu 24.04 libraries. Its maximum
GLIBC symbol requirement is 2.39, replacing the prior host build's 2.43 requirement.
All linked libraries resolve within the isolated Ubuntu 24.04 and Debian 13
runtime roots; CLI help and embedded flag notices execute successfully there.
The GTK window also opens and exits cleanly with each runtime, using an isolated
`/usr/bin` test mount. Per-user XDG settings are saved successfully and audio
default metadata remains unchanged.

APT dependency resolution succeeds against signed Debian 13 and Ubuntu 24.04
indexes; the latter provides Mint 22.x's package baseline. The three archives
contain the same executable and 136 Rust dependency licence records/notices.
Checksums, root ownership, the desktop entry, and exclusion of NVIDIA model,
DLL, installer, and exported weight files were checked. Full distribution
installations, Mint-native VM execution, and physical-device audio on those
systems were not tested. See `package-check-0.6.1.json` for that historical package validation.
The Clearwave release checks are recorded in `package-check.json` and
`releases/validation.json`.

## Clearwave rename — 0.6.2

The source package, executable, application title in all five languages, desktop
entry, icon filename, build controls, and internal PipeWire nodes now use
Clearwave. Installed preferences from the previous name are read when new
preferences are absent; saving uses the Clearwave data directory. Both isolated
Ubuntu and Debian GTK runs preserved the old preferences and saved the same
language and strength under the new name.

All 25 Rust unit checks, formatting, and desktop-entry validation passed.
The model still loads from the relocated bundle. The synthetic AMD/PipeWire run
recorded 24,000 finite nonzero samples through `clearwave.microphone`; duration,
signal, parent-death, and source-loss cleanup passed without changing defaults.
The descriptive virtual-microphone suffix remains unchanged.

The three rebuilt amd64 packages passed content, executable-hash, ownership,
licence-notice, and dependency-resolution checks. Native GTK startup and
preference migration were checked with Ubuntu 24.04 and Debian 13 library roots.
These checks do not establish full distribution installation, physical-device
audio quality, or vendor waveform parity. See `package-check.json`,
`clearwave-pipewire-check.json`, and `releases/validation.json`.

The publication build embeds Clearwave’s MIT license in `licenses`; all three
packages include the same LICENSE and retain 136 Rust dependency notices.
Package hashes, binary identity, root ownership, and exclusion of NVIDIA files
were checked again after this licensing-only update.

## Responsive folder picker — 0.6.3

All 27 Rust tests pass, including bounded folder-selection protocol validation
and cancellation reaping the owned child. The folder picker now runs as a
separate GTK process supervised by a worker thread, and Settings is non-modal.
The main GTK thread does not wait for the chooser process or perform its folder
browsing.

The native X11 regression selected and persisted a real local model folder,
then deliberately froze chooser helpers with SIGSTOP. Cancel removed the frozen
helper in 303 ms including the test's 200 ms click delay; Settings close also
cancelled it. The main window closed normally in 16 ms with a frozen helper,
and killing the application also killed its stopped helper. Startup and
preference migration passed with the Ubuntu 24.04 and Debian 13 runtime roots.
The original desktop's CPU spin did not reproduce in the isolated display; the
fault-injection check establishes cancellation and shutdown behavior even if a
chooser hangs. See `folder-picker-check.json`.

The development style guide is excluded from source release archives through
`.gitattributes`; the existing 0.6.2 local and GitHub ZIPs were checked after
refreshing archive metadata. Binary package allowlists already exclude it.
