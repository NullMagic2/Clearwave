# Clearwave 0.6.3 — Linux packages

Download the matching **amd64 / x86-64** package:

| Distribution | File |
| --- | --- |
| Debian 13 | `clearwave_0.6.3-1+debian13_amd64.deb` |
| Ubuntu 24.04 LTS or newer | `clearwave_0.6.3-1+ubuntu24.04_amd64.deb` |
| Linux Mint 22.x | `clearwave_0.6.3-1+mint22_amd64.deb` |

The three packages contain the same compatible executable. They are local,
unsigned builds. Older distribution releases and ARM processors are not targeted.

From the directory containing your download, install its exact filename:

```sh
sudo apt install ./clearwave_0.6.3-1+ubuntu24.04_amd64.deb
```

Substitute the Debian or Mint filename as appropriate. APT installs the declared
GTK3, SVG, Vulkan, and PipeWire dependencies. Launch **Clearwave** from the
application menu or run `clearwave`.

In Settings, choose your locally supplied extracted model bundle. NVIDIA assets
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
are not distributed. Obtain NVIDIA RTX Voice separately to supply its supported
model bundle; NVIDIA’s files remain under NVIDIA’s own license.
