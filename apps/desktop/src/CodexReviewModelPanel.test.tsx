// @vitest-environment happy-dom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const bridge = vi.hoisted(() => ({
  getCodexReviewModelStatus: vi.fn(),
  setCodexReviewModel: vi.fn(),
  listServices: vi.fn(),
}));
vi.mock("./bridge", () => bridge);

const notifyMocks = vi.hoisted(() => ({
  success: vi.fn(),
  error: vi.fn(),
  warning: vi.fn(),
}));
vi.mock("./notify", () => ({ notify: notifyMocks }));

import { CodexReviewModelPanel } from "./CodexReviewModelPanel";
import {
  parseCodexReviewModelStatus,
  reviewModelCandidates,
} from "./codex-review-model";

const baseStatus = {
  detected: true,
  config_path: "/tmp/.codex/config.toml",
  catalog_path: "/tmp/.codex/model-catalog.json",
  catalog_configured: true,
  catalog_exists: true,
  session_model: "gpt-6-astra",
  state: { kind: "codex_auto_review" },
  preview_paths: ["/tmp/.codex/model-catalog.json"],
};

const services = {
  items: [
    { enabled: true, models: ["gpt-6-astra", "claude-sonnet-5"] },
    { enabled: true, models: ["glm-5", "gpt-6-astra"] },
    { enabled: false, models: ["disabled-model"] },
  ],
  next_cursor: null,
};

async function flush(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

async function chooseOption(label: string, option: string): Promise<void> {
  const trigger = document.querySelector<HTMLButtonElement>(
    `button[role="combobox"][aria-label="${label}"]`,
  );
  if (!trigger) throw new Error(`Missing select trigger: ${label}`);
  await act(async () => {
    trigger.dispatchEvent(
      new PointerEvent("pointerdown", {
        bubbles: true,
        button: 0,
        pointerType: "mouse",
      }),
    );
    await Promise.resolve();
  });
  const item = [
    ...document.querySelectorAll<HTMLElement>('[role="option"]'),
  ].find((candidate) => candidate.textContent?.trim() === option);
  if (!item) throw new Error(`Missing select option: ${option}`);
  await act(async () => {
    item.click();
    await Promise.resolve();
  });
}

function button(text: string, scope: ParentNode = document): HTMLButtonElement {
  const found = [...scope.querySelectorAll("button")].find(
    (item) => item.textContent === text,
  );
  if (!found) throw new Error(`Missing button: ${text}`);
  return found;
}

describe("CodexReviewModelPanel", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    bridge.getCodexReviewModelStatus.mockReset().mockResolvedValue(baseStatus);
    bridge.listServices.mockReset().mockResolvedValue(services);
    bridge.setCodexReviewModel.mockReset().mockResolvedValue({
      ...baseStatus,
      state: { kind: "override", model: "claude-sonnet-5" },
    });
    notifyMocks.success.mockReset();
  });

  afterEach(async () => {
    await act(async () => root.unmount());
    container.remove();
  });

  it("offers enabled service models from any provider and saves after confirmation", async () => {
    await act(async () => {
      root.render(<CodexReviewModelPanel />);
      await flush();
    });
    expect(container.textContent).toContain("Codex 自动审批模型");
    expect(container.textContent).toContain("审批请求会失败");

    await chooseOption("审批模型", "claude-sonnet-5");

    await act(async () => button("保存", container).click());
    expect(bridge.setCodexReviewModel).not.toHaveBeenCalled();
    const dialog = document.querySelector("[role='alertdialog']")!;
    expect(dialog.textContent).toContain("claude-sonnet-5");
    expect(dialog.textContent).toContain("/tmp/.codex/model-catalog.json");
    expect(dialog.textContent).not.toContain("本地模型目录");
    await act(async () => {
      button("保存", dialog).click();
      await flush();
    });
    expect(bridge.setCodexReviewModel).toHaveBeenCalledExactlyOnceWith(
      "claude-sonnet-5",
    );
    expect(notifyMocks.success).toHaveBeenCalled();
    expect(container.textContent).toContain("当前：claude-sonnet-5");
    expect(button("保存", container).disabled).toBe(true);
  });

  it("does not warn when an enabled service provides codex-auto-review", async () => {
    bridge.listServices.mockResolvedValue({
      ...services,
      items: [
        ...services.items,
        { enabled: true, models: ["codex-auto-review"] },
      ],
    });
    await act(async () => {
      root.render(<CodexReviewModelPanel />);
      await flush();
    });

    expect(container.textContent).not.toContain("审批请求会失败");
  });

  it("warns that the first save pins the local catalog", async () => {
    bridge.getCodexReviewModelStatus.mockResolvedValue({
      ...baseStatus,
      catalog_configured: false,
      state: { kind: "bundled_catalog" },
    });
    await act(async () => {
      root.render(<CodexReviewModelPanel />);
      await flush();
    });
    await act(async () => button("保存", container).click());
    expect(
      document.querySelector("[role='alertdialog']")?.textContent,
    ).toContain("本地模型目录");
  });

  it("follows the session model with a null model", async () => {
    await act(async () => {
      root.render(<CodexReviewModelPanel />);
      await flush();
    });
    await act(async () => button("保存", container).click());
    await act(async () => {
      button("保存", document.querySelector("[role='alertdialog']")!).click();
      await flush();
    });
    expect(bridge.setCodexReviewModel).toHaveBeenCalledExactlyOnceWith(null);
  });

  it("renders nothing when Codex is not detected", async () => {
    bridge.getCodexReviewModelStatus.mockResolvedValue({
      ...baseStatus,
      detected: false,
    });
    await act(async () => {
      root.render(<CodexReviewModelPanel />);
      await flush();
    });
    expect(container.textContent).toBe("");
  });

  it("parses IPC strictly and deduplicates candidates", () => {
    expect(
      parseCodexReviewModelStatus({
        ...baseStatus,
        state: { kind: "override", model: "glm-5" },
      }).state,
    ).toEqual({ kind: "override", model: "glm-5" });
    expect(() =>
      parseCodexReviewModelStatus({ ...baseStatus, extra: true }),
    ).toThrow("unexpected field");
    expect(() =>
      parseCodexReviewModelStatus({ ...baseStatus, state: { kind: "nope" } }),
    ).toThrow("unknown state");
    expect(() =>
      parseCodexReviewModelStatus({
        ...baseStatus,
        state: { kind: "session_model", extra: true },
      }),
    ).toThrow("unexpected field");
    expect(reviewModelCandidates(services.items)).toEqual([
      "claude-sonnet-5",
      "glm-5",
      "gpt-6-astra",
    ]);
  });
});
