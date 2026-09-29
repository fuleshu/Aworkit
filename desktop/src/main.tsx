import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { MantineProvider } from "@mantine/core";
import { App } from "./App";
import { defaultDesktopAdapters } from "./adapters/defaultAdapters";
import {
  initializeBrowserAppearance,
  projectAppearancePreference,
} from "./workbench/appearance";
import { createSettingsV2CorePort } from "./workbench/settingsV2Port";
import { initializeSystemTextScale } from "./workbench/systemTextScale";
import "@mantine/core/styles.css";
import "@xyflow/react/dist/style.css";
import "./styles.css";
import "./typography.css";

const root = document.getElementById("root");
if (root === null) {
  throw new Error("Aworkit presentation root is missing");
}
// The window's native drag-drop interception is disabled so the composer can
// receive HTML5 file drops. Without this guard a file dropped anywhere else
// would navigate the webview to the dropped file. The composer's own handlers
// run first and stop propagation for the drops they accept.
window.addEventListener("dragover", (event) => event.preventDefault());
window.addEventListener("drop", (event) => event.preventDefault());
async function revealDesktop(): Promise<void> {
  try {
    await initializeSystemTextScale();
  } catch (error) {
    console.warn("Operating-system text scaling is unavailable", error);
  }
  try {
    const settings = (await createSettingsV2CorePort().snapshot()).settings;
    projectAppearancePreference(
      settings.appearance.mode,
      settings.appearance.fontScale,
    );
  } catch {
    // The pre-rendered System appearance remains a safe non-persistent fallback.
  }
  initializeBrowserAppearance();
  document.documentElement.dataset.appearanceReady = "true";
  createRoot(root!).render(
    <StrictMode>
      <MantineProvider defaultColorScheme="auto">
        <App adapters={defaultDesktopAdapters} />
      </MantineProvider>
    </StrictMode>,
  );
}

void revealDesktop();
