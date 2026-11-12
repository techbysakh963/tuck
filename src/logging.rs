//! A small file logger that rotates while the app runs and collapses repeats.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use log::{Level, LevelFilter, Log, Metadata, Record};

pub const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;

const LOG_FILE: &str = "tuck.log";

struct State {
    file: File,
    path: PathBuf,
    written: u64,
    max: u64,
    last: Option<(Level, String, String)>,
    repeats: u64,
}

pub struct FileLogger {
    state: Mutex<State>,
    level: LevelFilter,
}

impl FileLogger {
    pub fn new(dir: &Path, max_bytes: u64, level: LevelFilter) -> std::io::Result<Self> {
        crate::private::create_dir(dir)?;
        let path = dir.join(LOG_FILE);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)?;
        let written = file.metadata()?.len();
        Ok(Self {
            state: Mutex::new(State {
                file,
                path,
                written,
                max: max_bytes,
                last: None,
                repeats: 0,
            }),
            level,
        })
    }
}

impl State {
    fn write_line(&mut self, line: &str) {
        let len = line.len() as u64;
        if self.written > 0 && self.written + len > self.max {
            self.rotate();
        }
        if self.file.write_all(line.as_bytes()).is_ok() {
            self.written += len;
        }
    }

    /// Keeps exactly one backup: `tuck.log.1`.
    fn rotate(&mut self) {
        let mut backup = OsString::from(self.path.as_os_str());
        backup.push(".1");
        let _ = fs::rename(&self.path, &backup);
        if let Ok(file) = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&self.path)
        {
            self.file = file;
            self.written = 0;
        }
    }

    fn flush_repeats(&mut self) {
        if self.repeats > 0 {
            let line = format!(
                "{} last message repeated {} times\n",
                timestamp(),
                self.repeats
            );
            self.repeats = 0;
            self.write_line(&line);
        }
    }
}

fn timestamp() -> String {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}", d.as_secs(), d.subsec_millis())
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let message = record.args().to_string();
        let key = (record.level(), record.target().to_string(), message);
        let Ok(mut st) = self.state.lock() else {
            return;
        };
        if st.last.as_ref() == Some(&key) {
            st.repeats += 1;
            return;
        }
        st.flush_repeats();
        let line = format!(
            "{} {:<5} {}: {}\n",
            timestamp(),
            key.0,
            key.1,
            key.2.replace('\n', "\n    ")
        );
        st.write_line(&line);
        st.last = Some(key);
    }

    fn flush(&self) {
        if let Ok(mut st) = self.state.lock() {
            st.flush_repeats();
            let _ = st.file.flush();
        }
    }
}

/// Installs the file logger in `dir` as the global logger.
pub fn init_logging(dir: &Path, level: LevelFilter) -> Result<(), String> {
    let logger = FileLogger::new(dir, MAX_LOG_BYTES, level)
        .map_err(|e| format!("cannot open log in {}: {e}", dir.display()))?;
    log::set_boxed_logger(Box::new(logger)).map_err(|e| e.to_string())?;
    log::set_max_level(level);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emit(logger: &FileLogger, level: Level, msg: &str) {
        logger.log(
            &Record::builder()
                .args(format_args!("{msg}"))
                .level(level)
                .target("tuck::test")
                .build(),
        );
    }

    #[test]
    fn log_rotates_while_running() {
        let tmp = tempfile::tempdir().unwrap();
        let logger = FileLogger::new(tmp.path(), 1000, LevelFilter::Info).unwrap();
        for i in 0..500 {
            emit(&logger, Level::Info, &format!("record number {i}"));
        }
        let active = fs::read_to_string(tmp.path().join("tuck.log")).unwrap();
        let backup = fs::read_to_string(tmp.path().join("tuck.log.1")).unwrap();
        assert!(active.len() <= 1200, "active is {} bytes", active.len());
        assert!(backup.len() <= 1200, "backup is {} bytes", backup.len());
        assert!(active.contains("record number 499"));
        assert!(!tmp.path().join("tuck.log.2").exists());
    }

    #[test]
    fn repeated_lines_collapse() {
        let tmp = tempfile::tempdir().unwrap();
        let logger = FileLogger::new(tmp.path(), MAX_LOG_BYTES, LevelFilter::Info).unwrap();
        for _ in 0..50 {
            emit(&logger, Level::Warn, "clipboard owner went away");
        }
        emit(&logger, Level::Info, "something else");
        let text = fs::read_to_string(tmp.path().join("tuck.log")).unwrap();
        assert_eq!(text.matches("clipboard owner went away").count(), 1);
        assert!(text.contains("last message repeated 49 times"));
        assert!(text.contains("something else"));
    }

    #[test]
    fn messages_below_level_are_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let logger = FileLogger::new(tmp.path(), MAX_LOG_BYTES, LevelFilter::Warn).unwrap();
        emit(&logger, Level::Info, "quiet");
        emit(&logger, Level::Error, "loud");
        let text = fs::read_to_string(tmp.path().join("tuck.log")).unwrap();
        assert!(!text.contains("quiet"));
        assert!(text.contains("loud"));
    }
}
