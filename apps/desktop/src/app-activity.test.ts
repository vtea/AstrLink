// @vitest-environment happy-dom

import { afterEach, describe, expect, it, vi } from "vitest";

import {
  describeChange,
  describeClick,
  describeWorkspacePage,
  installAppActionLogs,
} from "./app-activity";
import { appLog } from "./app-log";

afterEach(() => {
  document.body.innerHTML = "";
});

describe("app activity logs", () => {
  it("names the workspace page and the service being edited", () => {
    expect(describeWorkspacePage({ kind: "overview" })).toBe("page overview");
    expect(describeWorkspacePage({ kind: "edit", serviceId: "svc_1" })).toBe(
      "page edit svc_1",
    );
    expect(describeWorkspacePage({ kind: "records" })).toBe("page records");
    expect(
      describeWorkspacePage({ kind: "records", tokenId: "token_01" }),
    ).toBe("page records token_01");
  });

  it("logs the clicked control name", () => {
    document.body.innerHTML = `<button>获取模型列表</button><input id="search" />`;
    expect(describeClick(document.querySelector("button"))).toBe(
      "click 获取模型列表",
    );
    expect(describeClick(document.querySelector("input"))).toBeUndefined();
  });

  it("logs request row IDs without prompt previews or accessible text", () => {
    document.body.innerHTML = `<button data-session-id="session_123" aria-label="private prompt"><strong>private prompt with credentials</strong></button>`;
    expect(describeClick(document.querySelector("strong"))).toBe(
      "click request-session session_123",
    );
    document
      .querySelector("button")!
      .setAttribute("data-session-id", "private prompt");
    expect(describeClick(document.querySelector("strong"))).toBe(
      "click request-session",
    );
  });

  it("logs a selected option and a checkbox without free-text or secret values", () => {
    document.body.innerHTML = `
      <select aria-label="级别"><option>全部</option><option selected>error</option></select>
      <input type="checkbox" aria-label="启用" checked />
      <input type="password" aria-label="密钥" value="super-secret" />
      <input aria-label="搜索" value="gpt-5" />
    `;
    expect(describeChange(document.querySelector("select"))).toBe(
      "change 级别 error",
    );
    expect(
      describeChange(document.querySelector("input[type='checkbox']")),
    ).toBe("change 启用 on");
    const secret = describeChange(
      document.querySelector("input[type='password']"),
    );
    expect(secret).toBe("change 密钥");
    expect(secret).not.toContain("super-secret");
    const text = describeChange(
      document.querySelector("input[aria-label='搜索']"),
    );
    expect(text).toBe("change 搜索");
    expect(text).not.toContain("gpt-5");
  });

  it("writes click and change records through the shared listener", () => {
    const debug = vi.spyOn(appLog, "debug").mockImplementation(() => {});
    document.body.innerHTML = `<button>概览</button><select aria-label="级别"><option selected>info</option></select>`;
    const stop = installAppActionLogs();
    document
      .querySelector("button")
      ?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    document
      .querySelector("select")
      ?.dispatchEvent(new Event("change", { bubbles: true }));
    expect(debug).toHaveBeenCalledWith("ui.action", "click 概览");
    expect(debug).toHaveBeenCalledWith("ui.action", "change 级别 info");
    stop();
    debug.mockRestore();
  });
});
