// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { TrayNoticeEvent } from "./tray-notices";

const host = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
  isVisible: vi.fn(),
  onFocusChanged: vi.fn(),
  notices: new Set<(event: { payload: TrayNoticeEvent }) => void>(),
  focus: new Set<(event: { payload: boolean }) => void>(),
  error: vi.fn(),
  warning: vi.fn(),
  dismiss: vi.fn(),
}));
vi.mock("@tauri-apps/api/core", () => ({ isTauri: () => true, invoke: host.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: host.listen }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => host }));
vi.mock("./notify", () => ({ notify: { error: host.error, warning: host.warning, dismiss: host.dismiss } }));

import { useTrayNotices } from "./tray-notices";

const notice: TrayNoticeEvent = {
  action: "show", key: "gateway-error", level: "error", title: "Gateway failed",
  description: "Failure", target: "services", view_label: "View",
};

function Harness({ navigate }: { navigate: (page: { kind: "overview" } | { kind: "list" }) => void }) {
  useTrayNotices(navigate);
  return null;
}

function emit(event = notice) {
  for (const callback of host.notices) callback({ payload: event });
}

describe("tray notices", () => {
  let container: HTMLDivElement;
  let root: Root;
  beforeEach(() => {
    vi.clearAllMocks();
    host.notices.clear();
    host.focus.clear();
    host.isVisible.mockReset().mockResolvedValue(true);
    host.invoke.mockReset().mockResolvedValue(undefined);
    host.listen.mockReset().mockImplementation(async (_event, callback) => {
      host.notices.add(callback);
      return () => host.notices.delete(callback);
    });
    host.onFocusChanged.mockReset().mockImplementation(async (callback) => {
      host.focus.add(callback);
      return () => host.focus.delete(callback);
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });
  afterEach(async () => {
    await act(async () => root.unmount());
    container.remove();
  });

  it("receives a startup notice immediately upon the ready handshake", async () => {
    let finishSubscription!: () => void;
    host.listen.mockImplementation((_event, callback) => new Promise((resolve) => {
      finishSubscription = () => {
        host.notices.add(callback);
        resolve(() => host.notices.delete(callback));
      };
    }));
    host.invoke.mockImplementation(async (command) => {
      expect(command).toBe("tray_notice_ready");
      expect(host.notices.size).toBe(1);
      expect(host.focus.size).toBe(1);
      emit();
    });
    await act(async () => root.render(<Harness navigate={vi.fn()} />));
    expect(host.invoke).not.toHaveBeenCalled();
    await act(async () => finishSubscription());
    expect(host.error).toHaveBeenCalledWith(notice.title, expect.any(Object));
  });

  it("an existing toast invokes the current navigation guard after changing pages", async () => {
    const oldNavigate = vi.fn();
    await act(async () => root.render(<Harness navigate={oldNavigate} />));
    await act(async () => emit());
    const action = host.error.mock.calls[0][1].action.onClick;
    const confirmUnsaved = vi.fn();
    const currentNavigate = vi.fn(() => confirmUnsaved());
    await act(async () => root.render(<Harness navigate={currentNavigate} />));
    await act(async () => action());
    expect(oldNavigate).not.toHaveBeenCalled();
    expect(currentNavigate).toHaveBeenCalledWith({ kind: "list" });
    expect(confirmUnsaved).toHaveBeenCalledOnce();
    expect(host.listen).toHaveBeenCalledOnce();
    expect(host.invoke).toHaveBeenCalledOnce();
  });

  it("does not revive a dismissed notice after a delayed visibility check", async () => {
    let resolveVisibility!: (visible: boolean) => void;
    host.isVisible.mockImplementationOnce(() => new Promise((resolve) => { resolveVisibility = resolve; }));
    await act(async () => root.render(<Harness navigate={vi.fn()} />));
    await act(async () => emit());
    await act(async () => emit({ ...notice, action: "dismiss" }));
    await act(async () => resolveVisibility(true));
    expect(host.error).not.toHaveBeenCalled();
    expect(host.dismiss).toHaveBeenCalledWith("tray-status");
  });

  it("keeps a hidden notice across renders and presents it on focus", async () => {
    host.isVisible.mockResolvedValue(false);
    await act(async () => root.render(<Harness navigate={vi.fn()} />));
    await act(async () => emit());
    expect(host.error).not.toHaveBeenCalled();
    const navigate = vi.fn();
    await act(async () => root.render(<Harness navigate={navigate} />));
    host.isVisible.mockResolvedValue(true);
    await act(async () => {
      for (const callback of host.focus) callback({ payload: true });
    });
    expect(host.error).toHaveBeenCalledOnce();
    host.error.mock.calls[0][1].action.onClick();
    expect(navigate).toHaveBeenCalledWith({ kind: "list" });
  });

  it("cleans up a subscription that finishes after unmount without signalling ready", async () => {
    let finishSubscription!: () => void;
    const stop = vi.fn();
    host.listen.mockImplementation(() => new Promise((resolve) => {
      finishSubscription = () => resolve(stop);
    }));
    await act(async () => root.render(<Harness navigate={vi.fn()} />));
    await act(async () => root.render(null));
    await act(async () => finishSubscription());
    expect(stop).toHaveBeenCalledOnce();
    expect(host.invoke).not.toHaveBeenCalled();
  });
});
