import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import App from "./App";
import { FileManagerWindow } from "./components/FileManagerWindow";
import { SettingsWindow } from "./components/SettingsWindow";
import { applyThemeClass, resolveStoredTheme } from "./hooks/useTheme";
import "./i18n";
import "./index.css";
import { INPUT_BAR_HEIGHT } from "./lib/utils";

/**
 * Which window this webview is: the mirror, the settings window opened by
 * `open_settings_window`, or the file manager opened by
 * `open_file_manager_window` (both in src-tauri/src/lib.rs). All of them load
 * the same bundle, so the label is the only thing that tells them apart. Rust
 * names the windows "settings" and "file-manager"; anything else (including a
 * failed lookup, i.e. a plain browser during `bun run dev`) is the app.
 */
function windowLabel(): string {
  try {
    return getCurrentWindow().label;
  } catch {
    return "main";
  }
}

const label = windowLabel();
const settingsWindow = label === "settings";
const filesWindow = label === "file-manager";

if (!settingsWindow && !filesWindow) {
  // Text-input bar height, shared by the Rust-side window math and the CSS.
  document.documentElement.style.setProperty("--input-h", `${INPUT_BAR_HEIGHT}px`);
} else {
  // The satellite windows paint the stored theme before their first render:
  // the main window owns the theme and pushes it with the state snapshot, but
  // that arrives a moment later, and without this a dark-themed app would
  // flash a white window first.
  applyThemeClass(resolveStoredTheme());
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    {settingsWindow ? <SettingsWindow /> : filesWindow ? <FileManagerWindow /> : <App />}
  </React.StrictMode>,
);
