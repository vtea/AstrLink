import { useEffect, useLayoutEffect, useRef } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { appLog } from "./app-log";
import { notify } from "./notify";

const TRAY_NOTICE_ID = "tray-status";

export type TrayNoticeEvent = {
  action: "show" | "dismiss";
  key: string;
  level: "warning" | "error";
  title: string;
  description: string;
  target: "overview" | "services";
  view_label: string;
};

type NoticePage = { kind: "overview" } | { kind: "list" };

export function useTrayNotices(navigate: (page: NoticePage) => void): void {
  const navigateRef = useRef(navigate);
  useLayoutEffect(() => {
    navigateRef.current = navigate;
  }, [navigate]);

  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    let revision = 0;
    let unlisten: (() => void) | undefined;
    let unlistenFocus: (() => void) | undefined;
    let held: TrayNoticeEvent | null = null;

    const present = async (notice: TrayNoticeEvent) => {
      const current = ++revision;
      if (notice.action === "dismiss") {
        held = null;
        notify.dismiss(TRAY_NOTICE_ID);
        return;
      }
      const visible = await getCurrentWindow().isVisible().catch(() => false);
      if (disposed || current !== revision) return;
      if (!visible) {
        held = notice;
        return;
      }
      held = null;
      const show = notice.level === "error" ? notify.error : notify.warning;
      show(notice.title, {
        id: TRAY_NOTICE_ID,
        description: notice.description,
        duration: notice.level === "error" ? Number.POSITIVE_INFINITY : 8_000,
        action: {
          label: notice.view_label,
          onClick: () => {
            if (!disposed) {
              navigateRef.current(
                notice.target === "services" ? { kind: "list" } : { kind: "overview" },
              );
            }
          },
        },
      });
    };

    // The host keeps the current notice pending until both listeners exist.
    // A fresh handshake also replays it after a webview reload.
    const subscribe = async () => {
      const stop = await listen<TrayNoticeEvent>("tray-status-notice", (event) => {
        if (!disposed) void present(event.payload);
      });
      if (disposed) {
        stop();
        return;
      }
      unlisten = stop;
      const stopFocus = await getCurrentWindow().onFocusChanged(({ payload: focused }) => {
        if (focused && held && !disposed) void present(held);
      });
      if (disposed) {
        stopFocus();
        return;
      }
      unlistenFocus = stopFocus;
      await invoke("tray_notice_ready");
    };
    void subscribe().catch((cause: unknown) => {
      if (!disposed) appLog.error("ui.tray", "Unable to subscribe to tray notices", cause);
    });

    return () => {
      disposed = true;
      unlisten?.();
      unlistenFocus?.();
    };
  }, []);
}
