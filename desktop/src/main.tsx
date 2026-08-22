import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { MantineProvider } from "@mantine/core";
import { App } from "./App";
import { defaultDesktopAdapters } from "./adapters/defaultAdapters";
import {
  initializeBrowserAppearance,
  projectAppearancePreference,
} from "./workbench/appearance";
import { createSettingsCorePort } from "./workbench/corePort";
import "./styles.css";
import "@mantine/core/styles.css";
import "@xyflow/react/dist/style.css";

const root = document.getElementById("root");
if (root === null) {
  throw new Error("Aworkit presentation root is missing");
}
async function revealDesktop(): Promise<void> {
  try {
    const settings = await createSettingsCorePort().snapshot();
    projectAppearancePreference(settings.appearance);
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
