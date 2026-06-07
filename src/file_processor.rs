//! High-performance file overwriting engine.
//!
//! Walks a USB drive recursively and overwrites files matching the configured
//! filter with pseudorandom data, then zeros. Uses a stack-allocated buffer
//! and `fastrand` for speed — this is not cryptographic randomness, but is
//! more than sufficient to make file recovery impractical.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::config::{Config, MAX_FILE_SIZE, OVERWRITE_BUFFER_SIZE};

/// Directories that must never be processed — these are Windows system
/// directories present on most drives and contain protected OS metadata.
const SKIP_DIRS: &[&str] = &[
    "System Volume Information",
    "$RECYCLE.BIN",
    "RECYCLER",
];

/// Walk all files on `drive_letter:\` recursively and overwrite those matching
/// the config filter.
///
/// Respects the `running` flag — checks it after every file and bails out
/// immediately if set to `false` (service shutdown requested).
pub fn process_drive(drive_letter: char, config: &Config, running: &AtomicBool) {
    let root = format!("{}:\\", drive_letter);
    log::info!("Starting file processing on drive {}", root);

    let count = walk_directory(Path::new(&root), config, running);
    if running.load(Ordering::SeqCst) {
        log::info!(
            "Completed processing drive {} — {} files overwritten",
            root, count
        );
    } else {
        log::info!(
            "Processing on drive {} interrupted by shutdown — {} files overwritten so far",
            root, count
        );
    }
}

/// Recursively walk `dir`, overwriting matching files. Returns the number of
/// files successfully overwritten.
fn walk_directory(dir: &Path, config: &Config, running: &AtomicBool) -> u64 {
    // Bail early if shutdown was requested.
    if !running.load(Ordering::SeqCst) {
        return 0;
    }

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            log::warn!("Cannot read directory {}: {}", dir.display(), e);
            return 0;
        }
    };

    let mut count: u64 = 0;

    for entry in entries {
        // Check running flag on every iteration for responsive shutdown.
        if !running.load(Ordering::SeqCst) {
            break;
        }

        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                log::warn!("Error reading entry in {}: {}", dir.display(), e);
                continue;
            }
        };

        let path = entry.path();
        let file_name = match entry.file_name().to_str() {
            Some(name) => name.to_owned(),
            None => {
                log::warn!("Skipping non-UTF-8 filename: {:?}", entry.file_name());
                continue;
            }
        };

        // Skip hidden files/directories (names starting with '.').
        if file_name.starts_with('.') {
            continue;
        }

        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                log::warn!("Cannot stat {}: {}", path.display(), e);
                continue;
            }
        };

        if metadata.is_dir() {
            // Skip well-known system directories.
            if SKIP_DIRS.iter().any(|&s| file_name.eq_ignore_ascii_case(s)) {
                log::debug!("Skipping system directory: {}", path.display());
                continue;
            }
            count += walk_directory(&path, config, running);
        } else if metadata.is_file() {
            // Apply the extension filter from config.
            if !config.should_process_file(&path) {
                continue;
            }

            let file_size = metadata.len();
            if file_size > MAX_FILE_SIZE as u64 {
                log::info!(
                    "Skipping {} — size {} exceeds MAX_FILE_SIZE ({})",
                    path.display(),
                    file_size,
                    MAX_FILE_SIZE
                );
                continue;
            }

            match overwrite_file(&path) {
                Ok(()) => {
                    log::info!("Overwritten: {}", path.display());
                    count += 1;
                }
                Err(e) => {
                    log::error!("Failed to overwrite {}: {}", path.display(), e);
                }
            }
        }
    }

    count
}

/// Overwrite a single file with pseudorandom data, then zeros.
///
/// # Strategy
/// 1. Read the file's current size.
/// 2. If the file is read-only, strip that attribute and retry.
/// 3. **Pass 1**: Overwrite the entire file with pseudorandom bytes (via
///    `fastrand::fill`).
/// 4. **Pass 2**: Overwrite again with zeros for thoroughness.
/// 5. Flush to disk with `sync_all()` to guarantee writes are committed.
///
/// A single `[u8; OVERWRITE_BUFFER_SIZE]` buffer is allocated on the stack —
/// no heap allocation per file.
pub fn overwrite_file(path: &Path) -> std::io::Result<()> {
    let file_size = fs::metadata(path)?.len();
    if file_size == 0 {
        // Nothing to overwrite.
        return Ok(());
    }

    // Try to remove read-only attribute if present, so we can open for writing.
    try_remove_readonly(path);

    // ---- Pass 1: pseudorandom overwrite ----
    overwrite_pass(path, file_size, FillStrategy::Random)?;

    // ---- Pass 2: zero overwrite ----
    overwrite_pass(path, file_size, FillStrategy::Zeros)?;

    Ok(())
}

/// What to fill the buffer with for a given overwrite pass.
#[derive(Clone, Copy)]
enum FillStrategy {
    Random,
    Zeros,
}

/// Execute one overwrite pass on `path` with the given fill strategy.
fn overwrite_pass(path: &Path, file_size: u64, strategy: FillStrategy) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .truncate(false) // Overwrite in-place, don't change file size.
        .open(path)?;

    // Stack-allocated buffer — no heap allocation.
    let mut buf = [0u8; OVERWRITE_BUFFER_SIZE];
    let mut remaining = file_size;

    while remaining > 0 {
        let chunk = remaining.min(buf.len() as u64) as usize;

        match strategy {
            FillStrategy::Random => fastrand::fill(&mut buf[..chunk]),
            FillStrategy::Zeros => {
                // buf is already zero-initialised on first use, but we must
                // re-zero it after a Random pass may have been the previous
                // caller. Because this is a separate function call with its
                // own `buf`, it's always zero here — no extra work needed.
            }
        }

        file.write_all(&buf[..chunk])?;
        remaining -= chunk as u64;
    }

    file.sync_all()?;
    Ok(())
}

/// Best-effort attempt to strip the read-only attribute from a file so we
/// can open it for writing. Failures are silently ignored — the subsequent
/// open will produce the real error.
fn try_remove_readonly(path: &Path) {
    if let Ok(metadata) = fs::metadata(path) {
        let mut perms = metadata.permissions();
        if perms.readonly() {
            perms.set_readonly(false);
            if let Err(e) = fs::set_permissions(path, perms) {
                log::warn!(
                    "Could not remove read-only attribute from {}: {}",
                    path.display(),
                    e
                );
            }
        }
    }
}
