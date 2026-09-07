import { Route, Router } from "@solidjs/router";
import { render } from "solid-js/web";

import { App, DirectoryRoute, Home, SessionRoute } from "./App.tsx";
import "./styles.css";

const root = document.getElementById("app");
if (!root) throw new Error("#app is missing from the page");

render(
  () => (
    <Router root={App}>
      <Route path="/" component={Home} />
      <Route path="/s/:sessionId" component={SessionRoute} />
      <Route path="/d/:cwd" component={DirectoryRoute} />
    </Router>
  ),
  root,
);
