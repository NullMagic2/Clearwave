# Clearwave 0.7.1-dev.4 — Linux packages

**Experimental pre-release.** This build corrects Broadcast 2.3 framing,
strength scaling, and quiet-input conditioning, adds optional echo reduction,
and improves Windows device discovery and shutdown. Fourteen fixed-delay
comparisons against the NVIDIA audio runtime on AMD meet the recorded limits;
native NVIDIA hardware equivalence and physical-device latency remain unverified.

Download the matching **amd64 / x86-64** package:

| Distribution | File |
| --- | --- |
| Debian 13 | `clearwave_0.7.1-dev.4-1+debian13_amd64.deb` |
| Ubuntu 24.04 LTS or newer | `clearwave_0.7.1-dev.4-1+ubuntu24.04_amd64.deb` |
| Linux Mint 22.x | `clearwave_0.7.1-dev.4-1+mint22_amd64.deb` |

The three packages contain the same compatible executable. They are local,
unsigned builds. Older distribution releases and ARM processors are not targeted.

From the directory containing your download, install its exact filename:

```sh
sudo apt install ./clearwave_0.7.1-dev.4-1+ubuntu24.04_amd64.deb
```

Substitute the Debian or Mint filename as appropriate. APT installs the declared
GTK3, SVG, Vulkan, and PipeWire dependencies. Launch **Clearwave** from the
application menu or run `clearwave`.

In Settings, **Installer…** imports a locally obtained RTX Voice 0.5.12.6 or
Broadcast 2.3.0.71988672 setup executable in the background when 7-Zip is installed
(`sudo apt install 7zip curl`,
or `p7zip-full` on systems providing that package). The installer is never run;
its validated model is saved locally and reused. Broadcast first checks local
model locations, then downloads about 33 MB directly from NVIDIA when needed.
**Cancel** stops the import.
Alternatively, choose the folder containing your local `voice-weights.safetensors`
(or `model.safetensors`) for DLL-free processing. Original encrypted model/DLL
pairs may also be directly in the selected folder or under their original
`NvAFX` subfolders. NVIDIA assets
are **not included**. The app starts with capture stopped. It requires an AMD
Vulkan driver and a running PipeWire session. Language selection, strength, and
the single denoised-signal meter are included.

Installed settings live in
`${XDG_DATA_HOME:-$HOME/.local/share}/clearwave/runtime/desktop.json`.

Verify package hashes with `sha256sum -c SHA256SUMS` when all three downloads are
present, or compare the selected package's hash with its line in `SHA256SUMS`.
Uninstall using `sudo apt remove clearwave`; per-user settings are preserved.

Installing Clearwave replaces the previous `rtx-voice-amd` package. Existing
installed preferences are read on first launch and saved under the Clearwave
data directory; model folders selected in those preferences remain usable.

Clearwave’s original code is MIT licensed. For legal reasons, NVIDIA model files
are not distributed. Obtain NVIDIA RTX Voice or Broadcast separately to supply a supported
model bundle; NVIDIA’s files remain under NVIDIA’s own license.
