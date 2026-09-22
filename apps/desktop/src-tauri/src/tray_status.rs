//! Menu-bar lamp. The color is only as strong as the facts the host already has:
//! the in-memory gateway snapshot, and a full service list read from core.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use tauri::{
    menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem},
    Emitter, Manager,
};
use tokio::sync::mpsc::{self, Sender};

use crate::app_log;
use crate::i18n::{self, Locale};
use crate::preferences::PreferencesStore;
use crate::sidecar::{CoreManager, CorePhase, CoreSnapshot};

pub const FETCH_INTERVAL: Duration = Duration::from_secs(30);
const UNREADABLE_AFTER: Duration = Duration::from_secs(120);
const UNREADABLE_STREAK: u32 = 3;
const NOTICE_EVENT: &str = "tray-status-notice";
const LAMP_MONO: u8 = 0;
const LAMP_GREEN: u8 = 1;
const LAMP_YELLOW: u8 = 2;
const LAMP_RED: u8 = 3;
static APPLIED_LAMP: AtomicU8 = AtomicU8::new(LAMP_MONO);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lamp {
    Mono,
    Green,
    Yellow,
    Red,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeLevel {
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeTarget {
    Overview,
    Services,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayNotice {
    pub key: String,
    pub level: NoticeLevel,
    pub title: String,
    pub description: String,
    pub target: NoticeTarget,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayFace {
    pub lamp: Lamp,
    pub tooltip: String,
    pub headline: String,
    pub gateway: String,
    pub api: String,
    pub notice: Option<TrayNotice>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewaySample {
    pub phase: CorePhase,
    pub pid: Option<u32>,
    pub inference_url: Option<String>,
    pub port_fallback: Option<PortFallback>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortFallback {
    pub requested: u16,
    pub active: u16,
}

impl GatewaySample {
    pub fn from_snapshot(snapshot: &CoreSnapshot) -> Self {
        Self {
            phase: snapshot.phase,
            pid: snapshot.pid,
            inference_url: snapshot
                .ready
                .as_ref()
                .map(|ready| ready.inference_url.clone()),
            port_fallback: snapshot
                .inference_port_fallback
                .as_ref()
                .map(|fallback| PortFallback {
                    requested: fallback.requested_port,
                    active: fallback.active_port,
                }),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServiceDigest {
    pub total: u32,
    pub disabled: u32,
    pub enabled_http: u32,
    pub connected: u32,
    pub incomplete: u32,
    pub failed_ids: Vec<String>,
}

impl ServiceDigest {
    fn enabled(&self) -> u32 {
        self.total.saturating_sub(self.disabled)
    }

    fn has_path(&self) -> bool {
        self.enabled_http > 0 || self.connected > 0
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpstreamMemory {
    digest: Option<ServiceDigest>,
    failed: bool,
    failure_streak: u32,
    failure_started: Option<Instant>,
    unreadable: bool,
}

impl UpstreamMemory {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn note_success(&mut self, digest: ServiceDigest) {
        *self = Self {
            digest: Some(digest),
            ..Self::default()
        };
    }

    /// Returns whether this failure is the one that crosses the unreadable threshold.
    pub fn note_failure(&mut self, now: Instant) -> bool {
        if self.failure_started.is_none() {
            self.failure_started = Some(now);
        }
        self.failed = true;
        self.failure_streak = self.failure_streak.saturating_add(1);
        let elapsed = now.saturating_duration_since(self.failure_started.unwrap_or(now));
        let was = self.unreadable;
        if self.failure_streak >= UNREADABLE_STREAK || elapsed >= UNREADABLE_AFTER {
            self.unreadable = true;
        }
        self.unreadable && !was
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EpochChange {
    Same,
    Entered,
    Left,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadyEpoch {
    pub epoch: u64,
    was_ready: bool,
    pid: Option<u32>,
}

impl ReadyEpoch {
    pub fn observe(&mut self, phase: CorePhase, pid: Option<u32>) -> EpochChange {
        if phase == CorePhase::Ready {
            let entered = !self.was_ready || self.pid != pid;
            self.was_ready = true;
            self.pid = pid;
            if entered {
                self.epoch = self.epoch.saturating_add(1);
                EpochChange::Entered
            } else {
                EpochChange::Same
            }
        } else if self.was_ready {
            self.was_ready = false;
            self.pid = None;
            EpochChange::Left
        } else {
            EpochChange::Same
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NoticeMemory {
    key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoticeStep {
    Hold,
    Show(TrayNotice),
    Dismiss,
}

pub fn step_notice(memory: &mut NoticeMemory, notice: Option<&TrayNotice>) -> NoticeStep {
    match notice {
        None => {
            if memory.key.take().is_some() {
                NoticeStep::Dismiss
            } else {
                NoticeStep::Hold
            }
        }
        Some(notice) => {
            if memory.key.as_deref() == Some(notice.key.as_str()) {
                NoticeStep::Hold
            } else {
                memory.key = Some(notice.key.clone());
                NoticeStep::Show(notice.clone())
            }
        }
    }
}

pub fn fetch_due(last_fetch: Option<Instant>, now: Instant) -> bool {
    match last_fetch {
        None => true,
        Some(at) => now.saturating_duration_since(at) >= FETCH_INTERVAL,
    }
}

pub fn parse_service_list(value: &Value) -> Result<ServiceDigest, &'static str> {
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .ok_or("service list omitted items")?;
    let mut digest = ServiceDigest::default();
    for item in items {
        let item = item.as_object().ok_or("service was not an object")?;
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or("service id")?;
        let enabled = item
            .get("enabled")
            .and_then(Value::as_bool)
            .ok_or("service enabled")?;
        let kind = item
            .get("kind")
            .and_then(Value::as_str)
            .ok_or("service kind")?;
        digest.total = digest.total.saturating_add(1);
        if !enabled {
            if classify_kind(kind).is_none() {
                return Err("unknown service kind");
            }
            if classify_kind(kind) == Some(true) {
                let _ = subscription_status(item)?;
            }
            digest.disabled = digest.disabled.saturating_add(1);
            continue;
        }
        match classify_kind(kind) {
            Some(false) => digest.enabled_http = digest.enabled_http.saturating_add(1),
            Some(true) => match subscription_status(item)? {
                "connected" => digest.connected = digest.connected.saturating_add(1),
                "authorizing" | "disconnected" => {
                    digest.incomplete = digest.incomplete.saturating_add(1)
                }
                "needs_reauth" | "error" => digest.failed_ids.push(id.to_string()),
                _ => return Err("unknown subscription status"),
            },
            None => return Err("unknown service kind"),
        }
    }
    digest.failed_ids.sort();
    Ok(digest)
}

fn classify_kind(kind: &str) -> Option<bool> {
    match kind {
        "codex_subscription" | "claude_subscription" | "grok_subscription" => Some(true),
        "opencode_go" | "opencode_zen" | "kimi_coding" | "glm_coding" | "minimax_coding"
        | "newapi" | "openai" | "anthropic" | "gemini" | "openai_compatible" | "deepseek"
        | "qwen" | "moonshot" | "glm" | "minimax" | "doubao" | "xai" | "custom" => Some(false),
        _ => None,
    }
}

fn subscription_status(
    item: &serde_json::Map<String, Value>,
) -> Result<&'static str, &'static str> {
    let status = item
        .get("subscription")
        .and_then(Value::as_object)
        .and_then(|subscription| subscription.get("status"))
        .and_then(Value::as_str)
        .ok_or("subscription status")?;
    match status {
        "connected" => Ok("connected"),
        "authorizing" => Ok("authorizing"),
        "disconnected" => Ok("disconnected"),
        "needs_reauth" => Ok("needs_reauth"),
        "error" => Ok("error"),
        _ => Err("unknown subscription status"),
    }
}

pub fn present(locale: Locale, gateway: &GatewaySample, upstream: &UpstreamMemory) -> TrayFace {
    let phase_label = i18n::t(locale, phase_key(gateway.phase), &[]);
    let gateway_line = gateway_line(locale, gateway, &phase_label);
    let (lamp, headline, api) = if gateway.phase != CorePhase::Ready {
        (
            phase_lamp(gateway.phase),
            phase_label.clone(),
            i18n::t(locale, "host.tray.gatewayUnchecked", &[]),
        )
    } else {
        ready_face(locale, gateway, upstream, &phase_label)
    };
    let notice = notice_for(gateway, upstream, &headline, &gateway_line, &api);
    TrayFace {
        lamp,
        tooltip: i18n::t(locale, "host.tray.tooltip", &[("status", &headline)]),
        headline,
        gateway: gateway_line,
        api: i18n::t(locale, "host.tray.api", &[("status", &api)]),
        notice,
    }
}

fn ready_face(
    locale: Locale,
    gateway: &GatewaySample,
    upstream: &UpstreamMemory,
    phase_label: &str,
) -> (Lamp, String, String) {
    let Some(digest) = upstream.digest.as_ref() else {
        let api = if upstream.failed {
            i18n::t(locale, "host.tray.apiUnreadable", &[])
        } else {
            i18n::t(locale, "host.tray.checking", &[])
        };
        return (Lamp::Yellow, api.clone(), api);
    };
    let lamp = catalog_lamp(digest, gateway.port_fallback.is_some());
    let headline = if !digest.failed_ids.is_empty() {
        if digest.has_path() {
            i18n::t(locale, "host.tray.degradedHeadline", &[])
        } else {
            i18n::t(locale, "host.tray.blockedHeadline", &[])
        }
    } else if digest.incomplete > 0 {
        i18n::t(locale, "host.tray.incompleteHeadline", &[])
    } else if gateway.port_fallback.is_some() {
        i18n::t(locale, "host.tray.portHeadline", &[])
    } else if upstream.unreadable {
        i18n::t(locale, "host.tray.apiUnreadable", &[])
    } else {
        phase_label.to_string()
    };
    let api = if upstream.unreadable {
        i18n::t(locale, "host.tray.apiUnreadable", &[])
    } else {
        catalog_api(locale, digest)
    };
    (lamp, headline, api)
}

fn catalog_lamp(digest: &ServiceDigest, port_fallback: bool) -> Lamp {
    if !digest.failed_ids.is_empty() && !digest.has_path() {
        Lamp::Red
    } else if !digest.failed_ids.is_empty() || digest.incomplete > 0 || port_fallback {
        Lamp::Yellow
    } else {
        Lamp::Green
    }
}

fn catalog_api(locale: Locale, digest: &ServiceDigest) -> String {
    if digest.total == 0 {
        return i18n::t(locale, "host.tray.noneConfigured", &[]);
    }
    if digest.enabled() == 0 {
        return i18n::t(locale, "host.tray.noneEnabled", &[]);
    }
    let mut parts = Vec::new();
    if digest.connected > 0 {
        parts.push(i18n::t(
            locale,
            "host.tray.connected",
            &[("count", &digest.connected.to_string())],
        ));
    }
    if digest.incomplete > 0 {
        parts.push(i18n::t(
            locale,
            "host.tray.incompleteCount",
            &[("count", &digest.incomplete.to_string())],
        ));
    }
    if !digest.failed_ids.is_empty() {
        parts.push(i18n::t(
            locale,
            "host.tray.failedCount",
            &[("count", &digest.failed_ids.len().to_string())],
        ));
    }
    if digest.enabled_http > 0 {
        parts.push(i18n::t(
            locale,
            "host.tray.httpConfigured",
            &[("count", &digest.enabled_http.to_string())],
        ));
    }
    parts.join(" · ")
}

fn notice_for(
    gateway: &GatewaySample,
    upstream: &UpstreamMemory,
    headline: &str,
    gateway_line: &str,
    api_line: &str,
) -> Option<TrayNotice> {
    let (key, level, description, target) = match gateway.phase {
        CorePhase::Error => (
            "gateway-error".to_string(),
            NoticeLevel::Error,
            gateway_line.to_string(),
            NoticeTarget::Overview,
        ),
        CorePhase::Exited => (
            "gateway-exited".to_string(),
            NoticeLevel::Error,
            gateway_line.to_string(),
            NoticeTarget::Overview,
        ),
        CorePhase::Ready => ready_notice(gateway, upstream, gateway_line, api_line)?,
        _ => return None,
    };
    Some(TrayNotice {
        key,
        level,
        title: headline.to_string(),
        description,
        target,
    })
}

fn ready_notice(
    gateway: &GatewaySample,
    upstream: &UpstreamMemory,
    gateway_line: &str,
    api_line: &str,
) -> Option<(String, NoticeLevel, String, NoticeTarget)> {
    if let Some(digest) = upstream.digest.as_ref() {
        if !digest.failed_ids.is_empty() {
            let blocked = !digest.has_path();
            return Some((
                format!(
                    "subs-{}-{}",
                    digest.failed_ids.join(","),
                    if blocked { "down" } else { "partial" }
                ),
                if blocked {
                    NoticeLevel::Error
                } else {
                    NoticeLevel::Warning
                },
                api_line.to_string(),
                NoticeTarget::Services,
            ));
        }
    }
    if upstream.unreadable {
        return Some((
            "api-unreadable".to_string(),
            NoticeLevel::Warning,
            api_line.to_string(),
            NoticeTarget::Overview,
        ));
    }
    gateway.port_fallback.map(|fallback| {
        (
            format!("port-{}-{}", fallback.requested, fallback.active),
            NoticeLevel::Warning,
            gateway_line.to_string(),
            NoticeTarget::Overview,
        )
    })
}

fn phase_lamp(phase: CorePhase) -> Lamp {
    match phase {
        CorePhase::Stopped => Lamp::Mono,
        CorePhase::Error | CorePhase::Exited => Lamp::Red,
        CorePhase::Spawning
        | CorePhase::WaitingForReady
        | CorePhase::Handshaking
        | CorePhase::Stopping
        | CorePhase::Ready => Lamp::Yellow,
    }
}

fn phase_key(phase: CorePhase) -> &'static str {
    match phase {
        CorePhase::Stopped => "core.phase.stopped",
        CorePhase::Spawning => "core.phase.spawning",
        CorePhase::WaitingForReady => "core.phase.waiting_for_ready",
        CorePhase::Handshaking => "core.phase.handshaking",
        CorePhase::Ready => "core.phase.ready",
        CorePhase::Stopping => "core.phase.stopping",
        CorePhase::Exited => "core.phase.exited",
        CorePhase::Error => "core.phase.error",
    }
}

fn gateway_line(locale: Locale, gateway: &GatewaySample, phase_label: &str) -> String {
    let detail = match (gateway.phase, gateway.port_fallback, &gateway.inference_url) {
        (CorePhase::Ready, Some(_), Some(address)) => format!(
            "{phase_label} · {}",
            i18n::t(locale, "host.tray.portChanged", &[("address", address)])
        ),
        _ => phase_label.to_string(),
    };
    i18n::t(locale, "host.tray.gateway", &[("detail", &detail)])
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AppliedFace {
    lamp: Lamp,
    tooltip: String,
    headline: String,
    gateway: String,
    api: String,
}

impl From<&TrayFace> for AppliedFace {
    fn from(face: &TrayFace) -> Self {
        Self {
            lamp: face.lamp,
            tooltip: face.tooltip.clone(),
            headline: face.headline.clone(),
            gateway: face.gateway.clone(),
            api: face.api.clone(),
        }
    }
}

pub enum TrayMsg {
    Refresh,
    WindowShown,
    FrontendReady,
    Services {
        id: u64,
        epoch: u64,
        result: Result<Value, String>,
    },
}

pub struct TrayControl {
    tx: Sender<TrayMsg>,
}

impl TrayControl {
    pub async fn frontend_ready(&self) -> Result<(), String> {
        self.tx
            .send(TrayMsg::FrontendReady)
            .await
            .map_err(|_| "tray status loop is unavailable".to_string())
    }
}

pub fn nudge(app: &tauri::AppHandle, message: TrayMsg) {
    if let Some(control) = app.try_state::<TrayControl>() {
        let _ = control.tx.try_send(message);
    }
}

pub fn lamp_overrides_icon() -> bool {
    matches!(APPLIED_LAMP.load(Ordering::Relaxed), LAMP_YELLOW | LAMP_RED)
}

fn store_lamp(lamp: Lamp) {
    APPLIED_LAMP.store(
        match lamp {
            Lamp::Mono => LAMP_MONO,
            Lamp::Green => LAMP_GREEN,
            Lamp::Yellow => LAMP_YELLOW,
            Lamp::Red => LAMP_RED,
        },
        Ordering::Relaxed,
    );
}

pub fn spawn(app: tauri::AppHandle, manager: std::sync::Arc<CoreManager>) -> TrayControl {
    let (tx, mut rx) = mpsc::channel(32);
    let loop_tx = tx.clone();
    tauri::async_runtime::spawn(async move {
        let mut runtime = TrayRuntime::default();
        runtime.tick(&app, &manager, &loop_tx).await;
        loop {
            match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
                Ok(Some(message)) => runtime.handle(message),
                Ok(None) => break,
                Err(_) => {}
            }
            runtime.tick(&app, &manager, &loop_tx).await;
        }
    });
    TrayControl { tx }
}

#[derive(Default)]
struct TrayRuntime {
    epoch: ReadyEpoch,
    upstream: UpstreamMemory,
    inflight: Option<u64>,
    next_request: u64,
    last_fetch: Option<Instant>,
    force: bool,
    applied: Option<AppliedFace>,
    notices: NoticeMemory,
    pending: Option<TrayNotice>,
    delivered: bool,
    frontend_ready: bool,
}

impl TrayRuntime {
    fn handle(&mut self, message: TrayMsg) {
        match message {
            TrayMsg::Refresh => self.force = true,
            TrayMsg::WindowShown => {}
            TrayMsg::FrontendReady => {
                self.frontend_ready = true;
                self.delivered = false;
            }
            TrayMsg::Services { id, epoch, result } => {
                if self.inflight != Some(id) || self.epoch.epoch != epoch {
                    return;
                }
                self.inflight = None;
                let now = Instant::now();
                match result {
                    Ok(value) => match parse_service_list(&value) {
                        Ok(digest) => self.upstream.note_success(digest),
                        Err(error) => {
                            if self.upstream.note_failure(now) {
                                app_log::warning!(
                                    "shell.tray",
                                    "API status could not be read: {error}"
                                );
                            }
                        }
                    },
                    Err(error) => {
                        if self.upstream.note_failure(now) {
                            app_log::warning!(
                                "shell.tray",
                                "API status could not be read: {error}"
                            );
                        }
                    }
                }
            }
        }
    }

    async fn tick(
        &mut self,
        app: &tauri::AppHandle,
        manager: &std::sync::Arc<CoreManager>,
        tx: &Sender<TrayMsg>,
    ) {
        let sample = GatewaySample::from_snapshot(&manager.snapshot());
        match self.epoch.observe(sample.phase, sample.pid) {
            EpochChange::Entered | EpochChange::Left => {
                self.upstream.reset();
                self.inflight = None;
                self.last_fetch = None;
            }
            EpochChange::Same => {}
        }
        if sample.phase == CorePhase::Ready
            && self.inflight.is_none()
            && (self.force || fetch_due(self.last_fetch, Instant::now()))
        {
            self.force = false;
            self.next_request = self.next_request.saturating_add(1);
            let id = self.next_request;
            let epoch = self.epoch.epoch;
            self.inflight = Some(id);
            self.last_fetch = Some(Instant::now());
            let tx = tx.clone();
            let manager = std::sync::Arc::clone(manager);
            tauri::async_runtime::spawn(async move {
                let result = manager.list_services().await;
                let _ = tx.send(TrayMsg::Services { id, epoch, result }).await;
            });
        }
        let locale = app
            .try_state::<std::sync::Arc<PreferencesStore>>()
            .map(|store| store.snapshot().values.locale)
            .unwrap_or_default();
        let face = present(locale, &sample, &self.upstream);
        let applied = AppliedFace::from(&face);
        if self.applied.as_ref() != Some(&applied) && apply_tray(app, &face).is_ok() {
            self.applied = Some(applied);
        }
        match step_notice(&mut self.notices, face.notice.as_ref()) {
            NoticeStep::Hold => {}
            NoticeStep::Dismiss => {
                self.pending = None;
                if self.delivered {
                    emit_notice(app, locale, "dismiss", None);
                }
                self.delivered = false;
            }
            NoticeStep::Show(notice) => {
                self.pending = Some(notice);
                self.delivered = false;
            }
        }
        self.deliver_if_visible(app, locale);
    }

    fn notice_to_deliver(&self) -> Option<&TrayNotice> {
        if !self.frontend_ready || self.delivered {
            return None;
        }
        self.pending.as_ref()
    }

    fn deliver_if_visible(&mut self, app: &tauri::AppHandle, locale: Locale) {
        let Some(notice) = self.notice_to_deliver().cloned() else {
            return;
        };
        let visible = app
            .get_webview_window("main")
            .and_then(|window| window.is_visible().ok())
            .unwrap_or(false);
        if !visible {
            return;
        }
        if emit_notice(app, locale, "show", Some(&notice)) {
            self.delivered = true;
        }
    }
}

fn emit_notice(
    app: &tauri::AppHandle,
    locale: Locale,
    action: &'static str,
    notice: Option<&TrayNotice>,
) -> bool {
    let event = TrayNoticeEvent {
        action,
        key: notice.map(|notice| notice.key.clone()).unwrap_or_default(),
        level: match notice.map(|notice| notice.level) {
            Some(NoticeLevel::Error) => "error",
            _ => "warning",
        },
        title: notice
            .map(|notice| notice.title.clone())
            .unwrap_or_default(),
        description: notice
            .map(|notice| notice.description.clone())
            .unwrap_or_default(),
        target: match notice.map(|notice| notice.target) {
            Some(NoticeTarget::Services) => "services",
            _ => "overview",
        },
        view_label: i18n::t(locale, "host.tray.view", &[]),
    };
    match app.emit(NOTICE_EVENT, &event) {
        Ok(()) => true,
        Err(error) => {
            app_log::error!("shell.tray", "unable to publish tray notice: {error}");
            false
        }
    }
}

#[derive(Clone, Serialize)]
struct TrayNoticeEvent {
    action: &'static str,
    key: String,
    level: &'static str,
    title: String,
    description: String,
    target: &'static str,
    view_label: String,
}

fn apply_tray(app: &tauri::AppHandle, face: &TrayFace) -> Result<(), String> {
    let Some(tray) = app.tray_by_id("main") else {
        return Err("tray is not installed".to_string());
    };
    // The popover tray owns clicks, menus and the ready/idle/watched glyphs.
    // Subscription lamps only overlay when something needs attention.
    let override_icon = matches!(face.lamp, Lamp::Yellow | Lamp::Red);
    if override_icon {
        apply_icon(app, &tray, face.lamp)?;
        tray.set_tooltip(Some(face.tooltip.as_str()))
            .map_err(|error| error.to_string())?;
        store_lamp(face.lamp);
    } else {
        let was_override = lamp_overrides_icon();
        store_lamp(face.lamp);
        if was_override {
            crate::tray::restore_native_icon(app);
        }
    }
    Ok(())
}

fn apply_icon(
    app: &tauri::AppHandle,
    tray: &tauri::tray::TrayIcon,
    lamp: Lamp,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let _ = app;
        let (icon, template) = match lamp {
            Lamp::Green => (tauri::include_image!("icons/tray/status-green.png"), false),
            Lamp::Yellow => (tauri::include_image!("icons/tray/status-yellow.png"), false),
            Lamp::Red => (tauri::include_image!("icons/tray/status-red.png"), false),
            Lamp::Mono => (tauri::include_image!("icons/tray/mac-idle/36x36.png"), true),
        };
        tray.set_icon_with_as_template(Some(icon), template)
            .map_err(|error| error.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        match lamp {
            Lamp::Mono => {
                if let Some(icon) = app.default_window_icon() {
                    tray.set_icon(Some(icon.clone()))
                        .map_err(|error| error.to_string())?;
                }
                Ok(())
            }
            Lamp::Green => tray
                .set_icon(Some(tauri::include_image!("icons/tray/status-green.png")))
                .map_err(|error| error.to_string()),
            Lamp::Yellow => tray
                .set_icon(Some(tauri::include_image!("icons/tray/status-yellow.png")))
                .map_err(|error| error.to_string()),
            Lamp::Red => tray
                .set_icon(Some(tauri::include_image!("icons/tray/status-red.png")))
                .map_err(|error| error.to_string()),
        }
    }
}

#[allow(dead_code)]
fn tray_menu(app: &tauri::AppHandle, face: &TrayFace) -> Result<Menu<tauri::Wry>, String> {
    let headline = MenuItem::with_id(app, "tray-status", &face.headline, false, None::<&str>)
        .map_err(|error| error.to_string())?;
    let gateway = MenuItem::with_id(app, "tray-gateway", &face.gateway, false, None::<&str>)
        .map_err(|error| error.to_string())?;
    let api = MenuItem::with_id(app, "tray-api", &face.api, false, None::<&str>)
        .map_err(|error| error.to_string())?;
    let separator = PredefinedMenuItem::separator(app).map_err(|error| error.to_string())?;
    let locale = app
        .try_state::<std::sync::Arc<PreferencesStore>>()
        .map(|store| store.snapshot().values.locale)
        .unwrap_or_default();
    let show = MenuItem::with_id(
        app,
        "show",
        i18n::t(locale, "host.tray.show", &[]),
        true,
        None::<&str>,
    )
    .map_err(|error| error.to_string())?;
    let quit = MenuItem::with_id(
        app,
        "quit",
        i18n::t(locale, "host.tray.quit", &[]),
        true,
        None::<&str>,
    )
    .map_err(|error| error.to_string())?;
    #[cfg(debug_assertions)]
    let reload = MenuItem::with_id(
        app,
        "reload",
        i18n::t(locale, "host.tray.reload", &[]),
        true,
        None::<&str>,
    )
    .map_err(|error| error.to_string())?;
    #[cfg(debug_assertions)]
    let items: [&dyn IsMenuItem<tauri::Wry>; 7] =
        [&headline, &gateway, &api, &separator, &show, &reload, &quit];
    #[cfg(not(debug_assertions))]
    let items: [&dyn IsMenuItem<tauri::Wry>; 6] =
        [&headline, &gateway, &api, &separator, &show, &quit];
    let menu = Menu::with_items(app, &items).map_err(|error| error.to_string())?;
    Ok(menu)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_notice_waits_for_frontend_and_replays_after_reload() {
        let notice = present(
            Locale::En,
            &phase(CorePhase::Error),
            &UpstreamMemory::default(),
        )
        .notice
        .unwrap();
        let mut runtime = TrayRuntime {
            pending: Some(notice.clone()),
            ..TrayRuntime::default()
        };
        runtime.handle(TrayMsg::WindowShown);
        assert!(runtime.notice_to_deliver().is_none());
        runtime.handle(TrayMsg::FrontendReady);
        assert_eq!(runtime.notice_to_deliver(), Some(&notice));
        runtime.delivered = true;
        assert!(runtime.notice_to_deliver().is_none());
        runtime.handle(TrayMsg::WindowShown);
        assert!(runtime.notice_to_deliver().is_none());
        runtime.handle(TrayMsg::FrontendReady);
        assert_eq!(runtime.notice_to_deliver(), Some(&notice));
        runtime.pending = None;
        assert!(runtime.notice_to_deliver().is_none());
    }

    fn ready(fallback: Option<PortFallback>) -> GatewaySample {
        GatewaySample {
            phase: CorePhase::Ready,
            pid: Some(7),
            inference_url: Some("http://127.0.0.1:43210".to_string()),
            port_fallback: fallback,
        }
    }

    fn phase(phase: CorePhase) -> GatewaySample {
        GatewaySample {
            phase,
            pid: None,
            inference_url: None,
            port_fallback: None,
        }
    }

    fn service(kind: &str, enabled: bool, status: Option<&str>) -> Value {
        let mut item = serde_json::json!({
            "id": format!("svc_{kind}_{enabled}"),
            "kind": kind,
            "enabled": enabled,
        });
        if let Some(status) = status {
            item["subscription"] = serde_json::json!({ "status": status });
        }
        item
    }

    fn digest_of(items: Vec<Value>) -> ServiceDigest {
        parse_service_list(&serde_json::json!({ "items": items })).expect("list")
    }

    fn with_digest(digest: ServiceDigest) -> UpstreamMemory {
        let mut memory = UpstreamMemory::default();
        memory.note_success(digest);
        memory
    }

    #[test]
    fn lamp_follows_gateway_phase_and_ignores_a_previous_catalog() {
        let red = with_digest(digest_of(vec![service(
            "codex_subscription",
            true,
            Some("needs_reauth"),
        )]));
        for (phase_name, lamp) in [
            (CorePhase::Stopped, Lamp::Mono),
            (CorePhase::Spawning, Lamp::Yellow),
            (CorePhase::WaitingForReady, Lamp::Yellow),
            (CorePhase::Handshaking, Lamp::Yellow),
            (CorePhase::Stopping, Lamp::Yellow),
            (CorePhase::Error, Lamp::Red),
            (CorePhase::Exited, Lamp::Red),
        ] {
            let face = present(Locale::ZhCN, &phase(phase_name), &red);
            assert_eq!(face.lamp, lamp, "{phase_name:?}");
            assert!(face.api.contains("未检查"), "{phase_name:?}");
        }
        assert!(present(Locale::ZhCN, &phase(CorePhase::Error), &red)
            .notice
            .unwrap()
            .key
            .starts_with("gateway-error"));
        assert_eq!(
            present(
                Locale::ZhCN,
                &phase(CorePhase::Exited),
                &UpstreamMemory::default()
            )
            .notice
            .unwrap()
            .key,
            "gateway-exited"
        );
        assert!(present(Locale::ZhCN, &phase(CorePhase::Spawning), &red)
            .notice
            .is_none());
    }

    #[test]
    fn ready_without_a_list_stays_yellow_until_the_unreadable_threshold() {
        let mut upstream = UpstreamMemory::default();
        let face = present(Locale::ZhCN, &ready(None), &upstream);
        assert_eq!(face.lamp, Lamp::Yellow);
        assert!(face.headline.contains("正在核对"));
        assert!(face.notice.is_none());

        assert!(!upstream.note_failure(Instant::now()));
        let face = present(Locale::ZhCN, &ready(None), &upstream);
        assert_eq!(face.lamp, Lamp::Yellow);
        assert!(face.api.contains("无法读取"));
        assert!(face.notice.is_none());

        let start = Instant::now();
        upstream.note_failure(start);
        assert!(upstream.note_failure(start));
        let face = present(Locale::En, &ready(None), &upstream);
        assert_eq!(face.notice.unwrap().key, "api-unreadable");
        assert_eq!(face.lamp, Lamp::Yellow);
    }

    #[test]
    fn a_later_read_failure_keeps_the_last_color_until_the_list_is_unreadable() {
        let mut upstream = with_digest(ServiceDigest::default());
        let start = Instant::now();
        assert!(!upstream.note_failure(start));
        let face = present(Locale::ZhCN, &ready(None), &upstream);
        assert_eq!(face.lamp, Lamp::Green);
        assert!(face.api.contains("未配置"));
        assert!(face.notice.is_none());

        assert!(upstream.note_failure(start + UNREADABLE_AFTER));
        let face = present(Locale::ZhCN, &ready(None), &upstream);
        assert_eq!(face.lamp, Lamp::Green);
        assert!(face.headline.contains("无法读取"));
        assert_eq!(face.notice.unwrap().level, NoticeLevel::Warning);
    }

    #[test]
    fn catalog_color_matches_what_the_list_can_prove() {
        let cases = [
            (Vec::new(), Lamp::Green, false),
            (vec![service("openai", false, None)], Lamp::Green, false),
            (vec![service("openai", true, None)], Lamp::Green, false),
            (
                vec![service("codex_subscription", true, Some("connected"))],
                Lamp::Green,
                false,
            ),
            (
                vec![service("claude_subscription", true, Some("disconnected"))],
                Lamp::Yellow,
                false,
            ),
            (
                vec![service("grok_subscription", true, Some("authorizing"))],
                Lamp::Yellow,
                false,
            ),
            (
                vec![service("codex_subscription", true, Some("needs_reauth"))],
                Lamp::Red,
                true,
            ),
            (
                vec![
                    service("codex_subscription", true, Some("error")),
                    service("openai", true, None),
                ],
                Lamp::Yellow,
                true,
            ),
            (
                vec![
                    service("codex_subscription", true, Some("needs_reauth")),
                    service("claude_subscription", true, Some("disconnected")),
                ],
                Lamp::Red,
                true,
            ),
            (
                vec![service("codex_subscription", false, Some("needs_reauth"))],
                Lamp::Green,
                false,
            ),
        ];
        for (items, lamp, notice) in cases {
            let face = present(Locale::En, &ready(None), &with_digest(digest_of(items)));
            assert_eq!(face.lamp, lamp);
            assert_eq!(face.notice.is_some(), notice);
        }
    }

    #[test]
    fn port_fallback_stays_yellow_and_does_not_outrank_a_dead_subscription() {
        let fallback = Some(PortFallback {
            requested: 8317,
            active: 43210,
        });
        let clear = present(
            Locale::ZhCN,
            &ready(fallback),
            &with_digest(ServiceDigest::default()),
        );
        assert_eq!(clear.lamp, Lamp::Yellow);
        assert!(clear.headline.contains("端口"));
        assert!(clear.gateway.contains("43210"));
        assert!(clear.notice.unwrap().key.starts_with("port-"));

        let blocked = present(
            Locale::ZhCN,
            &ready(fallback),
            &with_digest(digest_of(vec![service(
                "codex_subscription",
                true,
                Some("error"),
            )])),
        );
        assert_eq!(blocked.lamp, Lamp::Red);
        assert!(blocked.notice.unwrap().key.starts_with("subs-"));
    }

    #[test]
    fn failed_subscription_ids_are_sorted_into_the_notice_key() {
        let digest = parse_service_list(&serde_json::json!({
            "items": [
                {"id": "svc_b", "kind": "codex_subscription", "enabled": true, "subscription": {"status": "needs_reauth"}},
                {"id": "svc_a", "kind": "claude_subscription", "enabled": true, "subscription": {"status": "error"}}
            ]
        }))
        .unwrap();
        let notice = present(Locale::En, &ready(None), &with_digest(digest))
            .notice
            .unwrap();
        assert_eq!(notice.key, "subs-svc_a,svc_b-down");
        assert_eq!(notice.target, NoticeTarget::Services);
    }

    #[test]
    fn an_unreadable_field_fails_the_whole_list() {
        for value in [
            serde_json::json!({"items": [{"kind": "openai", "enabled": true}]}),
            serde_json::json!({"items": [{"id": "svc", "kind": "openai"}]}),
            serde_json::json!({"items": [{"id": "svc", "kind": "nope", "enabled": true}]}),
            serde_json::json!({"items": [{"id": "svc", "kind": "codex_subscription", "enabled": true, "subscription": {"status": "nope"}}]}),
        ] {
            assert!(parse_service_list(&value).is_err());
        }
    }

    #[test]
    fn epoch_changes_when_ready_starts_or_the_process_changes() {
        let mut epoch = ReadyEpoch::default();
        assert_eq!(epoch.observe(CorePhase::Stopped, None), EpochChange::Same);
        assert_eq!(
            epoch.observe(CorePhase::Ready, Some(1)),
            EpochChange::Entered
        );
        assert_eq!(epoch.observe(CorePhase::Ready, Some(1)), EpochChange::Same);
        assert_eq!(
            epoch.observe(CorePhase::Ready, Some(2)),
            EpochChange::Entered
        );
        assert_eq!(epoch.observe(CorePhase::Exited, None), EpochChange::Left);
        assert_eq!(epoch.observe(CorePhase::Error, None), EpochChange::Same);
        assert_eq!(epoch.epoch, 2);
    }

    #[test]
    fn the_same_notice_key_is_not_shown_twice() {
        let notice = TrayNotice {
            key: "gateway-error".to_string(),
            level: NoticeLevel::Error,
            title: "Needs attention".to_string(),
            description: "Gateway".to_string(),
            target: NoticeTarget::Overview,
        };
        let mut memory = NoticeMemory::default();
        assert!(matches!(
            step_notice(&mut memory, Some(&notice)),
            NoticeStep::Show(_)
        ));
        assert_eq!(step_notice(&mut memory, Some(&notice)), NoticeStep::Hold);
        assert_eq!(step_notice(&mut memory, None), NoticeStep::Dismiss);
        assert_eq!(step_notice(&mut memory, None), NoticeStep::Hold);
        assert!(matches!(
            step_notice(&mut memory, Some(&notice)),
            NoticeStep::Show(_)
        ));
    }

    #[test]
    fn tray_copy_exists_in_both_locales() {
        let face = present(Locale::En, &ready(None), &UpstreamMemory::default());
        assert!(face.tooltip.starts_with("AstrLink"));
        assert!(face.headline.contains("Checking"));
        let face = present(
            Locale::ZhCN,
            &phase(CorePhase::Stopped),
            &UpstreamMemory::default(),
        );
        assert!(face.headline.contains("已停止"));
        assert_ne!(i18n::t(Locale::En, "host.tray.view", &[]), "host.tray.view");
        assert_ne!(
            i18n::t(Locale::ZhCN, "host.tray.view", &[]),
            "host.tray.view"
        );
    }
}
