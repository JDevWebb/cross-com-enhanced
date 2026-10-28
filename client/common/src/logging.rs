//! A small file logger: one line per record in `crosscom.log`, which both DLLs append to.

use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use log::LevelFilter;
use log::Log;
use log::Metadata;
use log::Record;

pub const FILE_NAME: &str = "crosscom.log";

struct FileLogger {
    file: Mutex<File>,
    /// Which DLL wrote the line.
    source: &'static str,
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        let line = format!("{}.{:03} {:5} {} {}\n", now.as_secs(), now.subsec_millis(), record.level(), self.source, record.args());
        if let Ok(mut file) = self.file.lock() {
            let _ = file.write_all(line.as_bytes());
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            let _ = file.flush();
        }
    }
}

static LOGGER: OnceLock<FileLogger> = OnceLock::new();

/// Starts logging to `crosscom.log` in `dir`. Only the first call in a DLL does anything; failures
/// are silent, since there's nowhere to report them.
pub fn init(dir: &Path, source: &'static str, level: Option<&str>) {
    let Ok(file) = OpenOptions::new().create(true).append(true).open(dir.join(FILE_NAME)) else {
        return;
    };
    let logger = LOGGER.get_or_init(|| FileLogger { file: Mutex::new(file), source });
    if log::set_logger(logger).is_ok() {
        log::set_max_level(parse_level(level));
    }
}

fn parse_level(level: Option<&str>) -> LevelFilter {
    level.and_then(|l| l.parse().ok()).unwrap_or(LevelFilter::Info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_defaults_to_info() {
        assert_eq!(parse_level(None), LevelFilter::Info);
        assert_eq!(parse_level(Some("nonsense")), LevelFilter::Info);
        assert_eq!(parse_level(Some("debug")), LevelFilter::Debug);
    }
}
