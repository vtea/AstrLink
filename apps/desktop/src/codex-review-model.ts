export type CodexReviewModelState =
  | { kind: "bundled_catalog" }
  | { kind: "codex_auto_review" }
  | { kind: "session_model" }
  | { kind: "override"; model: string };

export interface CodexReviewModelStatus {
  detected: boolean;
  config_path: string;
  catalog_path: string;
  catalog_configured: boolean;
  catalog_exists: boolean;
  session_model: string | null;
  state: CodexReviewModelState;
  preview_paths: string[];
}

const STATUS_KEYS = [
  "detected",
  "config_path",
  "catalog_path",
  "catalog_configured",
  "catalog_exists",
  "session_model",
  "state",
  "preview_paths",
] as const;

function invalid(path: string, detail: string): never {
  throw new Error(
    `Invalid AstrLink Codex review-model IPC at ${path}: ${detail}`,
  );
}

function objectAt(value: unknown, path: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return invalid(path, "expected an object");
  }
  return value as Record<string, unknown>;
}

function stringAt(value: unknown, path: string, max = 8192): string {
  if (typeof value !== "string" || value.length === 0 || value.length > max) {
    return invalid(path, "expected a bounded non-empty string");
  }
  return value;
}

function booleanAt(value: unknown, path: string): boolean {
  if (typeof value !== "boolean") invalid(path, "expected a boolean");
  return value;
}

function parseState(value: unknown): CodexReviewModelState {
  const root = objectAt(value, "$.state");
  const allowed = root.kind === "override" ? ["kind", "model"] : ["kind"];
  for (const key of Object.keys(root)) {
    if (!allowed.includes(key)) invalid(`$.state.${key}`, "unexpected field");
  }
  switch (root.kind) {
    case "bundled_catalog":
    case "codex_auto_review":
    case "session_model":
      return { kind: root.kind };
    case "override":
      return {
        kind: "override",
        model: stringAt(root.model, "$.state.model", 256),
      };
    default:
      return invalid("$.state.kind", "unknown state");
  }
}

export function parseCodexReviewModelStatus(
  value: unknown,
): CodexReviewModelStatus {
  const root = objectAt(value, "$");
  for (const key of Object.keys(root)) {
    if (!(STATUS_KEYS as readonly string[]).includes(key)) {
      invalid(`$.${key}`, "unexpected field");
    }
  }
  if (!Array.isArray(root.preview_paths)) {
    invalid("$.preview_paths", "expected an array");
  }
  return {
    detected: booleanAt(root.detected, "$.detected"),
    config_path: stringAt(root.config_path, "$.config_path"),
    catalog_path: stringAt(root.catalog_path, "$.catalog_path"),
    catalog_configured: booleanAt(
      root.catalog_configured,
      "$.catalog_configured",
    ),
    catalog_exists: booleanAt(root.catalog_exists, "$.catalog_exists"),
    session_model:
      root.session_model === null
        ? null
        : stringAt(root.session_model, "$.session_model", 256),
    state: parseState(root.state),
    preview_paths: root.preview_paths.map((item, index) =>
      stringAt(item, `$.preview_paths[${index}]`),
    ),
  };
}

/** Distinct models from enabled services, which is what the gateway can route. */
export function reviewModelCandidates(
  services: readonly { enabled: boolean; models: readonly string[] }[],
): string[] {
  const models = new Set<string>();
  for (const service of services) {
    if (!service.enabled) continue;
    for (const model of service.models) {
      if (model.trim()) models.add(model);
    }
  }
  return [...models].sort((left, right) => left.localeCompare(right));
}
