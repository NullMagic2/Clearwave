# Building the Linux packages

The release baseline is Ubuntu 24.04 amd64: glibc 2.39, GTK 3.24, and GLib 2.80.
A build linked against a newer host's glibc is rejected by `package.py`.

On an Ubuntu 24.04 development system with GTK3 development libraries installed:

```sh
./build.sh
python3 packaging/package.py
```

For an isolated sysroot build, extract authenticated Ubuntu 24.04 packages for
`libgtk-3-dev`, `libc6-dev`, `librsvg2-common`, and their dependencies into a private
folder. Ensure the usual merged-/usr aliases (`lib` → `usr/lib`, `lib64` →
`usr/lib64`, `bin` → `usr/bin`, `sbin` → `usr/sbin`) resolve within that folder.
No package maintainer scripts need to execute on the build host.

```sh
CLEARWAVE_SYSROOT=/absolute/path/to/noble-sysroot \
CLEARWAVE_BUILD_DIR=/absolute/path/to/scratch-build \
  ./packaging/build-compatible.sh
```

The build uses the locked offline Cargo dependencies, system GCC with the supplied
library root, and pkg-config paths confined to that root. The resulting ELF has
no embedded build-system library path. The build directory can be removed after
successful packaging. `bin/` contains one executable; `releases/` contains the
three `.deb` files, installation guide, manifest, and SHA-256 sums.

All three distributions use the same executable with different target labels and
package revisions. Package contents are assembled from an explicit allowlist;
`runtime/`, NVIDIA DLLs, the installer, and model weights are not copied. Rust
component and flag artwork notices are included under `/usr/share/doc/clearwave`.
Clearwave’s original source code is MIT licensed; packages include its LICENSE.
