# Native Windows build

Clearwave uses native windows-rs controls and a capture Audio Processing Object.
There is no Windows playback-output selector or virtual-cable requirement.
GTK and PipeWire are used only on Linux.

## Build

Use 64-bit Windows 10/11, Rust/Cargo 1.88 or newer, and Visual Studio Build Tools
with the desktop C++ workload and Windows SDK. In Developer PowerShell:

```powershell
cargo fetch --locked
.\build.ps1
```

`bin/` receives `clearwave.exe` and `clearwave_apo.dll`. `clean_build.ps1` builds
with an isolated temporary directory and removes intermediates. Package both
binaries with `packaging/windows/setup-apo.ps1` using
`python packaging/package_windows.py`. The executable embeds the common-controls
manifest; the APO DLL deliberately does not embed an application manifest.
GNU cross-compilation is also supported with MinGW-w64 and windres.

## Microphone processing

This is an unsigned **APO development preview**. Read `WINDOWS-APO.md` in the
archive (or `docs/WINDOWS_APO.md` in source) before setup. It describes installation,
removal, 48 kHz format requirements, 120 ms latency, fallback and deployment limits.
Start offers a short Yes/Cancel setup dialog when the selected microphone is not
registered. Setup runs asynchronously and requests elevation; the manual setup
button uses the same flow.
Applications continue selecting that original microphone. No kernel driver or
virtual microphone is created, and microphone audio is never sent to speakers.

A protected audio host may reject the unsigned DLL. An existing OEM mode/legacy
APO or registry protection can also prevent generic attachment. Production signing,
OEM integration and native Windows host verification remain outstanding; the
package does not change system security settings to work around them.

Model discovery, GPU work and the audio transport run outside the desktop thread.
Stop or closing Clearwave terminates its owned worker; the APO falls back to
aligned dry audio. Closing does not uninstall the effect. Disable Clearwave using
its setup script's Uninstall action to remove the persistent capture attachment.

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
