//! A small file logger: one line per record in `crosscom.log`, which both DLLs append to.

use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;
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
    /// Lines written for the overlay's graphics hook so far.
    library_lines: AtomicU32,
}

/// The most lines the overlay's graphics hook (hudhook) writes: it can log a failure for every
/// frame, thousands a minute.
const MAX_LIBRARY_LINES: u32 = 200;

/// Whether a record from `target` is written (and if so, whether it's the last one that will be),
/// counting the graphics hook's in `count`.
fn library_line(target: &str, count: &AtomicU32) -> Option<bool> {
    if !target.starts_with("hudhook") {
        return Some(false);
    }
    let n = count.fetch_add(1, Ordering::Relaxed) + 1;
    (n <= MAX_LIBRARY_LINES).then_some(n == MAX_LIBRARY_LINES)
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let Some(note) = library_line(record.target(), &self.library_lines) else {
            return;
        };
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        let line = format!("{}.{:03} {:5} {} {}\n", now.as_secs(), now.subsec_millis(), record.level(), self.source, record.args());
        if let Ok(mut file) = self.file.lock() {
            let _ = file.write_all(line.as_bytes());
            if note {
                let _ = writeln!(
                    file,
                    "{}.{:03} INFO  {} (the overlay's graphics hook says no more here: {MAX_LIBRARY_LINES} lines)",
                    now.as_secs(),
                    now.subsec_millis(),
                    self.source
                );
            }
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
    let logger = LOGGER.get_or_init(|| FileLogger {
        file: Mutex::new(file),
        source,
        library_lines: AtomicU32::new(0),
    });
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
    fn the_graphics_hook_says_only_so_much() {
        let count = AtomicU32::new(0);
        assert_eq!(library_line("ubiorbitapi_r2_loader::overlay", &count), Some(false));
        for _ in 1..MAX_LIBRARY_LINES {
            assert_eq!(library_line("hudhook::hooks::dx11", &count), Some(false));
        }
        assert_eq!(library_line("hudhook::renderer", &count), Some(true), "the last one says so");
        assert_eq!(library_line("hudhook::renderer", &count), None);
        assert_eq!(library_line("crosscom", &count), Some(false), "ours are all written");
    }

    #[test]
    fn level_defaults_to_info() {
        assert_eq!(parse_level(None), LevelFilter::Info);
        assert_eq!(parse_level(Some("nonsense")), LevelFilter::Info);
        assert_eq!(parse_level(Some("debug")), LevelFilter::Debug);
    }
}
