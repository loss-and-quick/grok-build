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

const SOLID_WEB_BROWSER = new URL("../node_modules/solid-js/web/dist/web.js", import.meta.url)
  .pathname;

plugin({
  name: "solid",
  setup(build) {
    // Bun resolves with the `node` condition, and Solid's `node` export is its
    // *server* renderer, which throws "Client-only API called on the server
    // side" the moment a component mounts. `onResolve` does not fire for bare
    // specifiers in a runtime plugin, so the specifier is rewritten on load
    // instead — in our own modules and in the two `@solidjs` packages that
    // import it.
    const toBrowserBuild = (code: string): string =>
      code.replaceAll(/(["`'])solid-js\/web\1/g, JSON.stringify(SOLID_WEB_BROWSER));

    build.onLoad({ filter: /node_modules\/@solidjs\/.*\.jsx?$/ }, async (args) => ({
      contents: toBrowserBuild(await Bun.file(args.path).text()),
      loader: "js",
    }));

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
