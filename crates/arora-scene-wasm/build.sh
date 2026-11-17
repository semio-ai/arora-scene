#!/usr/bin/env bash
# Builds the wasm module for one or both backends into the npm package, at
# `packages/scene/wasm/<backend>/`, which the package's loader imports.
#
#   crates/arora-scene-wasm/build.sh            # webgpu and webgl2
#   crates/arora-scene-wasm/build.sh webgl2     # one backend
#
# The workspace's `web` profile (opt "z", fat LTO, one codegen unit), since the
# module is downloaded, then binaryen's `wasm-opt -Oz` when WASM_OPT names it
# (or it is on PATH): that pass shrinks the module by a fifth and, after opt
# "z", makes it faster rather than slower. Each backend has its own target
# directory, so switching does not rebuild.
#
# A variant builds the same backend another way, side by side with the plain
# build, so the two can be served and measured together:
#
#   VARIANT=prof FEATURES=bench,profile KEEP_NAMES=1 crates/arora-scene-wasm/build.sh webgpu
#
# writes `wasm/webgpuprof/` from its own target directory. KEEP_NAMES keeps the
# name section, so a profiler shows Rust function names. The render benchmark
# reads back what the `bench` feature records.
#
# Needs wasm-bindgen-cli at exactly the version Cargo.lock pins for the
# wasm-bindgen crate; set WASM_BINDGEN to its path if it is not on PATH.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
bindgen="${WASM_BINDGEN:-wasm-bindgen}"
variant="${VARIANT:-}"
extra="${FEATURES:-}"
wasm_opt="${WASM_OPT:-$(command -v wasm-opt || true)}"
backends=("$@")
[ ${#backends[@]} -eq 0 ] && backends=(webgpu webgl2)
for backend in "${backends[@]}"; do
  echo "== $backend"
  start=$(date +%s)
  cargo build --locked --profile web --target wasm32-unknown-unknown -p arora-scene-wasm \
    --features "$backend${extra:+,$extra}" --target-dir "$root/target/web-$backend$variant" \
    --manifest-path "$root/Cargo.toml"
  echo "cargo build: $(( $(date +%s) - start ))s"
  out="$root/packages/scene/wasm/$backend$variant"
  rm -rf "$out"
  # The name section is most of an unstripped module and only names functions
  # for a debugger; no shipped build carries it.
  strip=(--remove-name-section --remove-producers-section)
  [ -n "${KEEP_NAMES:-}" ] && strip=()
  "$bindgen" --target web ${strip[@]+"${strip[@]}"} \
    --out-dir "$out" \
    "$root/target/web-$backend$variant/wasm32-unknown-unknown/web/arora_scene_wasm.wasm"
  if [ -n "$wasm_opt" ] && [ -z "${KEEP_NAMES:-}" ]; then
    # The features rustc's wasm32 target emits by default; wasm-opt must be
    # told, or it refuses the module.
    "$wasm_opt" -Oz --enable-bulk-memory --enable-mutable-globals \
      --enable-nontrapping-float-to-int --enable-sign-ext \
      --enable-reference-types --enable-multivalue \
      "$out/arora_scene_wasm_bg.wasm" -o "$out/arora_scene_wasm_bg.wasm"
  fi
  ls -l "$out"
done
