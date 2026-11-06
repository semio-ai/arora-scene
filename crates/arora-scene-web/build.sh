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
# A variant builds the same backend another way, side by side with the plain
# build, so the two can be served and measured together:
#
#   VARIANT=prof FEATURES=profile KEEP_NAMES=1 crates/arora-scene-web/build.sh webgpu
#
# writes `web/pkg-webgpuprof/` from its own target directory. VARIANT is
# lowercase letters and digits, which is what the bench server serves.
# KEEP_NAMES keeps the name section, so a profiler shows Rust function names.
#
# Needs wasm-bindgen-cli at exactly the version Cargo.lock pins for the
# wasm-bindgen crate; set WASM_BINDGEN to its path if it is not on PATH.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
bindgen="${WASM_BINDGEN:-wasm-bindgen}"
variant="${VARIANT:-}"
extra="${FEATURES:-}"
backends=("$@")
[ ${#backends[@]} -eq 0 ] && backends=(webgpu webgl2)
for backend in "${backends[@]}"; do
  echo "== $backend"
  start=$(date +%s)
  cargo build --release --target wasm32-unknown-unknown -p arora-scene-web \
    --features "$backend${extra:+,$extra}" --target-dir "$root/target/web-$backend$variant" \
    --manifest-path "$root/Cargo.toml"
  echo "cargo build: $(( $(date +%s) - start ))s"
  out="$here/web/pkg-$backend$variant"
  rm -rf "$out"
  # The name section is most of an unstripped module (59 of 98 MB here) and
  # only names functions for a debugger; no shipped build would carry it.
  strip=(--remove-name-section --remove-producers-section)
  [ -n "${KEEP_NAMES:-}" ] && strip=()
  "$bindgen" --target web --no-typescript ${strip[@]+"${strip[@]}"} \
    --out-dir "$out" \
    "$root/target/web-$backend$variant/wasm32-unknown-unknown/release/arora_scene_web.wasm"
  ls -l "$out"
done
