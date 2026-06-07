//! Windows Service implementation.
//!
//! Registers with the Service Control Manager, reports status transitions,
//! and delegates real work to [`crate::usb_monitor::UsbMonitor`].
//!
//! The service accepts `STOP` and `SHUTDOWN` controls for clean exit on both
//! manual stop and Windows shutdown/reboot.

use std::ffi::OsString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
};

use crate::config::{self, Config, SERVICE_NAME};
use crate::usb_monitor::UsbMonitor;

// ---------------------------------------------------------------------------
// Service entry-point glue
// ---------------------------------------------------------------------------

define_windows_service!(ffi_service_main, service_main);

/// Called from `main()` — asks the SCM to dispatch to our service entry point.
///
/// Returns `Err` if we were **not** launched by the SCM (e.g. user double-
/// clicked the exe), which is the signal for `main()` to fall back to GUI mode.
pub fn run() -> Result<(), windows_service::Error> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

// ---------------------------------------------------------------------------
// Service main
// ---------------------------------------------------------------------------

/// Entry point invoked by the SCM after `StartServiceCtrlDispatcher` succeeds.
fn service_main(_arguments: Vec<OsString>) {
    if let Err(e) = run_service() {
        log::error!("Service terminated with error: {}", e);
    }
}

/// Core service logic — runs until a STOP or SHUTDOWN signal is received.
fn run_service() -> Result<(), Box<dyn std::error::Error>> {
    // Shared flag: flipped to `false` when the SCM asks us to stop.
    let running = Arc::new(AtomicBool::new(true));
    let running_for_handler = running.clone();

    // Register our control handler with the SCM.
    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                log::info!("Received {:?} control — initiating shutdown", control_event);
                running_for_handler.store(false, Ordering::SeqCst);
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = service_control_handler::register(SERVICE_NAME, event_handler)?;

    // ------ Report RUNNING ------
    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    // Load configuration.
    let config = Config::load();
    log::info!(
        "USB Security Guard service started — filter mode: {:?}",
        config.filter_mode
    );

    let config = Arc::new(RwLock::new(config));

    // Run the USB monitor — this blocks until `running` is set to false.
    let monitor = UsbMonitor::new(config, running.clone());
    monitor.run();

    // ------ Report STOPPED ------
    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    log::info!("USB Security Guard service stopped gracefully");
    Ok(())
}
