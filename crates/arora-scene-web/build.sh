#!/usr/bin/env bash
# Builds the browser host for one or both backends into `web/pkg-<backend>/`.
#
#   crates/arora-scene-web/build.sh            # webgpu and webgl2
#   crates/arora-scene-web/build.sh webgl2     # one backend
#
# Release profile (the workspace's: opt "s", fat LTO, one codegen unit), since
# the download size and the compiled code are both part of what is measured.
# Each backend has its own target directory, so switching does not rebuild.
#
# Needs wasm-bindgen-cli at exactly the version Cargo.lock pins for the
# wasm-bindgen crate; set WASM_BINDGEN to its path if it is not on PATH.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
bindgen="${WASM_BINDGEN:-wasm-bindgen}"
backends=("$@")
[ ${#backends[@]} -eq 0 ] && backends=(webgpu webgl2)
for backend in "${backends[@]}"; do
  echo "== $backend"
  start=$(date +%s)
  cargo build --release --target wasm32-unknown-unknown -p arora-scene-web \
    --features "$backend" --target-dir "$root/target/web-$backend" \
    --manifest-path "$root/Cargo.toml"
  echo "cargo build: $(( $(date +%s) - start ))s"
  out="$here/web/pkg-$backend"
  rm -rf "$out"
  # The name section is most of an unstripped module (59 of 98 MB here) and
  # only names functions for a debugger; no shipped build would carry it.
  "$bindgen" --target web --no-typescript --remove-name-section --remove-producers-section \
    --out-dir "$out" \
    "$root/target/web-$backend/wasm32-unknown-unknown/release/arora_scene_web.wasm"
  ls -l "$out"
done
