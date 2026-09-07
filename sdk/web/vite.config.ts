// Vite is here for one reason: Solid's JSX transform. Bun bundles TypeScript
// natively but does not do Solid's compile-time JSX, which is what turns a
// component into fine-grained DOM updates rather than a re-render.
//
// Bun stays the package manager and test runner; this config only builds and
// serves the page.
import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

export default defineConfig({
  plugins: [solid()],
  // Loopback only. The gateway this page talks to authenticates the whole
  // machine's agent behind one shared secret; a dev server reachable off-box is
  // an invitation to type that secret into it.
  server: { host: "127.0.0.1", port: 2421, strictPort: true },
  preview: { host: "127.0.0.1", port: 2421, strictPort: true },
  build: { outDir: "dist", target: "es2022" },
  resolve: {
    alias: {
      "@grok-build/theme": new URL("../theme/src/index.ts", import.meta.url).pathname,
    },
  },
});
