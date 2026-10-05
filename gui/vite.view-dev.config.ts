import {createRequire} from "node:module";
import path from "node:path";
const require=createRequire(import.meta.url);
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";

export default defineConfig({
  root: fileURLToPath(new URL("../tools/view-dev", import.meta.url)),
  plugins: [react()],
  resolve: { dedupe: ["react", "react-dom"], alias: {
    react: path.dirname(require.resolve("react/package.json")),
    "react-dom": path.dirname(require.resolve("react-dom/package.json")),
  } },
  server: { host: "127.0.0.1", fs: { allow: [fileURLToPath(new URL("..", import.meta.url))] } },
  build: { outDir: "dist", emptyOutDir: true },
});
