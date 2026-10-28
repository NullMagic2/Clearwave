# Style guide

Use Rust 2024, `cargo fmt`, and `cargo clippy --all-targets -- -D warnings`.
Keep modules focused on package loading, tensor storage, GPU execution, or the
command-line interface. Prefer descriptive domain names and plain error messages.

## Reuse and structure

- Make code reusable. Before implementing behavior, check whether the project
  already provides it; reuse or extract the existing implementation first.
- Add blank lines between variable declaration blocks that serve different
  purposes. Keep closely related declarations together.
- Add a module comment describing each module's responsibility and its role in
  the application. Keep that comment to a maximum of 10 lines.
- Keep smaller explanatory comments to 3–4 lines. Explain purpose, constraints,
  or reasoning instead of narrating an obvious statement. Remove redundant comments.
- Avoid magic numbers. Give meaningful values named constants and reuse them.
  Prefer existing platform constants for operating-system values.
- Review whole files when changing code. Remove duplication, obsolete branches,
  and unnecessary abstractions when a simpler reusable design preserves behavior.

## Data and errors

- Check file bounds, sizes, signatures, versions, and dimensions before allocation
  or computation. Treat package and runtime files as input data.
- Use `Result` for operational failures. Avoid `unwrap`, `expect`, and unchecked
  indexing in input-dependent code.
- Keep the processing engine and shared application code safe Rust. Native
  Windows API calls belong only in the Windows interoperability boundary, with
  explicit safety explanations; the rest of the application denies unsafe code.
  Do not execute vendor libraries to load model data or silently substitute a
  different model or CPU backend.
- Preserve stored tensor ordering during export. Publish inferred dimensions
  separately until the complete graph contract is established.
- Keep audio and model processing local. Do not change default audio devices,
  install drivers, or enable background services as a side effect of inspection.
- Live capture requires an explicit source. Linux uses temporary PipeWire nodes;
  Windows uses the selected microphone’s capture APO and a separate GPU worker. Clean up owned
  subprocesses and preserve defaults unless the user explicitly requests a change.
  Linux virtual microphone descriptions append ` – Clearwave` using an en dash.
  Windows must never route the microphone to speakers or require a virtual cable.

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

Use native GTK3 widgets on Linux and native Windows controls through windows-rs
on Windows. Follow each platform's fonts, scaling, accessibility, and visual theme.
Keep the main window focused on
microphone selection, strength, the denoised signal bar, and starting/stopping.
Put advanced configuration in Settings. Save active settings automatically. Keep widget access on the main thread;
communicate with workers using bounded data messages. Avoid starting capture on launch. Display
the measured denoised output only and clear stale readings.

Keep application-owned translated text in `i18n.rs`. Use native language names
beside flags in the language menu. Embed flag assets; preserve device names and
the virtual-microphone suffix across languages. Language changes must preserve
active audio processing and settings compatibility.

The development style guide stays out of release archives and binary packages.
