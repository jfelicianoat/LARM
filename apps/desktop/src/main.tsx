import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { runSmoke } from "./smoke";
import { isTauri } from "./tauri";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);

if (isTauri()) {
  void import("@tauri-apps/api/core").then(({ invoke }) => runSmoke(invoke)).catch((e) => console.error("smoke", e));
}
