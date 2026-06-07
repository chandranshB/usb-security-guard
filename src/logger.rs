//! Windows Event Log logger for USB Security Guard.
//!
//! Provides both direct logging functions ([`info`], [`warning`], [`error`]) and
//! a [`log::Log`] trait implementation so that the standard `log::info!()`,
//! `log::warn!()`, and `log::error!()` macros route to the Windows Event Log.
//!
//! # Usage
//!
//! ```no_run
//! logger::init("MyServiceName");
//! log::info!("Service started");
//! ```

use std::sync::OnceLock;

use winapi::shared::minwindef::WORD;
use winapi::um::winbase::{DeregisterEventSource, RegisterEventSourceW, ReportEventW};
use winapi::um::winnt::{
    EVENTLOG_ERROR_TYPE, EVENTLOG_INFORMATION_TYPE, EVENTLOG_WARNING_TYPE, HANDLE,
};

// ─── Global state ────────────────────────────────────────────────────────────

/// The registered Windows Event Source handle.
///
/// Initialised once by [`init`] and never replaced. The handle is deliberately
/// leaked (never deregistered) because the process lifetime == service lifetime
/// and Windows cleans up on exit.
static EVENT_SOURCE: OnceLock<EventSource> = OnceLock::new();

/// Wrapper around the raw `HANDLE` so we can store it in a `OnceLock`.
///
/// # Safety
/// `HANDLE` is a pointer type. The Event Source handle is valid for the entire
/// process lifetime and is safe to share across threads (Windows Event Log API
/// is thread-safe).
struct EventSource(HANDLE);

// SAFETY: The Windows Event Source handle is process-global and thread-safe.
unsafe impl Send for EventSource {}
unsafe impl Sync for EventSource {}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Initialise the logger subsystem.
///
/// * Registers an event source with the Windows Event Log under `source_name`.
/// * Sets this module as the global `log` crate logger at `log::LevelFilter::Info`.
///
/// Must be called **once** at process startup. Subsequent calls are harmless
/// no-ops (the `OnceLock` prevents double-init).
pub fn init(source_name: &str) {
    // Register the event source with Windows.
    let wide_name = to_wide(source_name);

    // SAFETY: `RegisterEventSourceW` is safe to call with a valid wide string.
    // A null first parameter means the local machine.
    let handle = unsafe { RegisterEventSourceW(std::ptr::null(), wide_name.as_ptr()) };

    if handle.is_null() {
        // Can't log yet — best-effort: write to stderr.
        eprintln!("[logger] RegisterEventSourceW failed");
        return;
    }

    let _ = EVENT_SOURCE.set(EventSource(handle));

    // Register as the global `log` crate logger. Ignore the error if already set.
    let _ = log::set_logger(&EventLogger);
    log::set_max_level(log::LevelFilter::Info);
}

/// Log an informational message to the Windows Event Log.
pub fn info(msg: &str) {
    report(EVENTLOG_INFORMATION_TYPE, msg);
}

/// Log a warning message to the Windows Event Log.
pub fn warning(msg: &str) {
    report(EVENTLOG_WARNING_TYPE, msg);
}

/// Log an error message to the Windows Event Log.
pub fn error(msg: &str) {
    report(EVENTLOG_ERROR_TYPE, msg);
}

// ─── log::Log implementation ─────────────────────────────────────────────────

/// Zero-sized type implementing [`log::Log`] for the global logger.
struct EventLogger;

impl log::Log for EventLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let event_type = match record.level() {
            log::Level::Error => EVENTLOG_ERROR_TYPE,
            log::Level::Warn => EVENTLOG_WARNING_TYPE,
            // Info, Debug, Trace all map to INFORMATION.
            _ => EVENTLOG_INFORMATION_TYPE,
        };

        let msg = format!("{}", record.args());
        report(event_type, &msg);
    }

    fn flush(&self) {
        // Windows Event Log writes are synchronous — nothing to flush.
    }
}

// ─── Internal helpers ────────────────────────────────────────────────────────

/// Write a single event to the Windows Event Log.
///
/// Silently does nothing if [`init`] has not been called yet.
fn report(event_type: WORD, msg: &str) {
    let source = match EVENT_SOURCE.get() {
        Some(s) => s,
        None => return, // Logger not yet initialised.
    };

    let wide_msg = to_wide(msg);
    let msg_ptr = wide_msg.as_ptr();

    // SAFETY: `ReportEventW` expects:
    //   - A valid event source handle (we hold one from `RegisterEventSourceW`).
    //   - A pointer to an array of wide-string pointers. We pass a single-element
    //     array on the stack.
    //   - Zero-length raw data (last two params are 0 / null).
    unsafe {
        ReportEventW(
            source.0,
            event_type,
            0,    // wCategory
            0,    // dwEventID
            std::ptr::null_mut(), // lpUserSid
            1,    // wNumStrings
            0,    // dwDataSize
            &msg_ptr as *const *const u16 as *mut *const u16,
            std::ptr::null_mut(), // lpRawData
        );
    }
}

/// Convert a UTF-8 `&str` to a null-terminated wide (UTF-16) string.
fn to_wide(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}
