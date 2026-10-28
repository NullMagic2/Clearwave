# Native Windows build

The Windows frontend uses Windows controls through windows-rs, Segoe UI,
common-controls theming, per-monitor DPI awareness, and the system folder picker.
The window and control surfaces use the native system dialog colors. The audio
path uses WASAPI through CPAL. GTK, PipeWire, and Linux command-line audio tools
are not used on Windows.

## Requirements

- 64-bit Windows 10 or 11, Rust/Cargo 1.88 or newer, and Visual Studio Build Tools
  with the C++ desktop workload and Windows SDK. Run from a Developer PowerShell
  so the linker and `rc.exe` resource compiler are available.
- An AMD graphics device with a working Vulkan driver.
- A locally supplied supported model. No NVIDIA files are included.
- A microphone and output endpoint supporting 48 kHz PCM. If unavailable, choose
  48 kHz in Windows Sound settings; Clearwave reports unsupported formats rather
  than substituting another processing model.

Fetch dependencies once with `cargo fetch --locked`, then run:

```powershell
./build.ps1
# Or remove intermediate build output after success:
./clean_build.ps1
./bin/clearwave.exe
```

The clean build preserves sources and replaces `bin/clearwave.exe` only after a
successful compilation. `CARGO` can select another Cargo executable. The packaged
GUI suppresses the console window; omit `--features windows-gui` in a manual
Cargo build when using the command-line tools in a terminal.

A Linux cross-build can use the Rust `x86_64-pc-windows-gnu` standard library and
MinGW-w64 linker/resource compiler:

```sh
cargo build --locked --release --target x86_64-pc-windows-gnu --features windows-gui
```

`WINDRES` can select a different GNU resource compiler. The GNU cross-build and
Windows-target Clippy passed locally. All 39 Windows-target
tests passed under Wine 11.20, including native controls, model-folder preference
overwrite, failed-save close behavior, keyboard order, and window/font disposal.
The language regression drives the embedded Windows combo with arrow keys,
checks translation and saved preferences for all five languages, and verifies
that the drop-down has room to display the choices.
Visual inspection in the Windows 11 Qemu Lightning VM confirmed matching window
and control surfaces, all five language choices in the expanded menu, and a
successful switch to Brazilian Portuguese before restoring English.
The Linux suite also passed all 42 tests. These compatibility checks do not
establish hardware audio behavior on Windows. The included Windows CI workflow
builds MSVC and runs the shared and native tests when it runs on GitHub.

## Routing and settings

Select the microphone and an explicit **Processed audio output**, then click
**Start**. To use it in calling or recording programs, select an already installed
virtual audio cable's playback endpoint as Clearwave's output and that cable's
paired recording endpoint in the other programs. Clearwave does not install an
audio driver, create a Windows recording endpoint, or change system defaults.
Choosing speakers sends the processed microphone to the speakers, so choose the
intended destination carefully to avoid acoustic feedback.

Preferences save automatically to `%LOCALAPPDATA%\Clearwave\runtime\desktop.json`.
They include the model folder, stable input/output IDs, AMD adapter, language, and
noise-removal strength. `gui --settings PATH` selects another preferences file.
Settings are compatible with the Linux schema; platform-specific device IDs must
be selected again when moving settings between operating systems.

Discovery and audio processing run outside the UI thread. The native folder
picker and audio worker run in owned subprocesses attached to Windows jobs, so
Cancel, Stop, closing the window, or process termination can release them without
waiting for a stuck dialog or GPU call on the message thread.

## Importing the model

Use **Installer…** in Settings to select the RTX Voice 0.5.12.6 or NVIDIA
Broadcast 2.3.0.71988672 setup executable. Broadcast import reuses a local model
cache or downloads approximately 33 MB from NVIDIA with Windows curl. It fetches
only model and decoder data, without CUDA or TensorRT runtime dependencies.
Install 7-Zip first; Clearwave detects `7z` on PATH or a standard Windows 7-Zip
installation. Extraction, graph validation, and weight export run in the owned
helper process. **Cancel** or closing Clearwave terminates that helper and its
archiver. The installer and vendor DLL are never executed. A completed import
is cached by installer SHA-256 beneath the app's runtime directory and saved as
the selected model folder. Incomplete staging folders are not used as models.
