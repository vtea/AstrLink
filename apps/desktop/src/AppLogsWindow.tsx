import { useT } from "./i18n";
import { AppLogs } from "./AppLogs";

export function AppLogsWindow() {
  const t = useT();
  return (
    <main className="flex h-dvh min-h-0 flex-col overflow-hidden px-3 pt-[calc(var(--window-chrome-height)+8px)] pb-3">
      <h1 className="mb-2 shrink-0 text-sm font-semibold tracking-tight">
        {t("logs.title")}
      </h1>
      <AppLogs detached />
    </main>
  );
}
