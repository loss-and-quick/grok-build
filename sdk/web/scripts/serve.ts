// Serves the built page on loopback.
//
// Static files only — this process never talks to the leader. The page opens
// its own WebSocket to `grok agent gateway`, so the gateway's shared secret
// stays between the browser and the gateway and never passes through here.
//
// Bound to 127.0.0.1 with no override: the gateway it fronts authenticates one
// secret for the whole machine's agent, and a page served off-box is an
// invitation to paste that secret into it.
import { file } from "bun";
import { resolve } from "node:path";

const PUBLIC_DIR = resolve(import.meta.dir, "..", "public");
const PORT = Number(process.env["GROK_WEB_PORT"] ?? 2421);

const TYPES: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
};

const server = Bun.serve({
  hostname: "127.0.0.1",
  port: PORT,
  async fetch(request) {
    const path = new URL(request.url).pathname;
    const name = path === "/" ? "index.html" : path.slice(1);
    // Reject anything that escapes `public/`, including encoded traversal.
    const target = resolve(PUBLIC_DIR, name);
    if (target !== PUBLIC_DIR && !target.startsWith(`${PUBLIC_DIR}/`)) {
      return new Response("not found", { status: 404 });
    }
    const asset = file(target);
    if (!(await asset.exists())) return new Response("not found", { status: 404 });
    const ext = target.slice(target.lastIndexOf("."));
    return new Response(asset, {
      headers: { "content-type": TYPES[ext] ?? "application/octet-stream" },
    });
  },
});

console.log(`grok web client on http://${server.hostname}:${server.port}`);
