// Test environment: a DOM, Solid's browser build, and Solid's JSX transform.
//
// Solid's reactivity is compile-time — `<For>` and a `{value()}` binding become
// direct DOM operations, not a render pass — so a component's `.tsx` has to go
// through `babel-preset-solid` before it runs. Vite does that when building the
// page; here the same preset is registered as a Bun loader, which is what keeps
// Bun as the test runner instead of dragging in a second one.
import { transformSync } from "@babel/core";
// @ts-expect-error -- babel-preset-solid ships no types.
import solidPreset from "babel-preset-solid";
import typescriptPreset from "@babel/preset-typescript";
import { GlobalRegistrator } from "@happy-dom/global-registrator";
import { plugin } from "bun";

GlobalRegistrator.register();

/**
 * Solid's browser builds, by specifier.
 *
 * **All three, not just `/web`.** Bun resolves with the `node` condition, and
 * every one of these packages points that condition at a *server* build.
 * Redirecting only the renderer left the reactive core on the server one, where
 * `createEffect` and `onMount` are literally `function (fn) {}` — so components
 * rendered once, statically, and nothing that depended on an effect or a
 * lifecycle could be tested at all. It failed silently, which is the worst way
 * for a test environment to be wrong.
 *
 * `web/dist/web.js` imports the core by bare specifier itself, which is why the
 * rewrite has to reach inside `node_modules/solid-js` too: otherwise the
 * renderer and the components would each hold a different copy of the runtime.
 */
const BROWSER_BUILD: Record<string, string> = {
  "solid-js": new URL("../node_modules/solid-js/dist/solid.js", import.meta.url).pathname,
  "solid-js/store": new URL("../node_modules/solid-js/store/dist/store.js", import.meta.url)
    .pathname,
  "solid-js/web": new URL("../node_modules/solid-js/web/dist/web.js", import.meta.url).pathname,
};

plugin({
  name: "solid",
  setup(build) {
    // `onResolve` does not fire for bare specifiers in a runtime plugin, so the
    // specifier is rewritten on load instead. The optional group is greedy, so
    // `solid-js/web` matches whole rather than as `solid-js` plus a suffix.
    const toBrowserBuild = (code: string): string =>
      code.replaceAll(
        /(["`'])(solid-js(?:\/(?:web|store))?)\1/g,
        (whole, _quote, specifier: string) =>
          JSON.stringify(BROWSER_BUILD[specifier] ?? whole),
      );

    const rewritten = (loader: "js" | "ts") => async (args: { path: string }) => ({
      contents: toBrowserBuild(await Bun.file(args.path).text()),
      loader,
    });

    build.onLoad({ filter: /node_modules\/@solidjs\/.*\.jsx?$/ }, rewritten("js"));
    build.onLoad({ filter: /node_modules\/solid-js\/.*\.js$/ }, rewritten("js"));
    // Our own non-component modules import the store and the core directly, so
    // they need the same redirect; they have no JSX, so they skip babel.
    build.onLoad({ filter: /\.ts$/ }, rewritten("ts"));

    build.onLoad({ filter: /\.tsx$/ }, (args) => {
      const source = Bun.file(args.path).text();
      return source.then((text) => {
        const result = transformSync(text, {
          filename: args.path,
          babelrc: false,
          configFile: false,
          presets: [
            typescriptPreset,
            [solidPreset, {}],
          ],
        });
        return { contents: toBrowserBuild(result?.code ?? text), loader: "js" };
      });
    });
  },
});
