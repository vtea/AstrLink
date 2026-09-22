import { isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useLayoutEffect, useRef, useState } from "react";

import { FilterSelect } from "@/components/FilterSelect";
import { FormMessage } from "@/components/FormMessage";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

import { listAppLogs, revealAppLog, type AppLogRecord } from "./bridge";
import { appLog, type LogLevel } from "./app-log";
import {
  APP_LOG_RECORD_EVENT,
  appendLogRecords,
  filterLogRecords,
  isLogLevel,
  logSelectionText,
  stickToTail,
} from "./app-log-view";
import { copyButtonLabel, useCopyFeedback } from "./copy-feedback";
import { i18n, useT } from "./i18n";
import { notify } from "./notify";
import { PageHeader } from "./PageHeader";

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : i18n.t("logs.failed");
}

function levelClass(level: string): string {
  if (level === "error") return "text-danger-foreground";
  if (level === "warn") return "text-warning-foreground";
  return "text-muted-foreground";
}

export function AppLogs({ detached = false }: { detached?: boolean }) {
  const t = useT();
  const [records, setRecords] = useState<AppLogRecord[]>([]);
  const [minimum, setMinimum] = useState<LogLevel | "">("");
  const [error, setError] = useState<string | null>(null);
  const [opening, setOpening] = useState(false);
  const [following, setFollowing] = useState(true);
  const scroller = useRef<HTMLDivElement>(null);
  const followingRef = useRef(true);
  const selectingRef = useRef(false);
  const queuedRef = useRef<AppLogRecord[]>([]);
  const copyFeedback = useCopyFeedback();

  useEffect(() => {
    if (!isTauri()) return;
    let active = true;
    let unlisten: (() => void) | undefined;
    let loading = true;
    let buffered: AppLogRecord[] = [];
    const receive = (incoming: AppLogRecord[]) => {
      if (!active) return;
      if (selectingRef.current) {
        queuedRef.current = appendLogRecords(queuedRef.current, incoming);
      } else {
        setRecords((current) => appendLogRecords(current, incoming));
      }
    };
    void listen<AppLogRecord>(APP_LOG_RECORD_EVENT, (event) => {
      const record = event.payload;
      if (
        !active || !record ||
        !Number.isSafeInteger(record.sequence) || record.sequence < 1 ||
        typeof record.time !== "string" ||
        typeof record.level !== "string" ||
        typeof record.target !== "string" ||
        typeof record.message !== "string"
      ) {
        return;
      }
      if (loading) buffered = appendLogRecords(buffered, [record]);
      else receive([record]);
    })
      .then(async (stop) => {
        if (!active) {
          stop();
          return;
        }
        unlisten = stop;
        // Subscribe first; merge events that overlap the history snapshot by ID.
        try {
          const history = await listAppLogs();
          receive(appendLogRecords(history, buffered));
        } catch (cause) {
          receive(buffered);
          if (active) setError(messageOf(cause));
        } finally {
          loading = false;
          buffered = [];
        }
      })
      .catch((cause: unknown) => {
        if (active) setError(messageOf(cause));
      });
    return () => {
      active = false;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    const onSelection = () => {
      const selected = logSelectionText(scroller.current).length > 0;
      if (selected) {
        if (!selectingRef.current) {
          selectingRef.current = true;
          followingRef.current = false;
          setFollowing(false);
        }
        return;
      }
      if (!selectingRef.current) return;
      selectingRef.current = false;
      const queued = queuedRef.current;
      if (queued.length === 0) return;
      queuedRef.current = [];
      setRecords((current) => appendLogRecords(current, queued));
    };
    document.addEventListener("selectionchange", onSelection);
    return () => document.removeEventListener("selectionchange", onSelection);
  }, []);

  useLayoutEffect(() => {
    const node = scroller.current;
    if (!node || !followingRef.current || selectingRef.current) return;
    node.scrollTop = node.scrollHeight;
  }, [records, minimum]);

  const visible = filterLogRecords(records, minimum);

  const openLog = async (): Promise<void> => {
    setOpening(true);
    setError(null);
    try {
      await revealAppLog();
    } catch (cause) {
      const message = messageOf(cause);
      setError(message);
      appLog.error("ui.logs", "Unable to reveal AstrLink log file", cause);
      notify.error(message);
    } finally {
      setOpening(false);
    }
  };

  return (
    <section className="flex min-h-0 flex-1 flex-col gap-3 pb-2">
      {detached ? null : (
        <PageHeader description={t("logs.description")} title={t("logs.title")} />
      )}
      {error ? <FormMessage tone="error">{error}</FormMessage> : null}
      <div className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-lg border bg-card">
        <div className="flex shrink-0 items-center justify-between gap-3 border-b px-3 py-2">
          <FilterSelect
            ariaLabel={t("logs.level")}
            className="w-36"
            label={t("logs.level")}
            onChange={(value) => setMinimum(isLogLevel(value) ? value : "")}
            options={[
              { value: "", label: t("logs.levelAll") },
              { value: "error", label: "error" },
              { value: "warn", label: "warn" },
              { value: "info", label: "info" },
              { value: "debug", label: "debug" },
              { value: "trace", label: "trace" },
            ]}
            value={minimum}
          />
          <div className="flex items-center gap-2">
            {following ? null : (
              <Button
                onClick={() => {
                  followingRef.current = true;
                  setFollowing(true);
                  const node = scroller.current;
                  if (node) node.scrollTop = node.scrollHeight;
                }}
                size="sm"
                type="button"
                variant="outline"
              >
                {t("logs.jumpToBottom")}
              </Button>
            )}
            <Button
              onClick={() => {
                const text = logSelectionText(scroller.current);
                if (!text) {
                  notify.warning(t("logs.copyEmpty"));
                  return;
                }
                copyFeedback.copy("logs", text);
              }}
              size="sm"
              type="button"
              variant="outline"
            >
              {copyButtonLabel(copyFeedback, "logs", t("logs.copy"))}
            </Button>
            <Button
              disabled={opening || !isTauri()}
              onClick={() => void openLog()}
              size="sm"
              type="button"
              variant="outline"
            >
              {opening ? t("logs.opening") : t("logs.open")}
            </Button>
          </div>
        </div>
        <div
          className="min-h-0 flex-1 select-text overflow-y-auto overscroll-none px-3 py-2 font-mono text-xs"
          onScroll={(event) => {
            const node = event.currentTarget;
            const next = stickToTail(
              node.scrollTop,
              node.clientHeight,
              node.scrollHeight,
            );
            followingRef.current = next;
            setFollowing(next);
          }}
          ref={scroller}
        >
          {visible.length === 0 ? (
            <p className="text-muted-foreground">{t("logs.empty")}</p>
          ) : (
            <ol className="grid cursor-text select-text gap-1">
              {visible.map((record) => (
                <li
                  className="grid grid-cols-[12.5rem_3.25rem_minmax(0,8rem)_minmax(0,1fr)] gap-2"
                  key={record.sequence}
                >
                  <span className="whitespace-nowrap text-muted-foreground tabular-nums">{record.time}</span>
                  <span className={cn("uppercase", levelClass(record.level))}>
                    {record.level}
                  </span>
                  <span className="truncate text-text-secondary" title={record.target}>
                    {record.target}
                  </span>
                  <span className="whitespace-pre-wrap break-all">{record.message}</span>
                </li>
              ))}
            </ol>
          )}
        </div>
      </div>
      {detached ? null : (
        <p className="text-xs text-muted-foreground">{t("logs.fileHint")}</p>
      )}
    </section>
  );
}
