#!/usr/bin/env bash
set -euo pipefail
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd -- "$project_dir"
cargo_cmd="${CARGO:-cargo}"
build_dir="$(mktemp -d -- "$project_dir/.clean-build.XXXXXXXX")"
cleanup() { rm -rf -- "$build_dir"; }
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
"$cargo_cmd" build --locked --offline --release --target-dir "$build_dir"
mkdir -p -- "$project_dir/bin"
install -m 755 -- "$build_dir/release/clearwave" "$project_dir/bin/.clearwave.new"
mv -f -- "$project_dir/bin/.clearwave.new" "$project_dir/bin/clearwave"
# Remove only this project's conventional build artifacts after a successful build.
rm -rf -- "$project_dir/.build" "$project_dir/target"
printf 'Built: %s (intermediate build files removed)\n' "$project_dir/bin/clearwave"
