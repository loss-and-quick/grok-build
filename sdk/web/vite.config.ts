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
  //
  // `/ws` is proxied to the gateway so that a page from this dev server and a
  // page served by `grok web` reach their socket the same way: from their own
  // origin. Without the proxy, development would be the only case that takes the
  // "ask for an address" path, which is the case least likely to be noticed when
  // it breaks.
  server: {
    host: "127.0.0.1",
    port: 2421,
    strictPort: true,
    proxy: { "/ws": { target: "ws://127.0.0.1:2420", ws: true } },
  },
  preview: {
    host: "127.0.0.1",
    port: 2421,
    strictPort: true,
    proxy: { "/ws": { target: "ws://127.0.0.1:2420", ws: true } },
  },
  build: { outDir: "dist", target: "es2022" },
  resolve: {
    alias: {
      "@grok-build/theme": new URL("../theme/src/index.ts", import.meta.url).pathname,
    },
  },
});
