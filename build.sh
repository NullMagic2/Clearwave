#!/usr/bin/env bash
set -euo pipefail
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd -- "$project_dir"
cargo_cmd="${CARGO:-cargo}"
build_dir="$project_dir/.build"
"$cargo_cmd" build --locked --offline --release --target-dir "$build_dir"
mkdir -p -- "$project_dir/bin"
install -m 755 -- "$build_dir/release/clearwave" "$project_dir/bin/.clearwave.new"
mv -f -- "$project_dir/bin/.clearwave.new" "$project_dir/bin/clearwave"
printf 'Built: %s\n' "$project_dir/bin/clearwave"
