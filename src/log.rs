//! A log of one line per event, to standard error.
//!
//! Fulcrum keeps its dependencies few, so this is its own: a level, a message,
//! and nothing else. `FULCRUM_LOG` sets the level: `error`, `warn`, `info` (the
//! default), or `debug`. CrowbarOS's checks read some lines by their words, so
//! a message that a check reads keeps its wording.

use std::{
    io::Write,
    sync::atomic::{AtomicU8, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Level {
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
}

static LEVEL: AtomicU8 = AtomicU8::new(Level::Info as u8);

/// Reads `FULCRUM_LOG`. A value it does not know leaves the level at `info`.
pub fn init_from_env() {
    let level = match std::env::var("FULCRUM_LOG").as_deref() {
        Ok("error") => Level::Error,
        Ok("warn") => Level::Warn,
        Ok("debug") => Level::Debug,
        _ => Level::Info,
    };
    set_level(level);
}

pub fn set_level(level: Level) {
    LEVEL.store(level as u8, Ordering::Relaxed);
}

pub fn enabled(level: Level) -> bool {
    level as u8 <= LEVEL.load(Ordering::Relaxed)
}

#[doc(hidden)]
pub fn write(level: Level, message: std::fmt::Arguments<'_>) {
    let tag = match level {
        Level::Error => "ERROR",
        Level::Warn => "WARN",
        Level::Info => "INFO",
        Level::Debug => "DEBUG",
    };
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "{tag} crowbar_compositor: {message}");
}

macro_rules! log {
    ($level:expr, $($arg:tt)*) => {
        if $crate::log::enabled($level) {
            $crate::log::write($level, format_args!($($arg)*));
        }
    };
}

macro_rules! error { ($($arg:tt)*) => { $crate::log::log!($crate::log::Level::Error, $($arg)*) }; }
macro_rules! warning { ($($arg:tt)*) => { $crate::log::log!($crate::log::Level::Warn, $($arg)*) }; }
macro_rules! info { ($($arg:tt)*) => { $crate::log::log!($crate::log::Level::Info, $($arg)*) }; }
macro_rules! debug { ($($arg:tt)*) => { $crate::log::log!($crate::log::Level::Debug, $($arg)*) }; }

#[allow(unused_imports)]
pub(crate) use {debug, error, info, log, warning};
