// @vitest-environment happy-dom

import { describe, expect, it } from "vitest";

import type { AppLogRecord } from "./bridge";
import {
  appendLogRecords,
  filterLogRecords,
  formatLogRecordLine,
  logSelectionText,
  stickToTail,
} from "./app-log-view";

let sequence = 0;
function record(message: string, level = "info"): AppLogRecord {
  return {
    sequence: ++sequence,
    time: "2026-09-22T05:37:00.123Z",
    level,
    target: "shell.test",
    message,
  };
}

describe("app log view", () => {
  it("merges overlapping snapshots and out-of-order events by sequence", () => {
    const first = record("same");
    const second = record("same");
    const third = record("new");
    expect(appendLogRecords([second, third], [first, second])).toEqual([
      first, second, third,
    ]);
  });
  it("keeps the newest records when the list is full", () => {
    const items = Array.from({ length: 3 }, (_, index) => record(`row-${index}`));
    expect(appendLogRecords(items, [record("row-3")], 3).map((item) => item.message)).toEqual([
      "row-1",
      "row-2",
      "row-3",
    ]);
  });

  it("filters by minimum level", () => {
    const items = [record("debug", "debug"), record("warn", "warn")];
    expect(filterLogRecords(items, "warn").map((item) => item.message)).toEqual(["warn"]);
    expect(filterLogRecords(items, "").map((item) => item.message)).toEqual([
      "debug",
      "warn",
    ]);
  });

  it("formats a record as one copyable line", () => {
    expect(formatLogRecordLine(record("ingress cancelled", "warn"))).toBe(
      "2026-09-22T05:37:00.123Z WARN shell.test ingress cancelled",
    );
  });

  it("reads only a selection inside the log list", () => {
    document.body.innerHTML = `<div id="log"><span>kept</span></div><div id="other">outside</div>`;
    const log = document.getElementById("log");
    const kept = log?.querySelector("span");
    const outside = document.getElementById("other");
    if (!log || !kept || !outside) throw new Error("missing nodes");
    const range = document.createRange();
    range.selectNodeContents(kept);
    const selection = window.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
    expect(logSelectionText(log)).toBe("kept");
    range.selectNodeContents(outside);
    selection?.removeAllRanges();
    selection?.addRange(range);
    expect(logSelectionText(log)).toBe("");
  });

  it("follows only while the scroller is near the bottom", () => {
    expect(stickToTail(80, 20, 100)).toBe(true);
    expect(stickToTail(0, 20, 100)).toBe(false);
  });
});
