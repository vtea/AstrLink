import { appLog } from "./app-log";

const CLICKABLE = [
  "button",
  "a",
  "summary",
  "[data-slot='button']",
  "[role='button']",
  "[role='tab']",
  "[role='menuitem']",
  "[role='option']",
  "[role='switch']",
  "[role='checkbox']",
  "[role='radio']",
].join(", ");

const SECRET_HINT = /password|token|secret|credential|api[-_]?key/i;

export function describeWorkspacePage(page: {
  kind: string;
  serviceId?: string;
  tokenId?: string;
}): string {
  if (page.kind === "edit" && page.serviceId) {
    return `page edit ${page.serviceId}`;
  }
  if (page.kind === "records" && page.tokenId) {
    return `page records ${page.tokenId}`;
  }
  return `page ${page.kind}`;
}

function collapse(value: string): string {
  return value.replace(/\s+/g, " ").trim().slice(0, 80);
}

function controlHint(element: Element): string {
  return [
    element.getAttribute("type"),
    element.getAttribute("name"),
    element.getAttribute("id"),
    element.getAttribute("autocomplete"),
    element.getAttribute("aria-label"),
  ]
    .filter((part): part is string => Boolean(part))
    .join(" ");
}

function isSecretControl(element: Element): boolean {
  const type = (element.getAttribute("type") ?? "").toLowerCase();
  return type === "password" || SECRET_HINT.test(controlHint(element));
}

export function actionLabel(element: Element): string {
  // Request rows contain prompt previews. Only log their opaque identifier,
  // never their visible text (or an accessible label derived from that text).
  const sessionId = element.getAttribute("data-session-id");
  if (sessionId !== null) {
    return /^[A-Za-z0-9_-]{1,128}$/.test(sessionId)
      ? `request-session ${sessionId}`
      : "request-session";
  }
  const aria = element.getAttribute("aria-label");
  if (aria?.trim()) return collapse(aria);
  const text = collapse(element.textContent ?? "");
  if (text) return text;
  const name = element.getAttribute("name") ?? element.getAttribute("id");
  if (name?.trim()) return collapse(name);
  return element.tagName.toLowerCase();
}

export function describeClick(target: EventTarget | null): string | undefined {
  if (!(target instanceof Element)) return undefined;
  const control = target.closest(CLICKABLE);
  if (!control) return undefined;
  return `click ${actionLabel(control)}`;
}

export function describeChange(target: EventTarget | null): string | undefined {
  if (!(target instanceof Element)) return undefined;
  const label = actionLabel(target);
  if (isSecretControl(target)) return `change ${label}`;
  if (target instanceof HTMLSelectElement) {
    const selected = collapse(target.selectedOptions[0]?.text ?? "");
    return selected ? `change ${label} ${selected}` : `change ${label}`;
  }
  if (
    target instanceof HTMLInputElement &&
    (target.type === "checkbox" || target.type === "radio")
  ) {
    return `change ${label} ${target.checked ? "on" : "off"}`;
  }
  return `change ${label}`;
}

export function installAppActionLogs(target: Document = document): () => void {
  const onClick = (event: Event) => {
    const message = describeClick(event.target);
    if (message) appLog.debug("ui.action", message);
  };
  const onChange = (event: Event) => {
    const message = describeChange(event.target);
    if (message) appLog.debug("ui.action", message);
  };
  target.addEventListener("click", onClick, true);
  target.addEventListener("change", onChange, true);
  return () => {
    target.removeEventListener("click", onClick, true);
    target.removeEventListener("change", onChange, true);
  };
}
