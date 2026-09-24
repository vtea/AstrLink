use std::{
    fs, io,
    io::Read,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use serde_json::Value;

pub const CODEX_AUTO_REVIEW_MODEL: &str = "codex-auto-review";
const OVERRIDE_KEY: &str = "auto_review_model_override";
const CATALOG_KEY: &str = "model_catalog_json";
const DEFAULT_CATALOG_NAME: &str = "model-catalog.json";
const MAX_MODEL_CHARS: usize = 256;
const CATALOG_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReviewModelState {
    /// No catalog file: Codex uses its bundled catalog and requests codex-auto-review.
    BundledCatalog,
    CodexAutoReview,
    SessionModel,
    Override {
        model: String,
    },
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct CodexReviewModelStatus {
    pub detected: bool,
    pub config_path: String,
    pub catalog_path: String,
    pub catalog_configured: bool,
    pub catalog_exists: bool,
    pub session_model: Option<String>,
    pub state: ReviewModelState,
    pub preview_paths: Vec<String>,
}

pub fn status(home: &Path) -> Result<CodexReviewModelStatus, String> {
    let codex_dir = codex_dir(home);
    let config_path = codex_dir.join("config.toml");
    let config = read_config(&config_path)?;
    ensure_profile_is_unambiguous(&codex_dir, &config)?;
    let configured = configured_catalog_path(&config, &codex_dir, home);
    let catalog_path = configured
        .clone()
        .unwrap_or_else(|| codex_dir.join(DEFAULT_CATALOG_NAME));
    ensure_catalog_inside_codex_home(&codex_dir, &catalog_path)?;
    let session_model = session_model(&config);
    let catalog_exists = configured.is_some() && catalog_path.is_file();
    let state = if catalog_exists {
        let catalog = parse_catalog(&read_text(&catalog_path)?, &catalog_path)?;
        effective_state(&catalog, session_model.as_deref())
    } else {
        ReviewModelState::BundledCatalog
    };
    let mut preview_paths = vec![display_path(&catalog_path)?];
    if configured.is_none() {
        preview_paths.push(display_path(&config_path)?);
    }
    Ok(CodexReviewModelStatus {
        detected: codex_dir.is_dir(),
        config_path: display_path(&config_path)?,
        catalog_path: display_path(&catalog_path)?,
        catalog_configured: configured.is_some(),
        catalog_exists,
        session_model,
        state,
        preview_paths,
    })
}

/// `model: None` lets Codex fall back to the session model.
pub fn set_review_model(
    home: &Path,
    model: Option<&str>,
    bundled_catalog: impl FnOnce() -> Result<String, String>,
) -> Result<CodexReviewModelStatus, String> {
    let model = model.map(validate_model).transpose()?;
    let codex_dir = codex_dir(home);
    if !codex_dir.is_dir() {
        return Err(format!(
            "Codex is not detected ({} is missing)",
            codex_dir.display()
        ));
    }
    let config_path = codex_dir.join("config.toml");
    let config_raw = match fs::read_to_string(&config_path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("unable to read {}: {error}", config_path.display())),
    };
    let mut config = parse_config(&config_raw)?;
    ensure_profile_is_unambiguous(&codex_dir, &config)?;
    let configured = configured_catalog_path(&config, &codex_dir, home);
    let catalog_path = configured
        .clone()
        .unwrap_or_else(|| codex_dir.join(DEFAULT_CATALOG_NAME));
    ensure_catalog_inside_codex_home(&codex_dir, &catalog_path)?;
    let write_path = catalog_write_path(&catalog_path)?;

    // An unconfigured file at the default path is still user data. Edit it
    // instead of replacing it with `codex debug models --bundled`.
    let existing = if write_path.is_file() {
        Some(read_text(&write_path)?)
    } else {
        None
    };
    let source = match &existing {
        Some(raw) => raw.clone(),
        None => bundled_catalog()?,
    };
    let mut catalog = parse_catalog(&source, &write_path)?;
    apply_review_model(
        &mut catalog,
        session_model(&config).as_deref(),
        model.as_deref(),
    )?;
    let next = pretty_json(&catalog)?;

    let stamp = unix_now();
    let catalog_changed = existing.as_deref() != Some(next.as_str());
    let config_changed = configured.is_none();
    if catalog_changed {
        backup_if_present(&write_path, stamp)?;
    }
    if config_changed {
        backup_if_present(&config_path, stamp)?;
        config[CATALOG_KEY] = toml_edit::value(display_path(&catalog_path)?);
    }
    let next_config = config.to_string();
    let original_catalog = existing.clone();
    let original_config = if config_path.is_file() {
        Some(config_raw)
    } else {
        None
    };
    let commit = (|| {
        if catalog_changed {
            write_atomic(&write_path, next.as_bytes())?;
        }
        if config_changed {
            write_atomic(&config_path, next_config.as_bytes())?;
        }
        Ok::<(), String>(())
    })();
    if let Err(error) = commit {
        let rollback_error = rollback_files(
            &write_path,
            original_catalog.as_deref(),
            &config_path,
            original_config.as_deref(),
        );
        return Err(format_commit_error(error, rollback_error));
    }
    match status(home) {
        Ok(status) => Ok(status),
        Err(error) => {
            let rollback_error = rollback_files(
                &write_path,
                original_catalog.as_deref(),
                &config_path,
                original_config.as_deref(),
            );
            Err(format_commit_error(error, rollback_error))
        }
    }
}

/// Runs `codex debug models --bundled` from the first Codex executable found.
pub fn bundled_catalog_from_codex(home: &Path) -> Result<String, String> {
    let executable = codex_executable(home).ok_or_else(|| {
        "unable to locate the codex executable; install Codex or configure model_catalog_json first"
            .to_string()
    })?;
    let mut command = Command::new(&executable);
    command
        .args(["debug", "models", "--bundled"])
        .current_dir(home)
        .stdin(Stdio::null());
    let output = output_with_timeout(&mut command, CATALOG_COMMAND_TIMEOUT)
        .map_err(|error| format!("unable to run {}: {error}", executable.display()))?;
    if !output.status.success() {
        return Err(format!(
            "codex debug models failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|_| "codex debug models returned non-UTF-8 output".to_string())
}

fn codex_executable(home: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) { "codex.exe" } else { "codex" };
    let mut candidates = std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(name))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    candidates.extend([
        PathBuf::from("/opt/homebrew/bin").join(name),
        PathBuf::from("/usr/local/bin").join(name),
        home.join(".local/bin").join(name),
        PathBuf::from("/Applications/ChatGPT.app/Contents/Resources").join(name),
        PathBuf::from("/Applications/Codex.app/Contents/Resources").join(name),
    ]);
    candidates.into_iter().find(|path| path.is_file())
}

pub fn apply_review_model(
    catalog: &mut Value,
    session_model: Option<&str>,
    model: Option<&str>,
) -> Result<(), String> {
    let models = catalog_models_mut(catalog)?;
    // Codex reads the override from the session model's entry and otherwise
    // prefers codex-auto-review, which AstrLink services usually cannot serve.
    if model != Some(CODEX_AUTO_REVIEW_MODEL) {
        models.retain(|entry| entry_slug(entry) != Some(CODEX_AUTO_REVIEW_MODEL));
    }
    let Some(model) = model else {
        for entry in models.iter_mut() {
            if let Some(object) = entry.as_object_mut() {
                object.remove(OVERRIDE_KEY);
            }
        }
        return Ok(());
    };
    for slug in [Some(model), session_model].into_iter().flatten() {
        ensure_entry(models, slug)?;
    }
    for entry in models.iter_mut() {
        if let Some(object) = entry.as_object_mut() {
            object.insert(OVERRIDE_KEY.to_string(), Value::String(model.to_string()));
        }
    }
    Ok(())
}

fn ensure_entry(models: &mut Vec<Value>, slug: &str) -> Result<(), String> {
    if models.iter().any(|entry| entry_slug(entry) == Some(slug)) {
        return Ok(());
    }
    let Some(template) = models
        .iter()
        .find(|entry| is_complete_model_entry(entry))
        .cloned()
    else {
        return Err(format!(
            "Codex model catalog does not contain a complete model template for {slug}; AstrLink will not add an incomplete entry"
        ));
    };
    let mut object = serde_json::Map::new();
    object.insert("slug".to_string(), Value::String(slug.to_string()));
    object.insert("display_name".to_string(), Value::String(slug.to_string()));
    object.insert("visibility".to_string(), Value::String("hide".to_string()));
    if let Some(levels) = template.get("supported_reasoning_levels") {
        object.insert("supported_reasoning_levels".to_string(), levels.clone());
    }
    models.push(Value::Object(object));
    Ok(())
}

fn is_complete_model_entry(entry: &Value) -> bool {
    entry.get("slug").and_then(Value::as_str).is_some()
        && entry
            .get("supported_reasoning_levels")
            .and_then(Value::as_array)
            .is_some()
}

pub fn effective_state(catalog: &Value, session_model: Option<&str>) -> ReviewModelState {
    let models = catalog
        .get("models")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let session_entry = session_model.and_then(|session| {
        models
            .iter()
            .find(|entry| entry_slug(entry) == Some(session))
    });
    if let Some(model) = session_entry
        .and_then(|entry| entry.get(OVERRIDE_KEY))
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
    {
        return ReviewModelState::Override {
            model: model.to_string(),
        };
    }
    if session_model.is_none() {
        if let Some(model) = shared_override(models) {
            return ReviewModelState::Override { model };
        }
    }
    if models
        .iter()
        .any(|entry| entry_slug(entry) == Some(CODEX_AUTO_REVIEW_MODEL))
    {
        ReviewModelState::CodexAutoReview
    } else {
        ReviewModelState::SessionModel
    }
}

fn validate_model(model: &str) -> Result<String, String> {
    let model = model.trim();
    if model.is_empty() || model.chars().count() > MAX_MODEL_CHARS {
        return Err("review model must contain 1 to 256 characters".to_string());
    }
    Ok(model.to_string())
}

fn shared_override(models: &[Value]) -> Option<String> {
    let mut shared = None;
    for entry in models {
        if entry_slug(entry).is_none() {
            continue;
        }
        let model = entry
            .get(OVERRIDE_KEY)
            .and_then(Value::as_str)
            .filter(|model| !model.is_empty())?;
        if let Some(previous) = shared {
            if previous != model {
                return None;
            }
        } else {
            shared = Some(model);
        }
    }
    shared.map(str::to_string)
}

fn entry_slug(entry: &Value) -> Option<&str> {
    entry.get("slug").and_then(Value::as_str)
}

fn catalog_models_mut(catalog: &mut Value) -> Result<&mut Vec<Value>, String> {
    catalog
        .get_mut("models")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "Codex model catalog must contain a models array".to_string())
}

fn parse_catalog(raw: &str, path: &Path) -> Result<Value, String> {
    let value: Value = serde_json::from_str(raw).map_err(|error| {
        format!(
            "{} is invalid JSON; AstrLink will not overwrite it: {error}",
            path.display()
        )
    })?;
    if value.get("models").and_then(Value::as_array).is_none() {
        return Err(format!(
            "{} must contain a models array; AstrLink will not overwrite it",
            path.display()
        ));
    }
    Ok(value)
}

fn parse_config(raw: &str) -> Result<toml_edit::DocumentMut, String> {
    raw.parse::<toml_edit::DocumentMut>().map_err(|error| {
        format!("Codex config.toml is invalid; AstrLink will not overwrite it: {error}")
    })
}

fn read_config(path: &Path) -> Result<toml_edit::DocumentMut, String> {
    match fs::read_to_string(path) {
        Ok(raw) => parse_config(&raw),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(toml_edit::DocumentMut::new()),
        Err(error) => Err(format!("unable to read {}: {error}", path.display())),
    }
}

fn ensure_catalog_inside_codex_home(codex_home: &Path, path: &Path) -> Result<(), String> {
    let codex_home = codex_home
        .canonicalize()
        .map_err(|error| format!("unable to resolve the Codex home directory: {error}"))?;
    let resolved = resolve_catalog_path(path)?;
    if resolved.starts_with(&codex_home) {
        return Ok(());
    }
    Err(format!(
        "model catalog must stay inside the Codex home directory ({})",
        path.display()
    ))
}

fn catalog_write_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_symlink() {
        path.canonicalize()
            .map_err(|error| format!("unable to resolve {}: {error}", path.display()))
    } else {
        Ok(path.to_path_buf())
    }
}

fn resolve_catalog_path(path: &Path) -> Result<PathBuf, String> {
    if path.exists() {
        return path
            .canonicalize()
            .map_err(|error| format!("unable to resolve {}: {error}", path.display()));
    }
    let mut nearest = path;
    let mut suffix = Vec::new();
    loop {
        if nearest.exists() {
            let mut resolved = nearest
                .canonicalize()
                .map_err(|error| format!("unable to resolve {}: {error}", nearest.display()))?;
            for part in suffix.iter().rev() {
                resolved.push(part);
            }
            return Ok(normalize_lexical(&resolved));
        }
        let Some(name) = nearest.file_name() else {
            return Err(format!("unable to resolve {}", path.display()));
        };
        suffix.push(name.to_os_string());
        let Some(parent) = nearest.parent() else {
            return Err(format!("unable to resolve {}", path.display()));
        };
        if parent == nearest {
            return Err(format!("unable to resolve {}", path.display()));
        }
        nearest = parent;
    }
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn output_with_timeout(
    command: &mut Command,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_thread = thread::spawn(move || read_pipe(stdout));
    let stderr_thread = thread::spawn(move || read_pipe(stderr));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return Err(format!("timed out after {}s", timeout.as_secs()));
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(error) => return Err(error.to_string()),
        }
    };
    Ok(std::process::Output {
        status,
        stdout: join_pipe(stdout_thread),
        stderr: join_pipe(stderr_thread),
    })
}

fn read_pipe(pipe: Option<impl Read>) -> Vec<u8> {
    let mut buffer = Vec::new();
    if let Some(mut pipe) = pipe {
        let _ = pipe.read_to_end(&mut buffer);
    }
    buffer
}

fn join_pipe(handle: thread::JoinHandle<Vec<u8>>) -> Vec<u8> {
    handle.join().unwrap_or_default()
}

fn configured_catalog_path(
    config: &toml_edit::DocumentMut,
    codex_dir: &Path,
    user_home: &Path,
) -> Option<PathBuf> {
    let raw = config.get(CATALOG_KEY)?.as_str()?.trim();
    if raw.is_empty() {
        return None;
    }
    let path = if let Some(rest) = raw.strip_prefix("~/") {
        user_home.join(rest)
    } else {
        PathBuf::from(raw)
    };
    Some(if path.is_absolute() {
        path
    } else {
        codex_dir.join(path)
    })
}

fn ensure_profile_is_unambiguous(
    codex_dir: &Path,
    config: &toml_edit::DocumentMut,
) -> Result<(), String> {
    let Some(name) = config
        .get("profile")
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|name| !name.is_empty())
    else {
        return Ok(());
    };
    let profile_file = codex_dir.join("profiles").join(format!("{name}.toml"));
    let profile_table = config
        .get("profiles")
        .and_then(|item| item.as_table())
        .and_then(|table| table.get(name))
        .and_then(|item| item.as_table());
    let overrides_catalog_or_model = profile_table
        .is_some_and(|table| table.get("model").is_some() || table.get(CATALOG_KEY).is_some());
    if overrides_catalog_or_model || profile_file.is_file() {
        return Err(format!(
            "Codex profile {name} changes the session model or model catalog; AstrLink will not edit an ambiguous profile ({})",
            codex_dir.display()
        ));
    }
    if profile_table.is_none() {
        return Err(format!(
            "Codex config selects undefined profile {name}; AstrLink will not edit {}",
            codex_dir.display()
        ));
    }
    Ok(())
}

fn rollback_files(
    catalog_path: &Path,
    original_catalog: Option<&str>,
    config_path: &Path,
    original_config: Option<&str>,
) -> Result<(), String> {
    restore_or_remove(catalog_path, original_catalog)?;
    restore_or_remove(config_path, original_config)
}

fn restore_or_remove(path: &Path, original: Option<&str>) -> Result<(), String> {
    match original {
        Some(contents) => write_atomic(path, contents.as_bytes()),
        None if path.exists() => fs::remove_file(path)
            .map_err(|error| format!("unable to remove {}: {error}", path.display())),
        None => Ok(()),
    }
}

fn format_commit_error(error: String, rollback: Result<(), String>) -> String {
    match rollback {
        Ok(()) => error,
        Err(rollback_error) => format!("{error}; rollback failed: {rollback_error}"),
    }
}

fn session_model(config: &toml_edit::DocumentMut) -> Option<String> {
    config
        .get("model")
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
}

fn codex_dir(home: &Path) -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                home.join(path)
            }
        })
        .unwrap_or_else(|| home.join(".codex"))
}

fn read_text(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("unable to read {}: {error}", path.display()))
}

fn pretty_json(value: &Value) -> Result<String, String> {
    let mut encoded = serde_json::to_string_pretty(value)
        .map_err(|error| format!("unable to encode Codex model catalog: {error}"))?;
    encoded.push('\n');
    Ok(encoded)
}

fn backup_if_present(path: &Path, stamp: u64) -> Result<(), String> {
    if !path.is_file() {
        return Ok(());
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".astrlink-backup-{stamp}"));
    let backup = path.with_file_name(name);
    fs::copy(path, &backup)
        .map(|_| ())
        .map_err(|error| format!("unable to back up {}: {error}", path.display()))
}

fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("unable to create {}: {error}", parent.display()))?;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".astrlink-tmp-{}", std::process::id()));
    let temporary = path.with_file_name(name);
    fs::write(&temporary, contents)
        .map_err(|error| format!("unable to write {}: {error}", temporary.display()))?;
    if let Ok(metadata) = fs::metadata(path) {
        let _ = fs::set_permissions(&temporary, metadata.permissions());
    }
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!("unable to write {}: {error}", path.display())
    })
}

fn display_path(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{} is not valid UTF-8", path.display()))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog() -> Value {
        json!({"models": [
            {
                "slug": "gpt-6-astra",
                "display_name": "GPT-6-Astra",
                "visibility": "list",
                "use_responses_lite": false,
                "supported_reasoning_levels": [{"effort": "low"}]
            },
            {
                "slug": "codex-auto-review",
                "display_name": "Codex Auto Review",
                "visibility": "hide",
                "supported_reasoning_levels": [{"effort": "low"}]
            }
        ]})
    }

    fn slugs(catalog: &Value) -> Vec<&str> {
        catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(entry_slug)
            .collect()
    }

    fn unique_home(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "astrlink-codex-review-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(path.join(".codex")).unwrap();
        path
    }

    fn backups(dir: &Path) -> usize {
        fs::read_dir(dir)
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".astrlink-backup-")
            })
            .count()
    }

    #[test]
    fn override_updates_entries_and_preserves_unknown_fields() {
        let mut value = catalog();
        apply_review_model(&mut value, Some("gpt-6-astra"), Some("gpt-6-astra")).unwrap();
        assert_eq!(slugs(&value), ["gpt-6-astra"]);
        assert_eq!(value["models"][0][OVERRIDE_KEY], "gpt-6-astra");
        assert_eq!(value["models"][0]["use_responses_lite"], false);
        assert_eq!(
            effective_state(&value, Some("gpt-6-astra")),
            ReviewModelState::Override {
                model: "gpt-6-astra".into()
            }
        );
    }

    #[test]
    fn override_adds_missing_review_and_session_entries() {
        let mut value = catalog();
        apply_review_model(&mut value, Some("claude-sonnet"), Some("glm-5-air")).unwrap();
        assert_eq!(slugs(&value), ["gpt-6-astra", "glm-5-air", "claude-sonnet"]);
        let added = &value["models"][1];
        assert_eq!(added["display_name"], "glm-5-air");
        assert_eq!(added["visibility"], "hide");
        assert!(added.get("use_responses_lite").is_none());
        assert_eq!(
            effective_state(&value, Some("claude-sonnet")),
            ReviewModelState::Override {
                model: "glm-5-air".into()
            }
        );
    }

    #[test]
    fn choosing_codex_auto_review_keeps_its_entry() {
        let mut value = catalog();
        apply_review_model(
            &mut value,
            Some("gpt-6-astra"),
            Some(CODEX_AUTO_REVIEW_MODEL),
        )
        .unwrap();
        assert_eq!(slugs(&value), ["gpt-6-astra", CODEX_AUTO_REVIEW_MODEL]);
        assert_eq!(
            value["models"][1]["display_name"], "Codex Auto Review",
            "the original entry must not be replaced"
        );
    }

    #[test]
    fn session_mode_clears_overrides_and_codex_auto_review() {
        let mut value = catalog();
        apply_review_model(&mut value, Some("gpt-6-astra"), Some("gpt-5.6-luna")).unwrap();
        apply_review_model(&mut value, Some("gpt-6-astra"), None).unwrap();
        assert!(value["models"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry.get(OVERRIDE_KEY).is_none()));
        assert!(!slugs(&value).contains(&CODEX_AUTO_REVIEW_MODEL));
        assert_eq!(
            effective_state(&value, Some("gpt-6-astra")),
            ReviewModelState::SessionModel
        );
        assert_eq!(
            effective_state(&catalog(), Some("gpt-6-astra")),
            ReviewModelState::CodexAutoReview
        );
    }

    #[test]
    fn invalid_catalog_and_config_are_not_overwritten() {
        let home = unique_home("invalid");
        let codex = home.join(".codex");
        let catalog_path = codex.join(DEFAULT_CATALOG_NAME);
        fs::write(&catalog_path, "{not json").unwrap();
        fs::write(
            codex.join("config.toml"),
            format!(
                "model_catalog_json = {:?}\n",
                catalog_path.to_str().unwrap()
            ),
        )
        .unwrap();
        let error = set_review_model(&home, Some("gpt-5.6-luna"), || unreachable!()).unwrap_err();
        assert!(error.contains("will not overwrite"), "{error}");
        assert_eq!(fs::read_to_string(&catalog_path).unwrap(), "{not json");

        fs::write(codex.join("config.toml"), "model = [").unwrap();
        let error = set_review_model(&home, Some("gpt-5.6-luna"), || unreachable!()).unwrap_err();
        assert!(error.contains("will not overwrite"), "{error}");
        assert_eq!(backups(&codex), 0);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn existing_catalog_is_backed_up_and_rewritten() {
        let home = unique_home("existing");
        let codex = home.join(".codex");
        let catalog_path = codex.join("custom.json");
        fs::write(&catalog_path, pretty_json(&catalog()).unwrap()).unwrap();
        let config = "model = \"gpt-6-astra\"\nmodel_catalog_json = \"custom.json\"\n";
        fs::write(codex.join("config.toml"), config).unwrap();

        let status = set_review_model(&home, Some(" gpt-5.6-luna "), || unreachable!()).unwrap();
        assert_eq!(
            status.state,
            ReviewModelState::Override {
                model: "gpt-5.6-luna".into()
            }
        );
        assert_eq!(
            fs::read_to_string(codex.join("config.toml")).unwrap(),
            config
        );
        assert_eq!(backups(&codex), 1);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn missing_catalog_is_generated_and_referenced_from_config() {
        let home = unique_home("generate");
        let codex = home.join(".codex");
        fs::write(
            codex.join("config.toml"),
            "# keep me\nmodel = \"gpt-6-astra\"\n\n[desktop]\nsansFontSize = 14\n",
        )
        .unwrap();
        let before = status(&home).unwrap();
        assert_eq!(before.state, ReviewModelState::BundledCatalog);
        assert!(!before.catalog_configured);

        let status = set_review_model(&home, None, || Ok(catalog().to_string())).unwrap();
        assert_eq!(status.state, ReviewModelState::SessionModel);
        assert!(status.catalog_configured && status.catalog_exists);
        let config = fs::read_to_string(codex.join("config.toml")).unwrap();
        assert!(config.starts_with("# keep me\n"), "{config}");
        assert!(config.contains("sansFontSize = 14"), "{config}");
        assert!(config.contains(&format!(
            "model_catalog_json = {:?}",
            codex.join(DEFAULT_CATALOG_NAME).to_str().unwrap()
        )));
        assert_eq!(backups(&codex), 1, "config.toml must be backed up");
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn rejects_empty_model_and_missing_codex() {
        let home = unique_home("reject");
        assert!(set_review_model(&home, Some("  "), || unreachable!()).is_err());
        fs::remove_dir_all(home.join(".codex")).unwrap();
        let error = set_review_model(&home, None, || unreachable!()).unwrap_err();
        assert!(error.contains("not detected"), "{error}");
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn override_without_session_model_stays_visible() {
        let home = unique_home("no-session");
        fs::write(home.join(".codex").join("config.toml"), "").unwrap();
        let status =
            set_review_model(&home, Some("glm-5-air"), || Ok(catalog().to_string())).unwrap();
        assert!(status.session_model.is_none());
        assert_eq!(
            status.state,
            ReviewModelState::Override {
                model: "glm-5-air".into()
            }
        );
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn unconfigured_existing_catalog_is_edited_not_replaced() {
        let home = unique_home("default-file");
        let codex = home.join(".codex");
        let catalog_path = codex.join(DEFAULT_CATALOG_NAME);
        fs::write(&catalog_path, pretty_json(&catalog()).unwrap()).unwrap();
        fs::write(codex.join("config.toml"), "model = \"gpt-6-astra\"\n").unwrap();
        let status = set_review_model(&home, Some("glm-5"), || unreachable!()).unwrap();
        let raw = fs::read_to_string(&catalog_path).unwrap();
        assert!(raw.contains("gpt-6-astra"), "{raw}");
        assert!(raw.contains("glm-5"), "{raw}");
        assert_eq!(
            status.state,
            ReviewModelState::Override {
                model: "glm-5".into()
            }
        );
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn catalog_outside_home_is_rejected() {
        let home = unique_home("outside");
        fs::write(
            home.join(".codex").join("config.toml"),
            "model_catalog_json = \"/etc/passwd\"\n",
        )
        .unwrap();
        let error = set_review_model(&home, Some("glm-5"), || unreachable!()).unwrap_err();
        assert!(error.contains("home directory"), "{error}");
        assert!(status(&home).unwrap_err().contains("home directory"));
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn subscription_login_file_is_left_untouched() {
        let home = unique_home("auth");
        let auth = home.join(".codex").join("auth.json");
        let secret = "{\"tokens\":{\"access_token\":\"keep\",\"refresh_token\":\"keep\"}}\n";
        fs::write(&auth, secret).unwrap();
        fs::write(
            home.join(".codex").join("config.toml"),
            "model = \"gpt-6-astra\"\n",
        )
        .unwrap();
        set_review_model(&home, Some("glm-5"), || Ok(catalog().to_string())).unwrap();
        assert_eq!(fs::read_to_string(&auth).unwrap(), secret);
        let _ = fs::remove_dir_all(home);
    }

    #[cfg(unix)]
    #[test]
    fn command_timeout_stops_a_hung_process() {
        let mut command = Command::new("sleep");
        command.arg("30");
        let error = output_with_timeout(&mut command, Duration::from_millis(200)).unwrap_err();
        assert!(error.contains("timed out"), "{error}");
    }
}
