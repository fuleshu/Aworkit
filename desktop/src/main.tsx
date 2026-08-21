import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { defaultDesktopAdapters } from "./adapters/defaultAdapters";
import "./styles.css";

const root = document.getElementById("root");
if (root === null) {
  throw new Error("Aworkit presentation root is missing");
}

createRoot(root).render(
  <StrictMode>
    <App adapters={defaultDesktopAdapters} />
  </StrictMode>,
);
