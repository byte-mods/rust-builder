import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import "./App.css";
import "@xyflow/react/dist/style.css";

// Root mount. StrictMode is on so we catch unsafe lifecycles and double-fired
// effects during dev — particularly relevant when components fire fetches
// (see App.tsx's health probe), since StrictMode will invoke effects twice
// in dev and any non-idempotent request would surface as a bug here.
const rootEl = document.getElementById("root");
if (!rootEl) {
  throw new Error("root element missing from index.html");
}
createRoot(rootEl).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
