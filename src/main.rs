//! USB Security Guard — entry point.
//!
//! A single binary that operates in three modes:
//!
//! 1. **Windows Service** — started by the Service Control Manager (SCM), runs
//!    headless in the background monitoring USB drives.
//! 2. **GUI controller** — launched by the user (double-click or `--gui`) for
//!    configuration.
//! 3. **CLI installer** — `--install` / `--uninstall` / `--start` / `--stop` /
//!    `--status` for managing the service from an elevated command prompt.
//!
//! When invoked without arguments the binary first attempts to register with
//! the SCM (the path taken when Windows starts the service). If that fails it
//! assumes a user launched the exe directly and opens the GUI instead.

#![windows_subsystem = "windows"]

mod config;
mod file_processor;
mod gui;
mod installer;
mod logger;
mod service;
mod usb_monitor;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Initialise the Windows Event Log logger before anything else.
    logger::init(config::SERVICE_NAME);

    if args.len() > 1 {
        match args[1].as_str() {
            // ── Service mode (started by SCM with explicit flag) ─────
            "--service" | "service" => {
                if let Err(e) = service::run() {
                    log::error!("Service failed: {}", e);
                    std::process::exit(1);
                }
            }

            // ── Installer CLI ────────────────────────────────────────
            "--install" | "install" => {
                println!("Installing USB Security Guard service...");
                match installer::install() {
                    Ok(()) => println!("Service installed successfully."),
                    Err(e) => {
                        eprintln!("Installation failed: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            "--uninstall" | "uninstall" => {
                println!("Uninstalling USB Security Guard service...");
                match installer::uninstall() {
                    Ok(()) => println!("Service uninstalled successfully."),
                    Err(e) => {
                        eprintln!("Uninstallation failed: {}", e);
                        std::process::exit(1);
                    }
                }
            }

            // ── Service control ──────────────────────────────────────
            "--start" | "start" => match installer::start_service() {
                Ok(()) => println!("Service started."),
                Err(e) => eprintln!("Failed to start: {}", e),
            },
            "--stop" | "stop" => match installer::stop_service() {
                Ok(()) => println!("Service stopped."),
                Err(e) => eprintln!("Failed to stop: {}", e),
            },
            "--status" | "status" => match installer::query_service_status() {
                Ok(status) => println!("Service status: {}", status),
                Err(e) => eprintln!("Failed to query status: {}", e),
            },

            // ── GUI mode (explicit) ─────────────────────────────────
            "--gui" | "gui" => {
                gui::run();
            }

            // ── Unknown flag → default to GUI ───────────────────────
            _ => {
                gui::run();
            }
        }
    } else {
        // No arguments: try to run as a Windows Service first.
        // `service::run()` calls `StartServiceCtrlDispatcher` which will
        // succeed only when we were launched by the SCM. If it fails we
        // know a user double-clicked the exe, so open the GUI.
        match service::run() {
            Ok(()) => {}
            Err(_) => {
                // Not started by SCM — launch the GUI.
                gui::run();
            }
        }
    }
}
