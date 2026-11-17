import { defineConfig } from "tsup";

export default defineConfig({
  entry: ["src/index.ts"],
  format: ["esm"],
  dts: true,
  clean: true,
  // The wasm-bindgen glue finds its `.wasm` beside itself through
  // `import.meta.url`, so it is imported from `wasm/` at run time rather than
  // bundled into `dist/`, where that URL would point at nothing.
  external: [/\/wasm\//, "react"],
});
