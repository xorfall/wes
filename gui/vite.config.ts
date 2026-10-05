import {createRequire} from "node:module";
import path from "node:path";
const require=createRequire(import.meta.url);
import { defineConfig, type ProxyOptions } from "vite";
import react from "@vitejs/plugin-react";

/**
 * The engine refuses cross-origin requests on purpose — it runs commands on this machine, and a page
 * from anywhere else must not be able to drive it. So in development the dev server proxies instead of
 * the engine relaxing; in use, the engine serves the built files itself and there is no other origin.
 */
/** Where the engine is. The default is its own default port; WES_ENGINE points at another one. */
const ENGINE = process.env["WES_ENGINE"] ?? "http://127.0.0.1:8099";

export default defineConfig({
  plugins: [react()],
  resolve: { dedupe: ["react", "react-dom"], alias: {
    react: path.dirname(require.resolve("react/package.json")),
    "react-dom": path.dirname(require.resolve("react-dom/package.json")),
  } },
  // The opaque View document permits data fonts only. Both clients consume these same faces.
  build: { outDir: "dist", assetsInlineLimit: file => file.endsWith(".woff2") ? true : undefined },
  server: {
    proxy: {
      "/submit": backend(),
      "/workspaces": backend(),
      "/diagnostics": backend(),
      "/values": backend(),
      "/complete": backend(),
      "/language": backend(),
      "/view-packages": backend(),
      "/view-instances": backend(),
      "/view-mounts": backend(),
      "/view-interaction": backend(),
      "/view-inputs": backend(),
      "/view-observation": backend(),
      "/live-view": backend(),
      "/traces": backend(),
      "/history": backend(),
      "/terminals": backend(),
      "/events": backend(true),
    },
  },
});

function backend(websocket = false): ProxyOptions {
  return { target: ENGINE, changeOrigin: true, ws: websocket,
    configure(proxy) {
      proxy.on("proxyReqWs", (request, incoming) => {
        if (incoming.headers.origin === undefined || incoming.headers.origin === `http://${incoming.headers.host}`) request.setHeader("Origin", ENGINE);
      });
      proxy.on("proxyReq", (request, incoming) => {
        if (incoming.headers.origin === undefined || incoming.headers.origin === `http://${incoming.headers.host}`) {
          request.setHeader("Origin", ENGINE);
        }
      });
    },
  };
}
