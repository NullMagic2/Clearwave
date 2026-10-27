#!/usr/bin/env bash
set -euo pipefail
: "${CLEARWAVE_SYSROOT:?Missing compatible system library root}"
voice_lib="$CLEARWAVE_SYSROOT/usr/lib/x86_64-linux-gnu"
exec gcc --sysroot="$CLEARWAVE_SYSROOT" -B"$voice_lib/" -L"$voice_lib" -Wl,-rpath-link,"$voice_lib" "$@"
