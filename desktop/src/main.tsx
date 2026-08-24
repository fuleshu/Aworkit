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
import "@mantine/core/styles.css";
import "@xyflow/react/dist/style.css";
import "./styles.css";

const root = document.getElementById("root");
if (root === null) {
  throw new Error("Aworkit presentation root is missing");
}
async function revealDesktop(): Promise<void> {
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
