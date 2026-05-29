import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import Hud from "./Hud";
import "./styles.css";

// The HUD overlay window is launched with ?view=hud; everything else renders
// the main application window.
const params = new URLSearchParams(window.location.search);
const isHud = params.get("view") === "hud";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>{isHud ? <Hud /> : <App />}</React.StrictMode>
);
