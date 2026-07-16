import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { resolve } from "path";

const host = process.env.TAURI_DEV_HOST;

// Dev-only automation bridge: lets an external test harness run JS inside the
// live webview. POST /__auto/eval {code} → queued; the app polls /__auto/pull,
// evals, and POSTs {id, result|error} back to /__auto/result which the harness
// reads via GET /__auto/result/:id. Never part of production builds.
function devAutomationBridge(): Plugin {
  type Job = { id: number; code: string };
  const queue: Job[] = [];
  const results = new Map<number, string>();
  let nextId = 1;
  const readBody = (req: import("http").IncomingMessage): Promise<string> =>
    new Promise((res) => {
      let b = "";
      req.on("data", (c) => (b += c));
      req.on("end", () => res(b));
    });
  return {
    name: "dev-automation-bridge",
    apply: "serve",
    configureServer(server) {
      server.middlewares.use(async (req, res, next) => {
        const url = req.url || "";
        if (!url.startsWith("/__auto")) return next();
        res.setHeader("Content-Type", "application/json");
        if (req.method === "POST" && url === "/__auto/eval") {
          const code = await readBody(req);
          const id = nextId++;
          queue.push({ id, code });
          res.end(JSON.stringify({ id }));
        } else if (req.method === "GET" && url === "/__auto/pull") {
          res.end(JSON.stringify(queue.shift() ?? null));
        } else if (req.method === "POST" && url === "/__auto/result") {
          const { id, result } = JSON.parse(await readBody(req));
          results.set(id, result);
          res.end("{}");
        } else if (req.method === "GET" && url.startsWith("/__auto/result/")) {
          const id = Number(url.split("/").pop());
          const r = results.get(id);
          if (r !== undefined) results.delete(id);
          res.end(JSON.stringify({ done: r !== undefined, result: r ?? null }));
        } else {
          res.statusCode = 404;
          res.end("{}");
        }
      });
    },
  };
}

// https://vitejs.dev/config/
export default defineConfig(async () => ({
  plugins: [react(), tailwindcss(), devAutomationBridge()],

  // Path aliases
  resolve: {
    alias: {
      "@": resolve(__dirname, "./src"),
      "@/bindings": resolve(__dirname, "./src/bindings.ts"),
    },
  },

  // Multiple entry points for main app and overlay
  build: {
    rollupOptions: {
      input: {
        main: resolve(__dirname, "index.html"),
        overlay: resolve(__dirname, "src/overlay/index.html"),
        hotkeys: resolve(__dirname, "src/overlay/hotkeys.html"),
      },
    },
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
