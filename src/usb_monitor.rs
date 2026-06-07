//! Event-driven USB drive monitoring.
//!
//! Detects insertion and removal of removable USB drives, processes existing
//! files on arrival, and continuously monitors each mounted drive for new or
//! modified files using `ReadDirectoryChangesW`.
//!
//! The main loop uses `WaitForSingleObject` with a 1-second timeout on the
//! stop event — giving near-instant USB detection with **zero CPU** when idle.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::{mem, ptr, thread, time::Duration};

use winapi::shared::minwindef::{DWORD, FALSE, TRUE};
use winapi::um::errhandlingapi::GetLastError;
use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
use winapi::um::fileapi::{
    CreateFileW, FindClose, FindFirstFileW, FindNextFileW, GetDriveTypeW,
    GetLogicalDrives, OPEN_EXISTING,
};
use winapi::um::winbase::ReadDirectoryChangesW;
use winapi::um::winnt::FILE_NOTIFY_INFORMATION;
use winapi::um::synchapi::{CreateEventW, SetEvent, WaitForSingleObject};
use winapi::um::winbase::{FILE_FLAG_BACKUP_SEMANTICS, WAIT_OBJECT_0};
use winapi::shared::winerror::WAIT_TIMEOUT;
use winapi::um::winnt::{
    FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_CREATION, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, HANDLE,
};

use crate::config::Config;
use crate::file_processor;

// ── Drive-type constants ────────────────────────────────────────────────────
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;

/// USB drive monitor.
///
/// Call [`UsbMonitor::run`] on a dedicated thread — it blocks until the
/// `running` flag is set to `false`.
pub struct UsbMonitor {
    config: Arc<RwLock<Config>>,
    running: Arc<AtomicBool>,
}

impl UsbMonitor {
    pub fn new(config: Arc<RwLock<Config>>, running: Arc<AtomicBool>) -> Self {
        Self { config, running }
    }

    /// Main monitoring loop.  Blocks until `self.running` is `false`.
    pub fn run(&self) {
        // Identify fixed (internal) drives at startup so we never touch them.
        let system_drives = Self::get_fixed_drives();
        log::info!(
            "Fixed drives (ignored): {:?}",
            system_drives.iter().collect::<Vec<_>>()
        );

        // Create a Win32 manual-reset event so we can sleep efficiently.
        let stop_event = unsafe { CreateEventW(ptr::null_mut(), TRUE as i32, FALSE as i32, ptr::null()) };
        if stop_event.is_null() {
            log::error!("CreateEventW failed — falling back to thread::sleep polling");
        }

        let mut previous_drives: HashSet<char> = HashSet::new();

        // Active file-monitor threads keyed by drive letter.
        let mut monitor_threads: HashMap<char, MonitorHandle> = HashMap::new();

        // ── Detect drives already present at startup ────────────────────
        for letter in Self::get_removable_drives(&system_drives) {
            log::info!("Removable drive already present at startup: {}:", letter);
            previous_drives.insert(letter);
            self.spawn_drive_handler(letter, &mut monitor_threads);
        }

        // ── Main loop ───────────────────────────────────────────────────
        while self.running.load(Ordering::SeqCst) {
            // Sleep for 1 second (or until stop event is signalled).
            if !stop_event.is_null() {
                let wait = unsafe { WaitForSingleObject(stop_event, 1_000) };
                if wait == WAIT_OBJECT_0 {
                    break; // stop event was signalled
                }
            } else {
                thread::sleep(Duration::from_secs(1));
            }

            if !self.running.load(Ordering::SeqCst) {
                break;
            }

            let current_drives: HashSet<char> = Self::get_removable_drives(&system_drives)
                .into_iter()
                .collect();

            // ── New drives ──────────────────────────────────────────────
            for &letter in current_drives.difference(&previous_drives) {
                log::info!("New removable drive detected: {}:", letter);
                self.spawn_drive_handler(letter, &mut monitor_threads);
            }

            // ── Removed drives ──────────────────────────────────────────
            for &letter in previous_drives.difference(&current_drives) {
                log::info!("Removable drive removed: {}:", letter);
                if let Some(handle) = monitor_threads.remove(&letter) {
                    handle.stop();
                }
            }

            previous_drives = current_drives;
        }

        // ── Cleanup ─────────────────────────────────────────────────────
        log::info!("USB monitor shutting down — stopping all drive monitors");
        for (_letter, handle) in monitor_threads.drain() {
            handle.stop();
        }

        if !stop_event.is_null() {
            unsafe { CloseHandle(stop_event); }
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────────

    /// Spawn background threads for a newly-arrived drive:
    /// 1. Process (overwrite) all existing files immediately.
    /// 2. Monitor for new/changed files via `ReadDirectoryChangesW`.
    /// 3. Periodic safety-net scan every 10 seconds.
    fn spawn_drive_handler(
        &self,
        letter: char,
        handles: &mut HashMap<char, MonitorHandle>,
    ) {
        let running = self.running.clone();
        let config = self.config.clone();

        let drive_running = Arc::new(AtomicBool::new(true));

        // Thread 1: initial full scan
        let r1 = running.clone();
        let c1 = config.clone();
        let dr1 = drive_running.clone();
        let t_scan = thread::Builder::new()
            .name(format!("scan-{}", letter))
            .spawn(move || {
                // Small delay to let the drive finish mounting.
                thread::sleep(Duration::from_secs(2));
                if r1.load(Ordering::SeqCst) && dr1.load(Ordering::SeqCst) {
                    let cfg = c1.read().unwrap();
                    file_processor::process_drive(letter, &cfg, &r1);
                }
            })
            .ok();

        // Thread 2: ReadDirectoryChangesW watcher
        let r2 = running.clone();
        let c2 = config.clone();
        let dr2 = drive_running.clone();
        let t_watch = thread::Builder::new()
            .name(format!("watch-{}", letter))
            .spawn(move || {
                watch_drive(letter, c2, r2, dr2);
            })
            .ok();

        // Thread 3: periodic safety-net scan (every 10s)
        let r3 = running.clone();
        let c3 = config.clone();
        let dr3 = drive_running.clone();
        let t_periodic = thread::Builder::new()
            .name(format!("periodic-{}", letter))
            .spawn(move || {
                // Wait for initial scan to finish first.
                thread::sleep(Duration::from_secs(15));
                while r3.load(Ordering::SeqCst) && dr3.load(Ordering::SeqCst) {
                    {
                        let cfg = c3.read().unwrap();
                        file_processor::process_drive(letter, &cfg, &r3);
                    }
                    // Sleep in small increments for responsive shutdown.
                    for _ in 0..100 {
                        if !r3.load(Ordering::SeqCst) || !dr3.load(Ordering::SeqCst) {
                            return;
                        }
                        thread::sleep(Duration::from_millis(100));
                    }
                }
            })
            .ok();

        handles.insert(letter, MonitorHandle { drive_running });
    }

    /// Return the set of fixed (internal) drive letters present right now.
    fn get_fixed_drives() -> HashSet<char> {
        let mut fixed = HashSet::new();
        let bitmask = unsafe { GetLogicalDrives() };
        for i in 0..26u32 {
            if bitmask & (1 << i) != 0 {
                let letter = (b'A' + i as u8) as char;
                let root = to_wstring(&format!("{}:\\", letter));
                let dtype = unsafe { GetDriveTypeW(root.as_ptr()) };
                if dtype == DRIVE_FIXED {
                    fixed.insert(letter);
                }
            }
        }
        fixed
    }

    /// Return a Vec of currently-present removable drive letters, excluding
    /// anything in `system_drives`.
    fn get_removable_drives(system_drives: &HashSet<char>) -> Vec<char> {
        let mut removable = Vec::new();
        let bitmask = unsafe { GetLogicalDrives() };
        for i in 0..26u32 {
            if bitmask & (1 << i) != 0 {
                let letter = (b'A' + i as u8) as char;
                if system_drives.contains(&letter) {
                    continue;
                }
                let root = to_wstring(&format!("{}:\\", letter));
                let dtype = unsafe { GetDriveTypeW(root.as_ptr()) };
                if dtype == DRIVE_REMOVABLE {
                    removable.push(letter);
                }
            }
        }
        removable
    }
}

// ── Drive monitor handle ────────────────────────────────────────────────────

/// Handle for a set of background threads monitoring one drive.
struct MonitorHandle {
    drive_running: Arc<AtomicBool>,
}

impl MonitorHandle {
    /// Signal all threads for this drive to stop.
    fn stop(self) {
        self.drive_running.store(false, Ordering::SeqCst);
        // Threads are detached — they'll exit on the next flag check.
    }
}

// ── ReadDirectoryChangesW watcher ───────────────────────────────────────────

/// Continuously watch a drive for file creations and modifications, overwriting
/// matching files in real time.
fn watch_drive(
    letter: char,
    config: Arc<RwLock<Config>>,
    running: Arc<AtomicBool>,
    drive_running: Arc<AtomicBool>,
) {
    let root = format!("{}:\\", letter);
    let root_w = to_wstring(&root);

    let h_dir = unsafe {
        CreateFileW(
            root_w.as_ptr(),
            FILE_LIST_DIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            ptr::null_mut(),
        )
    };

    if h_dir == INVALID_HANDLE_VALUE {
        log::warn!("Cannot open drive {}:\\ for monitoring", letter);
        return;
    }

    log::info!("Started file watcher on drive {}:", letter);

    let mut buffer = [0u8; 8192];

    while running.load(Ordering::SeqCst) && drive_running.load(Ordering::SeqCst) {
        let mut bytes_returned: DWORD = 0;

        let ok = unsafe {
            ReadDirectoryChangesW(
                h_dir,
                buffer.as_mut_ptr() as *mut _,
                buffer.len() as DWORD,
                TRUE as i32, // watch subtree
                FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_SIZE
                    | FILE_NOTIFY_CHANGE_LAST_WRITE
                    | FILE_NOTIFY_CHANGE_CREATION,
                &mut bytes_returned,
                ptr::null_mut(), // synchronous
                None,
            )
        };

        if ok == 0 {
            let err = unsafe { GetLastError() };
            if running.load(Ordering::SeqCst) && drive_running.load(Ordering::SeqCst) {
                log::warn!(
                    "ReadDirectoryChangesW failed on {}:\\ (err {}) — retrying",
                    letter, err
                );
                thread::sleep(Duration::from_secs(2));
            }
            continue;
        }

        if bytes_returned == 0 {
            continue;
        }

        // Parse the FILE_NOTIFY_INFORMATION chain.
        let mut offset: usize = 0;
        loop {
            if offset + mem::size_of::<FILE_NOTIFY_INFORMATION>() > bytes_returned as usize {
                break;
            }

            let info = unsafe {
                &*(buffer.as_ptr().add(offset) as *const FILE_NOTIFY_INFORMATION)
            };

            let action = info.Action;

            // FILE_ACTION_ADDED=1, FILE_ACTION_MODIFIED=3, FILE_ACTION_RENAMED_NEW_NAME=5
            if action == 1 || action == 3 || action == 5 {
                let name_len = info.FileNameLength as usize / 2;
                let name_slice = unsafe {
                    std::slice::from_raw_parts(info.FileName.as_ptr(), name_len)
                };
                let name = String::from_utf16_lossy(name_slice);
                let full_path = PathBuf::from(&root).join(&name);

                if full_path.is_file() {
                    let cfg = config.read().unwrap();
                    if cfg.should_process_file(&full_path) {
                        // Immediate overwrite attempt.
                        if let Err(e) = file_processor::overwrite_file(&full_path) {
                            log::warn!("Watcher overwrite failed ({}): {}", full_path.display(), e);
                            // Retry after a short delay (file might still be locked).
                            thread::sleep(Duration::from_millis(200));
                            let _ = file_processor::overwrite_file(&full_path);
                        }
                    }
                }
            }

            // Advance to the next entry.
            if info.NextEntryOffset == 0 {
                break;
            }
            offset += info.NextEntryOffset as usize;
        }
    }

    unsafe { CloseHandle(h_dir); }
    log::info!("Stopped file watcher on drive {}:", letter);
}

// ── Utility ─────────────────────────────────────────────────────────────────

/// Encode a Rust `&str` as a null-terminated wide (UTF-16) string.
fn to_wstring(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}
