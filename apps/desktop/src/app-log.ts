import { invoke, isTauri } from "@tauri-apps/api/core";

export type LogLevel = "error" | "warn" | "info" | "debug" | "trace";

const LEVEL_RANK: Record<LogLevel, number> = {
  error: 50,
  warn: 40,
  info: 30,
  debug: 20,
  trace: 10,
};

const TARGET_PATTERN = /^[A-Za-z0-9._-]{1,64}$/;

let threshold: LogLevel = defaultAppLogThreshold();

export function defaultAppLogThreshold(): LogLevel {
  return import.meta.env.DEV ? "debug" : "info";
}

export function setAppLogThreshold(level: LogLevel): void {
  threshold = level;
}

export function shouldEmitLog(level: LogLevel, minimum: LogLevel): boolean {
  return LEVEL_RANK[level] >= LEVEL_RANK[minimum];
}

export function isValidLogTarget(target: string): boolean {
  return TARGET_PATTERN.test(target);
}

export function formatLogError(cause: unknown): string | undefined {
  if (cause instanceof Error) {
    return `${cause.name}: ${cause.message}`;
  }
  return undefined;
}

function resolveMessage(message: string, cause: unknown): string {
  const detail = formatLogError(cause);
  return detail ? `${message} ${detail}` : message;
}

function mirrorConsole(
  level: LogLevel,
  target: string,
  message: string,
  cause: unknown,
): void {
  const line = `${target}: ${message}`;
  const extra = cause instanceof Error ? cause : undefined;
  switch (level) {
    case "error":
      if (extra) console.error(line, extra);
      else console.error(line);
      break;
    case "warn":
      if (extra) console.warn(line, extra);
      else console.warn(line);
      break;
    case "info":
      if (extra) console.info(line, extra);
      else console.info(line);
      break;
    case "debug":
    case "trace":
      if (extra) console.debug(line, extra);
      else console.debug(line);
      break;
  }
}

function emit(level: LogLevel, target: string, message: string, cause?: unknown): void {
  if (!shouldEmitLog(level, threshold)) {
    return;
  }
  mirrorConsole(level, target, message, cause);
  if (!isTauri() || !isValidLogTarget(target)) {
    return;
  }
  void invoke("append_app_log", {
    level,
    target,
    message: resolveMessage(message, cause),
  }).catch((error: unknown) => {
    console.error("Unable to append AstrLink log", error);
  });
}

export const appLog = {
  error(target: string, message: string, cause?: unknown): void {
    emit("error", target, message, cause);
  },
  warn(target: string, message: string, cause?: unknown): void {
    emit("warn", target, message, cause);
  },
  info(target: string, message: string, cause?: unknown): void {
    emit("info", target, message, cause);
  },
  debug(target: string, message: string, cause?: unknown): void {
    emit("debug", target, message, cause);
  },
  trace(target: string, message: string, cause?: unknown): void {
    emit("trace", target, message, cause);
  },
};
