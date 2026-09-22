use std::{
    collections::VecDeque,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

const LOG_DIRECTORY_NAME: &str = "logs";
const LOG_FILE_NAME: &str = "astrlink.log";
const ROTATED_LOG_FILE_NAME: &str = "astrlink.log.1";
const MAX_PENDING: usize = 64;
const MAX_RING: usize = 500;
const MAX_MESSAGE_BYTES: usize = 4096;
const ROTATE_AFTER: u64 = 2 * 1024 * 1024;
const MAX_TARGET_LEN: usize = 64;

static LOGGER: Mutex<Logger> = Mutex::new(Logger::new());
static PUBLISHER: Mutex<Option<Arc<dyn Fn(AppLogRecord) + Send + Sync>>> = Mutex::new(None);

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct AppLogRecord {
    pub sequence: u64,
    pub time: String,
    pub level: String,
    pub target: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Level {
    fn rank(self) -> u8 {
        match self {
            Level::Error => 5,
            Level::Warn => 4,
            Level::Info => 3,
            Level::Debug => 2,
            Level::Trace => 1,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
            Level::Trace => "trace",
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
            Level::Trace => "TRACE",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "error" => Some(Level::Error),
            "warn" => Some(Level::Warn),
            "info" => Some(Level::Info),
            "debug" => Some(Level::Debug),
            "trace" => Some(Level::Trace),
            _ => None,
        }
    }

    fn enabled(self, threshold: Level) -> bool {
        self.rank() >= threshold.rank()
    }
}

pub fn default_threshold() -> Level {
    if cfg!(debug_assertions) {
        Level::Debug
    } else {
        Level::Info
    }
}

pub fn resolve_threshold(env_value: Option<&str>) -> Level {
    env_value
        .and_then(Level::parse)
        .unwrap_or_else(default_threshold)
}

struct Logger {
    next_sequence: u64,
    threshold: Level,
    pending: VecDeque<String>,
    ring: VecDeque<AppLogRecord>,
    file: Option<File>,
    path: Option<PathBuf>,
    size: u64,
}

impl Logger {
    const fn new() -> Self {
        Self {
            next_sequence: 0,
            threshold: if cfg!(debug_assertions) {
                Level::Debug
            } else {
                Level::Info
            },
            pending: VecDeque::new(),
            ring: VecDeque::new(),
            file: None,
            path: None,
            size: 0,
        }
    }

    fn enabled(&self, level: Level) -> bool {
        level.enabled(self.threshold)
    }

    fn emit(
        &mut self,
        level: Level,
        target: &str,
        message: &str,
        now: SystemTime,
    ) -> Option<AppLogRecord> {
        if !self.enabled(level) {
            return None;
        }
        self.next_sequence += 1;
        let record = AppLogRecord {
            sequence: self.next_sequence,
            time: format_utc_timestamp(now),
            level: level.name().to_string(),
            target: sanitize_target(target).to_string(),
            message: sanitize_message(message),
        };
        self.push_record(record.clone());
        let _ = self.write_line(&format_line(now, level, target, message));
        Some(record)
    }

    fn push_record(&mut self, record: AppLogRecord) {
        if self.ring.len() == MAX_RING {
            self.ring.pop_front();
        }
        self.ring.push_back(record);
    }

    fn init(&mut self, app_data_dir: &Path, threshold: Level) -> io::Result<()> {
        self.threshold = threshold;
        let path = log_file_path(app_data_dir);
        if let Some(directory) = path.parent() {
            fs::create_dir_all(directory)?;
        }
        let first_open = self.path.is_none();
        if path.exists() && fs::metadata(&path)?.len() > ROTATE_AFTER {
            rotate_file(&path)?;
        }
        if first_open {
            self.replay_tail(&path);
        }
        self.path = Some(path.clone());
        self.reopen(&path)?;
        let pending = std::mem::take(&mut self.pending);
        for line in pending {
            self.write_line(&line)?;
        }
        Ok(())
    }

    fn replay_tail(&mut self, path: &Path) {
        let Ok(text) = fs::read_to_string(path) else {
            return;
        };
        let mut history = VecDeque::new();
        for line in text.lines() {
            let Some(record) = parse_log_line(line) else {
                continue;
            };
            if history.len() == MAX_RING {
                history.pop_front();
            }
            history.push_back(record);
        }
        for record in std::mem::take(&mut self.ring) {
            if history.len() == MAX_RING {
                history.pop_front();
            }
            history.push_back(record);
        }
        // Replay happens during setup, before the publisher is installed.
        // Give the file tail and pre-init buffer one ordered, process-local ID
        // space so history snapshots can be merged with live events.
        for record in &mut history {
            self.next_sequence += 1;
            record.sequence = self.next_sequence;
        }
        self.ring = history;
    }

    fn write_line(&mut self, line: &str) -> io::Result<()> {
        if self.file.is_none() {
            if self.pending.len() == MAX_PENDING {
                self.pending.pop_front();
            }
            self.pending.push_back(line.to_string());
            return Ok(());
        }
        let bytes = line.as_bytes();
        if let Some(file) = self.file.as_mut() {
            file.write_all(bytes)?;
            file.flush()?;
        }
        self.size = self.size.saturating_add(bytes.len() as u64);
        if self.size > ROTATE_AFTER {
            self.rotate()?;
        }
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        self.file.take();
        if let Err(error) = rotate_file(&path) {
            let _ = self.reopen(&path);
            return Err(error);
        }
        self.reopen(&path)
    }

    fn reopen(&mut self, path: &Path) -> io::Result<()> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        self.size = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        self.file = Some(file);
        Ok(())
    }
}

fn rotate_file(path: &Path) -> io::Result<()> {
    let rotated = path.with_file_name(ROTATED_LOG_FILE_NAME);
    if rotated.exists() {
        fs::remove_file(&rotated)?;
    }
    if path.exists() {
        fs::rename(path, rotated)?;
    }
    Ok(())
}

fn lock_logger() -> MutexGuard<'static, Logger> {
    LOGGER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn log_file_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join(LOG_DIRECTORY_NAME).join(LOG_FILE_NAME)
}

pub fn init(app_data_dir: &Path) -> Result<(), String> {
    let threshold = resolve_threshold(std::env::var("ASTRLINK_LOG").ok().as_deref());
    lock_logger()
        .init(app_data_dir, threshold)
        .map_err(|error| format!("unable to open AstrLink log file: {error}"))
}

pub fn enabled(level: Level) -> bool {
    lock_logger().enabled(level)
}

pub fn write(level: Level, target: &str, args: fmt::Arguments<'_>) {
    let record = {
        let mut logger = lock_logger();
        if !logger.enabled(level) {
            return;
        }
        let message = args.to_string();
        logger.emit(level, target, &message, SystemTime::now())
    };
    if let Some(record) = record {
        publish(record);
    }
}

pub fn recent() -> Vec<AppLogRecord> {
    lock_logger().ring.iter().cloned().collect()
}

pub fn set_publisher(publisher: impl Fn(AppLogRecord) + Send + Sync + 'static) {
    let mut guard = PUBLISHER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(Arc::new(publisher));
}

fn publish(record: AppLogRecord) {
    let publisher = PUBLISHER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    if let Some(publisher) = publisher {
        publisher(record);
    }
}

pub fn append_from_ui(level: &str, target: &str, message: &str) -> Result<(), String> {
    let level = Level::parse(level).ok_or_else(|| "invalid log level".to_string())?;
    if !is_valid_target(target) {
        return Err("invalid log target".into());
    }
    write(level, target, format_args!("{message}"));
    Ok(())
}

pub fn is_valid_target(target: &str) -> bool {
    (1..=MAX_TARGET_LEN).contains(&target.len())
        && target.bytes().all(|byte| {
            matches!(
                byte,
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'
            )
        })
}

fn sanitize_target(target: &str) -> &str {
    if is_valid_target(target) {
        target
    } else {
        "invalid"
    }
}

pub fn sanitize_message(message: &str) -> String {
    let collapsed: String = message
        .chars()
        .map(|character| {
            if character == '\n' || character == '\r' {
                ' '
            } else {
                character
            }
        })
        .collect();
    truncate_bytes(&collapsed, MAX_MESSAGE_BYTES)
}

fn truncate_bytes(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn parse_log_line(line: &str) -> Option<AppLogRecord> {
    let mut parts = line.splitn(4, ' ');
    let time = parts.next()?;
    let level = Level::parse(parts.next()?)?;
    let target = parts.next()?;
    let message = parts.next().unwrap_or("");
    if !time.ends_with('Z') || !time.contains('T') || !is_valid_target(target) {
        return None;
    }
    Some(AppLogRecord {
        sequence: 0, // Assigned by replay_tail before this record is exposed.
        time: time.to_string(),
        level: level.name().to_string(),
        target: target.to_string(),
        message: message.to_string(),
    })
}

pub fn format_line(now: SystemTime, level: Level, target: &str, message: &str) -> String {
    format!(
        "{} {} {} {}\n",
        format_utc_timestamp(now),
        level.as_str(),
        sanitize_target(target),
        sanitize_message(message)
    )
}

fn format_utc_timestamp(now: SystemTime) -> String {
    let duration = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = duration.as_secs();
    let millis = duration.subsec_millis();
    let days = (seconds / 86_400) as i64;
    let time_of_day = seconds % 86_400;
    let hour = time_of_day / 3_600;
    let minute = (time_of_day % 3_600) / 60;
    let second = time_of_day % 60;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Days since 1970-01-01 to civil year/month/day (Howard Hinnant).
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    }
    .div_euclid(146_097);
    let day_of_era = (shifted - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_period = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_period + 2) / 5 + 1;
    let month = if month_period < 10 {
        month_period + 3
    } else {
        month_period - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    (year as i32, month as u32, day as u32)
}

macro_rules! error {
    ($target:expr, $($arg:tt)+) => {
        if $crate::app_log::enabled($crate::app_log::Level::Error) {
            $crate::app_log::write(
                $crate::app_log::Level::Error,
                $target,
                ::std::format_args!($($arg)+),
            );
        }
    };
}
pub(crate) use error;

macro_rules! warning {
    ($target:expr, $($arg:tt)+) => {
        if $crate::app_log::enabled($crate::app_log::Level::Warn) {
            $crate::app_log::write(
                $crate::app_log::Level::Warn,
                $target,
                ::std::format_args!($($arg)+),
            );
        }
    };
}
pub(crate) use warning;

#[allow(unused_macros)]
macro_rules! info {
    ($target:expr, $($arg:tt)+) => {
        if $crate::app_log::enabled($crate::app_log::Level::Info) {
            $crate::app_log::write(
                $crate::app_log::Level::Info,
                $target,
                ::std::format_args!($($arg)+),
            );
        }
    };
}
pub(crate) use info;

#[allow(unused_macros)]
macro_rules! debug {
    ($target:expr, $($arg:tt)+) => {
        if $crate::app_log::enabled($crate::app_log::Level::Debug) {
            $crate::app_log::write(
                $crate::app_log::Level::Debug,
                $target,
                ::std::format_args!($($arg)+),
            );
        }
    };
}
pub(crate) use debug;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn temporary_directory(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "astrlink-app-log-{name}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ))
    }

    fn read_log(directory: &Path) -> String {
        fs::read_to_string(directory.join(LOG_DIRECTORY_NAME).join(LOG_FILE_NAME)).unwrap()
    }

    #[test]
    fn parses_level_names() {
        assert_eq!(Level::parse("ERROR"), Some(Level::Error));
        assert_eq!(Level::parse(" warn "), Some(Level::Warn));
        assert_eq!(Level::parse("nope"), None);
    }

    #[test]
    fn env_overrides_default_threshold() {
        assert_eq!(resolve_threshold(Some("trace")), Level::Trace);
        assert_eq!(resolve_threshold(Some("nope")), default_threshold());
        assert_eq!(resolve_threshold(None), default_threshold());
    }

    #[test]
    fn drops_records_below_threshold_before_format() {
        let mut logger = Logger::new();
        logger.threshold = Level::Info;
        assert!(logger
            .emit(Level::Debug, "shell.test", "hidden", UNIX_EPOCH)
            .is_none());
        assert!(logger
            .emit(Level::Info, "shell.test", "kept", UNIX_EPOCH)
            .is_some());
        assert_eq!(logger.pending.len(), 1);
        assert!(logger.pending[0].contains("kept"));
        assert!(!logger.pending[0].contains("hidden"));
    }

    #[test]
    fn formats_stable_utc_line() {
        let time = UNIX_EPOCH + Duration::from_millis(1_790_055_420_123);
        assert_eq!(
            format_line(time, Level::Info, "shell.sidecar", "message"),
            "2026-09-22T05:37:00.123Z INFO shell.sidecar message\n"
        );
        assert_eq!(format_utc_timestamp(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            format_utc_timestamp(UNIX_EPOCH + Duration::from_secs(1_704_067_200)),
            "2024-01-01T00:00:00.000Z"
        );
    }

    #[test]
    fn collapses_newlines_and_truncates_long_messages() {
        assert_eq!(sanitize_message("hello\r\nworld"), "hello  world");
        let long = "é".repeat(3000);
        let sanitized = sanitize_message(&long);
        assert!(sanitized.len() <= MAX_MESSAGE_BYTES);
        assert!(sanitized.is_char_boundary(sanitized.len()));
        assert!(sanitized.starts_with('é'));
    }

    #[test]
    fn log_file_path_is_stable_under_the_data_directory() {
        assert_eq!(
            log_file_path(Path::new("data").join("astrlink").as_path()),
            PathBuf::from("data")
                .join("astrlink")
                .join("logs")
                .join("astrlink.log")
        );
    }

    #[test]
    fn rejects_invalid_targets() {
        assert!(is_valid_target("shell.sidecar"));
        assert!(is_valid_target("ui.theme"));
        assert!(!is_valid_target(""));
        assert!(!is_valid_target("has space"));
        assert!(!is_valid_target(&"a".repeat(65)));
        assert_eq!(
            format_line(UNIX_EPOCH, Level::Error, "bad target", "x"),
            "1970-01-01T00:00:00.000Z ERROR invalid x\n"
        );
    }

    #[test]
    fn flushes_pre_init_buffer_after_opening_the_file() {
        let directory = temporary_directory("pending");
        let _ = fs::remove_dir_all(&directory);
        let mut logger = Logger::new();
        logger.threshold = Level::Info;
        logger.emit(Level::Info, "shell.test", "before", UNIX_EPOCH);
        logger.emit(Level::Debug, "shell.test", "hidden", UNIX_EPOCH);
        logger.init(&directory, Level::Info).unwrap();
        let text = read_log(&directory);
        assert!(text.contains("before"));
        assert!(!text.contains("hidden"));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn rotates_to_a_single_backup_when_the_file_exceeds_two_mebibytes() {
        let directory = temporary_directory("rotate");
        let _ = fs::remove_dir_all(&directory);
        let logs = directory.join(LOG_DIRECTORY_NAME);
        fs::create_dir_all(&logs).unwrap();
        let current = logs.join(LOG_FILE_NAME);
        fs::write(&current, vec![b'x'; ROTATE_AFTER as usize + 8]).unwrap();
        fs::write(logs.join(ROTATED_LOG_FILE_NAME), b"stale").unwrap();

        let mut logger = Logger::new();
        logger.init(&directory, Level::Info).unwrap();
        logger.emit(Level::Info, "shell.test", "fresh", UNIX_EPOCH);

        let archived = fs::read(logs.join(ROTATED_LOG_FILE_NAME)).unwrap();
        assert_eq!(archived.len(), ROTATE_AFTER as usize + 8);
        assert!(!archived.starts_with(b"stale"));
        let active = read_log(&directory);
        assert!(active.contains("fresh"));
        assert!(!active.contains("xxxxxxxx"));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn replays_the_log_tail_into_the_ring_on_first_open() {
        let directory = temporary_directory("replay");
        let _ = fs::remove_dir_all(&directory);
        let logs = directory.join(LOG_DIRECTORY_NAME);
        fs::create_dir_all(&logs).unwrap();
        let mut lines = String::new();
        for index in 0..(MAX_RING + 10) {
            lines.push_str(&format_line(
                UNIX_EPOCH,
                Level::Info,
                "shell.test",
                &format!("old-{index}"),
            ));
        }
        lines.push_str("not a log line\n");
        fs::write(logs.join(LOG_FILE_NAME), lines).unwrap();

        let mut logger = Logger::new();
        logger.threshold = Level::Info;
        logger
            .emit(Level::Info, "shell.test", "live", UNIX_EPOCH)
            .unwrap();
        logger.init(&directory, Level::Info).unwrap();

        assert_eq!(logger.ring.len(), MAX_RING);
        assert_eq!(logger.ring.front().unwrap().message, "old-11");
        assert_eq!(logger.ring.front().unwrap().level, "info");
        assert_eq!(logger.ring.back().unwrap().message, "live");
        let len = logger.ring.len();
        logger.init(&directory, Level::Info).unwrap();
        assert_eq!(logger.ring.len(), len);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn ring_keeps_the_newest_five_hundred_records() {
        let mut logger = Logger::new();
        logger.threshold = Level::Info;
        for index in 0..(MAX_RING + 5) {
            logger.emit(
                Level::Info,
                "shell.test",
                &format!("row-{index}"),
                UNIX_EPOCH,
            );
        }
        assert_eq!(logger.ring.len(), MAX_RING);
        assert_eq!(logger.ring.front().unwrap().message, "row-5");
        assert_eq!(
            logger.ring.back().unwrap().message,
            format!("row-{}", MAX_RING + 4)
        );
        assert_eq!(logger.ring.back().unwrap().level, "info");
    }

    #[test]
    fn identical_log_messages_get_distinct_ordered_sequences() {
        let mut logger = Logger::new();
        let first = logger
            .emit(Level::Info, "shell.test", "same", UNIX_EPOCH)
            .unwrap();
        let second = logger
            .emit(Level::Info, "shell.test", "same", UNIX_EPOCH)
            .unwrap();
        assert!(first.sequence > 0);
        assert!(second.sequence > first.sequence);
        assert_eq!(logger.ring[0], first);
        assert_eq!(logger.ring[1], second);
    }

    #[test]
    fn keeps_only_the_last_pending_records() {
        let mut logger = Logger::new();
        logger.threshold = Level::Info;
        for index in 0..(MAX_PENDING + 5) {
            logger.emit(
                Level::Info,
                "shell.test",
                &format!("row-{index}"),
                UNIX_EPOCH,
            );
        }
        assert_eq!(logger.pending.len(), MAX_PENDING);
        assert!(logger.pending.front().unwrap().contains("row-5"));
        assert!(logger.pending.back().unwrap().contains("row-68"));
    }
}
