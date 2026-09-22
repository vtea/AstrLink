// @vitest-environment happy-dom

import { StrictMode, act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AppLogRecord } from "./bridge";

const host = vi.hoisted(() => ({
  native: false,
  listen: vi.fn(),
  listeners: new Set<(event: { payload: AppLogRecord }) => void>(),
}));
vi.mock("@tauri-apps/api/core", () => ({ isTauri: () => host.native }));
vi.mock("@tauri-apps/api/event", () => ({ listen: host.listen }));

const bridge = vi.hoisted(() => ({
  getAppLogLocation: vi.fn(),
  listAppLogs: vi.fn(),
  revealAppLog: vi.fn(),
}));
vi.mock("./bridge", () => bridge);

const notifyMocks = vi.hoisted(() => ({
  success: vi.fn(),
  error: vi.fn(),
  warning: vi.fn(),
}));
vi.mock("./notify", () => ({ notify: notifyMocks }));

import { AppLogs } from "./AppLogs";

function record(sequence: number, message: string): AppLogRecord {
  return { sequence, time: "2026-09-22T00:00:00.000Z", level: "info", target: "test", message };
}

function emit(record: AppLogRecord) {
  for (const listener of host.listeners) listener({ payload: record });
}

describe("AppLogs", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    host.native = false;
    host.listeners.clear();
    host.listen.mockReset().mockImplementation(async (_name, callback) => {
      host.listeners.add(callback);
      return () => host.listeners.delete(callback);
    });
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    bridge.listAppLogs.mockReset().mockResolvedValue([]);
    bridge.revealAppLog.mockReset().mockResolvedValue(undefined);
    notifyMocks.error.mockReset();
  });

  afterEach(async () => {
    await act(async () => {
      window.getSelection()?.removeAllRanges();
      root.unmount();
    });
    container.remove();
  });

  it("shows an empty live list outside the desktop host", async () => {
    await act(async () => {
      root.render(<AppLogs />);
    });

    expect(container.textContent).toContain("尚无日志");
    expect(container.textContent).toContain("打开日志文件");
    expect(bridge.listAppLogs).not.toHaveBeenCalled();
  });

  it("waits for subscription and merges overlapping history with incoming logs", async () => {
    host.native = true;
    let subscribe!: () => void;
    host.listen.mockImplementation((_name, callback) => new Promise((resolve) => {
      subscribe = () => {
        host.listeners.add(callback);
        resolve(() => host.listeners.delete(callback));
      };
    }));
    let resolveHistory!: (records: AppLogRecord[]) => void;
    bridge.listAppLogs.mockImplementation(() => new Promise((resolve) => { resolveHistory = resolve; }));
    await act(async () => root.render(<AppLogs />));
    expect(bridge.listAppLogs).not.toHaveBeenCalled();
    await act(async () => subscribe());
    const first = record(1, "history");
    const overlap = record(2, "overlap");
    const newest = record(3, "live");
    await act(async () => {
      emit(newest);
      emit(overlap);
      resolveHistory([first, overlap]);
    });
    expect([...container.querySelectorAll("li span:last-child")].map((node) => node.textContent))
      .toEqual(["history", "overlap", "live"]);
  });

  it("keeps live events even when loading history fails", async () => {
    host.native = true;
    let rejectHistory!: (error: Error) => void;
    bridge.listAppLogs.mockImplementation(() => new Promise((_resolve, reject) => { rejectHistory = reject; }));
    await act(async () => root.render(<AppLogs />));
    await act(async () => {
      emit(record(1, "retained"));
      rejectHistory(new Error("history unavailable"));
    });
    expect(container.textContent).toContain("history unavailable");
    expect(container.querySelectorAll("li")).toHaveLength(1);
    await act(async () => emit(record(2, "next")));
    expect(container.querySelectorAll("li")).toHaveLength(2);
  });

  it("queues each log once while selecting text under StrictMode", async () => {
    host.native = true;
    bridge.listAppLogs.mockResolvedValue([record(1, "initial")]);
    await act(async () => root.render(<StrictMode><AppLogs /></StrictMode>));
    expect(host.listeners.size).toBe(1);
    await act(async () => {
      const range = document.createRange();
      range.selectNodeContents(container.querySelector("li span:last-child")!);
      window.getSelection()!.addRange(range);
      document.dispatchEvent(new Event("selectionchange"));
    });
    await act(async () => emit(record(2, "incoming")));
    expect(container.querySelectorAll("li")).toHaveLength(1);
    await act(async () => {
      window.getSelection()!.removeAllRanges();
      document.dispatchEvent(new Event("selectionchange"));
    });
    expect([...container.querySelectorAll("li")].filter((node) => node.textContent?.includes("incoming")))
      .toHaveLength(1);
  });
});
