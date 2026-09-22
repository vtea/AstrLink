import { afterEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.hoisted(() => vi.fn());
const isTauriMock = vi.hoisted(() => vi.fn(() => false));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
  isTauri: isTauriMock,
}));

import {
  appLog,
  defaultAppLogThreshold,
  formatLogError,
  setAppLogThreshold,
  shouldEmitLog,
} from "./app-log";

afterEach(() => {
  invokeMock.mockReset();
  isTauriMock.mockReset();
  isTauriMock.mockReturnValue(false);
  setAppLogThreshold(defaultAppLogThreshold());
  vi.restoreAllMocks();
});

describe("appLog", () => {
  it("does not invoke below the threshold", () => {
    const debug = vi.spyOn(console, "debug").mockImplementation(() => undefined);
    isTauriMock.mockReturnValue(true);
    setAppLogThreshold("info");

    appLog.debug("ui.test", "hidden");

    expect(invokeMock).not.toHaveBeenCalled();
    expect(debug).not.toHaveBeenCalled();
  });

  it("sends only Error name and message through IPC", () => {
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    isTauriMock.mockReturnValue(true);
    invokeMock.mockResolvedValue(undefined);

    appLog.error("ui.test", "failed", new TypeError("boom"));

    expect(invokeMock).toHaveBeenCalledWith("append_app_log", {
      level: "error",
      target: "ui.test",
      message: "failed TypeError: boom",
    });
  });

  it("does not expand arbitrary objects into the file line", () => {
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    isTauriMock.mockReturnValue(true);
    invokeMock.mockResolvedValue(undefined);

    appLog.error("ui.test", "failed", { token: "secret" });

    expect(invokeMock).toHaveBeenCalledWith("append_app_log", {
      level: "error",
      target: "ui.test",
      message: "failed",
    });
  });

  it("does not invoke outside Tauri", () => {
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    isTauriMock.mockReturnValue(false);

    appLog.error("ui.test", "failed", new Error("boom"));

    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("mirrors to the console and swallows a failed append", async () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => undefined);
    isTauriMock.mockReturnValue(true);
    invokeMock.mockRejectedValue(new Error("ipc closed"));

    appLog.error("ui.theme", "Unable to observe AstrLink theme");
    await Promise.resolve();
    await Promise.resolve();

    expect(error).toHaveBeenCalledWith(
      "ui.theme: Unable to observe AstrLink theme",
    );
    expect(error).toHaveBeenCalledWith(
      "Unable to append AstrLink log",
      expect.any(Error),
    );
  });
});

describe("log helpers", () => {
  it("keeps Error text and ignores other values", () => {
    expect(formatLogError(new TypeError("boom"))).toBe("TypeError: boom");
    expect(formatLogError({ token: "secret" })).toBeUndefined();
    expect(formatLogError("plain")).toBeUndefined();
  });

  it("compares levels by rank", () => {
    expect(shouldEmitLog("error", "info")).toBe(true);
    expect(shouldEmitLog("debug", "info")).toBe(false);
  });
});
