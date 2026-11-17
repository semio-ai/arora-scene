import { fileURLToPath } from "node:url";

import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// The example imports the package from source, so it runs against whatever
// `npm run build:wasm` last wrote into `wasm/`.
export default defineConfig({
  root: fileURLToPath(new URL(".", import.meta.url)),
  plugins: [react()],
  server: { fs: { allow: [".."] } },
});
