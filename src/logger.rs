use std::fmt;
use std::io::Write;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, PartialOrd)]
pub enum Level {
    Debug = 0,
    Info,
    Warn,
    Error,
    Fatal,
}

static MIN_LEVEL: AtomicU8 = AtomicU8::new(Level::Info as u8);

fn level_to_string(level: Level) -> &'static str {
    match level {
        Level::Debug => "DEBUG",
        Level::Info => "INFO ",
        Level::Warn => "WARN ",
        Level::Error => "ERROR",
        Level::Fatal => "FATAL",
    }
}

/// Reads LOG_LEVEL from the environment (DEBUG, INFO, WARN, ERROR). Defaults to INFO.
pub fn init() {
    let Ok(log_level) = std::env::var("LOG_LEVEL") else {
        set_level(Level::Info);
        return;
    };

    match log_level.to_ascii_uppercase().as_str() {
        "DEBUG" => {
            set_level(Level::Debug);
            crate::log_warn!("DEBUG logging enabled. Expect verbose output.");
        }
        "INFO" => set_level(Level::Info),
        "WARN" => set_level(Level::Warn),
        "ERROR" => set_level(Level::Error),
        _ => {
            set_level(Level::Info);
            crate::log_warn!("Unknown LOG_LEVEL='{}', defaulting to INFO.", log_level);
        }
    }
}

pub fn set_level(level: Level) {
    MIN_LEVEL.store(level as u8, Ordering::Relaxed);
}

pub fn log(level: Level, file: &str, line: u32, func: &str, args: fmt::Arguments) {
    if (level as u8) < MIN_LEVEL.load(Ordering::Relaxed) {
        return;
    }

    let time = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let tag = level_to_string(level);

    let line = match level {
        Level::Debug | Level::Fatal => format!("{time} [{tag}] {file}:{line} {func}: {args}\n"),
        Level::Info => format!("{time} [{tag}] {args}\n"),
        Level::Warn | Level::Error => format!("{time} [{tag}] {func}: {args}\n"),
    };

    let mut stderr = std::io::stderr().lock();
    let _ = stderr.write_all(line.as_bytes());
    let _ = stderr.flush();
}

#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => { $crate::logger::log($crate::logger::Level::Debug, file!(), line!(), module_path!(), format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::logger::log($crate::logger::Level::Info, file!(), line!(), module_path!(), format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => { $crate::logger::log($crate::logger::Level::Warn, file!(), line!(), module_path!(), format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => { $crate::logger::log($crate::logger::Level::Error, file!(), line!(), module_path!(), format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! log_fatal {
    ($($arg:tt)*) => { $crate::logger::log($crate::logger::Level::Fatal, file!(), line!(), module_path!(), format_args!($($arg)*)) };
}
