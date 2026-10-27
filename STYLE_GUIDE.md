# Style guide

Use Rust 2024, `cargo fmt`, and `cargo clippy --all-targets -- -D warnings`.
Keep modules focused on package loading, tensor storage, GPU execution, or the
command-line interface. Prefer descriptive domain names and plain error messages.

## Data and errors

- Check file bounds, sizes, signatures, versions, and dimensions before allocation
  or computation. Treat package and runtime files as input data.
- Use `Result` for operational failures. Avoid `unwrap`, `expect`, and unchecked
  indexing in input-dependent code.
- Keep native unsafe code forbidden. Do not execute vendor libraries to load
  model data. Do not silently substitute a different model or CPU backend.
- Preserve stored tensor ordering during export. Publish inferred dimensions
  separately until the complete graph contract is established.
- Keep audio and model processing local. Do not change default audio devices,
  install drivers, or enable background services as a side effect of inspection.
- Live capture requires an explicit source. Use temporary PipeWire nodes, clean up
  owned subprocesses, and preserve current defaults unless the user invokes the
  explicit default-selection command. Virtual microphone descriptions append
  ` – Clearwave` to the original source description using an en dash.

## GPU code

Use portable WGSL with explicit dimensions and floating-point precision. Identify
the adapter and backend in reports. Describe operation scope precisely; a matrix
dispatch is not equivalent to end-to-end neural-network inference.

## Progress and validation

Count implementation only when executable behavior exists. Keep validation
results separate from implementation percentages. A passing numerical check
does not add implementation progress. Document unavailable features directly.

## Build output

`bin/` contains executable deliverables only. Source files, lockfiles, shaders,
and documentation stay in the source tree. Scratch, generated models, and
intermediate build files do not belong in `bin/`.

## Desktop interface

Use native GTK3 widgets and the user's theme. Keep the main window focused on
microphone selection, strength, the denoised signal bar, and starting/stopping.
Put advanced configuration in Settings. Save active settings automatically. Keep widget access on the main thread;
communicate with workers using bounded data messages. Avoid starting capture on launch. Display
the measured denoised output only and clear stale readings.

Keep application-owned translated text in `i18n.rs`. Use native language names
beside flags in the language menu. Embed flag assets; preserve device names and
the virtual-microphone suffix across languages. Language changes must preserve
active audio processing and settings compatibility.
