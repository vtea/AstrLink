import type { AppLogRecord } from "./bridge";
import { shouldEmitLog, type LogLevel } from "./app-log";

export const APP_LOG_RECORD_EVENT = "app-log-record";
export const APP_LOG_RECORD_LIMIT = 500;
const TAIL_SLACK_PX = 24;

const LEVELS: LogLevel[] = ["error", "warn", "info", "debug", "trace"];

export function isLogLevel(value: string): value is LogLevel {
  return LEVELS.some((level) => level === value);
}

export function appendLogRecords(
  items: AppLogRecord[],
  incoming: AppLogRecord[],
  limit = APP_LOG_RECORD_LIMIT,
): AppLogRecord[] {
  const bySequence = new Map(items.map((item) => [item.sequence, item]));
  for (const item of incoming) bySequence.set(item.sequence, item);
  const next = [...bySequence.values()].sort((a, b) => a.sequence - b.sequence);
  return next.length > limit ? next.slice(next.length - limit) : next;
}

export function filterLogRecords(
  items: AppLogRecord[],
  minimum: LogLevel | "",
): AppLogRecord[] {
  if (!minimum) return items;
  return items.filter(
    (item) => isLogLevel(item.level) && shouldEmitLog(item.level, minimum),
  );
}

export function formatLogRecordLine(record: AppLogRecord): string {
  return `${record.time} ${record.level.toUpperCase()} ${record.target} ${record.message}`;
}

export function logSelectionText(root: Node | null): string {
  if (!root || typeof window === "undefined") return "";
  const selection = window.getSelection();
  if (!selection || selection.isCollapsed || selection.rangeCount === 0) return "";
  const range = selection.getRangeAt(0);
  if (!root.contains(range.commonAncestorContainer)) return "";
  return selection.toString();
}

export function stickToTail(
  scrollTop: number,
  clientHeight: number,
  scrollHeight: number,
): boolean {
  return scrollHeight - (scrollTop + clientHeight) <= TAIL_SLACK_PX;
}
