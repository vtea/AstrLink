import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { getDesktopPlatform } from "./window-chrome";

export const APP_LOG_WINDOW_LABEL = "app-log";

function windowLabel(): string | null {
  if (getDesktopPlatform() === "browser") return null;
  try {
    return getCurrentWindow().label;
  } catch {
    return null;
  }
}

export function isAppLogWindow(): boolean {
  return windowLabel() === APP_LOG_WINDOW_LABEL;
}

export function detachedLogWindowEnabled(): boolean {
  const label = windowLabel();
  return label !== null && label !== APP_LOG_WINDOW_LABEL;
}

export function showAppLogWindow(): Promise<void> {
  return invoke<void>("show_app_log_window");
}
