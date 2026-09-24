import { useEffect, useRef, useState } from "react";

import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormMessage } from "@/components/FormMessage";
import { HelpDisclosure } from "@/components/HelpDisclosure";
import { Panel, PanelFooter, PanelHeader } from "@/components/Panel";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

import {
  getCodexReviewModelStatus,
  listServices,
  setCodexReviewModel,
} from "./bridge";
import {
  reviewModelCandidates,
  type CodexReviewModelStatus,
} from "./codex-review-model";
import { i18n, useT } from "./i18n";
import { notify } from "./notify";

const FOLLOW_SESSION = "__follow_session__";

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : i18n.t("agentDebug.failed");
}

function selectionOf(status: CodexReviewModelStatus): string {
  return status.state.kind === "override" ? status.state.model : FOLLOW_SESSION;
}

export function CodexReviewModelPanel() {
  const t = useT();
  const [status, setStatus] = useState<CodexReviewModelStatus | null>(null);
  const [models, setModels] = useState<string[]>([]);
  const [selected, setSelected] = useState(FOLLOW_SESSION);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    void getCodexReviewModelStatus()
      .then((next) => {
        if (!mounted.current) return;
        setStatus(next);
        setSelected(selectionOf(next));
      })
      .catch((next: unknown) => {
        if (mounted.current) setError(messageOf(next));
      });
    void listServices()
      .then((services) => {
        if (!mounted.current) return;
        setModels(reviewModelCandidates(services.items));
      })
      .catch((next: unknown) => {
        if (mounted.current) setError(messageOf(next));
      });
    return () => {
      mounted.current = false;
    };
  }, []);

  const save = async (): Promise<void> => {
    setConfirming(false);
    setSaving(true);
    setError(null);
    try {
      const next = await setCodexReviewModel(
        selected === FOLLOW_SESSION ? null : selected,
      );
      if (!mounted.current) return;
      setStatus(next);
      setSelected(selectionOf(next));
      notify.success(i18n.t("agentDebug.reviewModel.notifySaved"));
    } catch (next) {
      if (mounted.current) setError(messageOf(next));
    } finally {
      if (mounted.current) setSaving(false);
    }
  };

  if (!status?.detected && !error) return null;

  const current = status ? selectionOf(status) : FOLLOW_SESSION;
  const orphan =
    status?.state.kind === "override" && !models.includes(status.state.model)
      ? status.state.model
      : null;
  const needsChoice =
    status?.state.kind === "codex_auto_review" ||
    status?.state.kind === "bundled_catalog";
  const missingAutoReviewService = !models.includes("codex-auto-review");
  const showAutoReviewWarning = needsChoice && missingAutoReviewService;
  const currentLabel = !status
    ? null
    : status.state.kind === "override"
      ? t("agentDebug.reviewModel.currentOverride", {
          model: status.state.model,
        })
      : status.state.kind === "session_model"
        ? t("agentDebug.reviewModel.currentSession")
        : t("agentDebug.reviewModel.currentAutoReview");
  const selectedLabel =
    selected === FOLLOW_SESSION
      ? t("agentDebug.reviewModel.followSession")
      : selected;
  const unchanged = !needsChoice && selected === current;

  return (
    <Panel aria-labelledby="codex-review-model-heading">
      <PanelHeader>
        <h2 className="text-sm font-semibold" id="codex-review-model-heading">
          {t("agentDebug.reviewModel.title")}
        </h2>
        <p className="mt-1 text-xs text-muted-foreground">
          {t("agentDebug.reviewModel.description")}
          {currentLabel ? ` ${currentLabel}` : null}
        </p>
      </PanelHeader>

      <div className="grid gap-3 px-4 py-3">
        {error ? (
          <FormMessage className="[overflow-wrap:anywhere]" tone="error">
            {error}
          </FormMessage>
        ) : showAutoReviewWarning ? (
          <FormMessage tone="warning">
            {t("agentDebug.reviewModel.autoReviewWarning")}
          </FormMessage>
        ) : null}
        <Field
          hint={
            models.length
              ? t("agentDebug.reviewModel.hint")
              : t("agentDebug.reviewModel.noServices")
          }
          label={t("agentDebug.reviewModel.field")}
        >
          <Select
            disabled={!status || saving}
            onValueChange={setSelected}
            value={selected}
          >
            <SelectTrigger
              aria-label={t("agentDebug.reviewModel.field")}
              className="w-full"
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={FOLLOW_SESSION}>
                {t("agentDebug.reviewModel.followSession")}
              </SelectItem>
              {orphan ? (
                <SelectItem value={orphan}>
                  {t("agentDebug.reviewModel.notInServices", {
                    model: orphan,
                  })}
                </SelectItem>
              ) : null}
              {models.map((model) => (
                <SelectItem key={model} value={model}>
                  {model}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </Field>
      </div>

      <PanelFooter
        className="justify-end"
        actions={
          <Button
            disabled={!status || saving || unchanged}
            onClick={() => setConfirming(true)}
            size="sm"
            type="button"
          >
            {saving
              ? t("agentDebug.reviewModel.saving")
              : t("agentDebug.reviewModel.save")}
          </Button>
        }
      />

      <ConfirmDialog
        confirmLabel={t("agentDebug.reviewModel.save")}
        description={
          <div className="grid gap-3">
            <p>
              {t("agentDebug.reviewModel.confirmBody", {
                model: selectedLabel,
              })}
            </p>
            {status && !status.catalog_configured ? (
              <p>{t("agentDebug.reviewModel.confirmPinsCatalog")}</p>
            ) : null}
            {status?.preview_paths.length ? (
              <HelpDisclosure title={t("agentDebug.pathsTitle")}>
                <ul className="list-disc pl-4 text-left font-mono text-xs text-text-secondary">
                  {status.preview_paths.map((path) => (
                    <li className="[overflow-wrap:anywhere]" key={path}>
                      {path}
                    </li>
                  ))}
                </ul>
              </HelpDisclosure>
            ) : null}
          </div>
        }
        disabled={saving}
        onCancel={() => setConfirming(false)}
        onConfirm={() => void save()}
        open={confirming}
        title={t("agentDebug.reviewModel.confirmTitle")}
      />
    </Panel>
  );
}
