import { App } from "./app.ts";

const root = document.getElementById("app");
if (!root) throw new Error("#app is missing from the page");
new App(root).mount();
