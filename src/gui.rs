//! Native Win32 GUI for USB Security Guard configuration.
//!
//! A clean, simple control panel built with raw Win32 API — no frameworks,
//! no web views, zero CPU when idle.  Uses the standard Windows message loop
//! and `SetTimer` for periodic status refresh.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::{mem, ptr};

use winapi::shared::basetsd::LONG_PTR;
use winapi::shared::minwindef::*;
use winapi::shared::windef::*;
use winapi::um::errhandlingapi::GetLastError;
use winapi::um::libloaderapi::GetModuleHandleW;
use winapi::um::wingdi::*;
use winapi::um::winuser::*;

use crate::config::{Config, FilterMode};
use crate::installer;

// ── Control IDs ─────────────────────────────────────────────────────────────

const ID_RADIO_ALL: u16 = 101;
const ID_RADIO_OFFICE: u16 = 102;
const ID_RADIO_PDF: u16 = 103;
const ID_RADIO_OFFICEPDF: u16 = 104;
const ID_BTN_APPLY: u16 = 201;
const ID_BTN_START: u16 = 202;
const ID_BTN_STOP: u16 = 203;
const ID_BTN_RESTART: u16 = 204;
const ID_BTN_INSTALL: u16 = 205;
const ID_BTN_UNINSTALL: u16 = 206;
const ID_STATIC_STATUS: u16 = 301;
const ID_STATIC_MODE: u16 = 302;
const ID_TIMER_REFRESH: usize = 1;

// ── Window dimensions ───────────────────────────────────────────────────────

const WIN_W: i32 = 480;
const WIN_H: i32 = 500;

// ── Application state stored in the window's user data ──────────────────────

struct GuiState {
    h_font: HFONT,
    h_font_title: HFONT,
    h_font_bold: HFONT,
    h_status: HWND,
    h_mode: HWND,
}

// ── Public entry point ──────────────────────────────────────────────────────

/// Create and run the configuration GUI.  Blocks until the window is closed.
pub fn run() {
    unsafe { gui_main() }
}

unsafe fn gui_main() {
    let h_instance = GetModuleHandleW(ptr::null());
    let class_name = w("USBSecurityGuardGUI");

    let wc = WNDCLASSEXW {
        cbSize: mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wnd_proc),
        cbClsExtra: 0,
        cbWndExtra: mem::size_of::<LONG_PTR>() as i32, // room for GuiState pointer
        hInstance: h_instance,
        hIcon: LoadIconW(ptr::null_mut(), IDI_SHIELD),
        hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
        hbrBackground: (COLOR_WINDOW + 1) as HBRUSH,
        lpszMenuName: ptr::null(),
        lpszClassName: class_name.as_ptr(),
        hIconSm: LoadIconW(ptr::null_mut(), IDI_SHIELD),
    };

    RegisterClassExW(&wc);

    // Centre the window on screen.
    let scr_w = GetSystemMetrics(SM_CXSCREEN);
    let scr_h = GetSystemMetrics(SM_CYSCREEN);
    let x = (scr_w - WIN_W) / 2;
    let y = (scr_h - WIN_H) / 2;

    let title = w("USB Security Guard");
    let hwnd = CreateWindowExW(
        0,
        class_name.as_ptr(),
        title.as_ptr(),
        WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
        x, y, WIN_W, WIN_H,
        ptr::null_mut(),
        ptr::null_mut(),
        h_instance,
        ptr::null_mut(),
    );

    if hwnd.is_null() {
        return;
    }

    ShowWindow(hwnd, SW_SHOW);
    UpdateWindow(hwnd);

    // Message loop.
    let mut msg: MSG = mem::zeroed();
    while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

// ── Window procedure ────────────────────────────────────────────────────────

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            on_create(hwnd);
            0
        }
        WM_COMMAND => {
            on_command(hwnd, LOWORD(wparam as DWORD), HIWORD(wparam as DWORD));
            0
        }
        WM_TIMER => {
            if wparam == ID_TIMER_REFRESH {
                refresh_status(hwnd);
            }
            0
        }
        WM_CTLCOLORSTATIC => {
            let hdc = wparam as HDC;
            let hwnd_ctl = lparam as HWND;
            let state_ptr = GetWindowLongPtrW(hwnd, 0) as *mut GuiState;
            
            if !state_ptr.is_null() {
                let state = &*state_ptr;
                if hwnd_ctl == state.h_status {
                    // Check if running or not running to colorize the text
                    let is_running = installer::is_running();
                    SetBkMode(hdc, TRANSPARENT as i32);
                    if is_running {
                        SetTextColor(hdc, RGB(0, 153, 0)); // Green
                    } else {
                        SetTextColor(hdc, RGB(204, 0, 0)); // Red
                    }
                    return GetSysColorBrush(COLOR_WINDOW) as LRESULT;
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_CLOSE => {
            let res = MessageBoxW(
                hwnd,
                w("Close the controller?\n\nThe USB security service will keep running in the background.").as_ptr(),
                w("USB Security Guard").as_ptr(),
                MB_YESNO | MB_ICONQUESTION,
            );
            if res == IDYES {
                DestroyWindow(hwnd);
            }
            0
        }
        WM_DESTROY => {
            // Clean up fonts.
            let state_ptr = GetWindowLongPtrW(hwnd, 0) as *mut GuiState;
            if !state_ptr.is_null() {
                let state = Box::from_raw(state_ptr);
                DeleteObject(state.h_font as *mut _);
                DeleteObject(state.h_font_title as *mut _);
                DeleteObject(state.h_font_bold as *mut _);
            }
            KillTimer(hwnd, ID_TIMER_REFRESH);
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

// ── WM_CREATE ───────────────────────────────────────────────────────────────

unsafe fn on_create(hwnd: HWND) {
    let h_inst = GetModuleHandleW(ptr::null());

    // ── Fonts ───────────────────────────────────────────────────────────
    let h_font = create_font("Segoe UI", 16, false);
    let h_font_title = create_font("Segoe UI", 26, true);
    let h_font_bold = create_font("Segoe UI", 18, true);

    let mut y: i32 = 15;
    let lm: i32 = 25; // left margin
    let cw: i32 = WIN_W - 60; // content width

    // ── Title ───────────────────────────────────────────────────────────
    let h = label(hwnd, h_inst, "USB Security Guard", lm, y, cw, 35, 0);
    set_font(h, h_font_title);
    y += 45;

    // ── Status line ─────────────────────────────────────────────────────
    let h_status = label(hwnd, h_inst, "PROTECTION: CHECKING...", lm, y, cw, 25, ID_STATIC_STATUS);
    set_font(h_status, h_font_bold);
    y += 30;

    // ── Mode display ────────────────────────────────────────────────────
    let h_mode = label(hwnd, h_inst, "Active Mode: Loading...", lm, y, cw, 22, ID_STATIC_MODE);
    set_font(h_mode, h_font_bold);
    y += 40;

    // ── Security Mode group ─────────────────────────────────────────────
    let gb = create_groupbox(hwnd, h_inst, " Configure Security Mode ", lm, y, cw, 150);
    set_font(gb, h_font_bold);

    let config = Config::load();
    let current = config.filter_mode;

    let ry = y + 25;
    let radios: &[(u16, &str, FilterMode)] = &[
        (ID_RADIO_ALL, "All Files (MAXIMUM DANGER)", FilterMode::All),
        (ID_RADIO_OFFICE, "Office Files Only", FilterMode::Office),
        (ID_RADIO_PDF, "PDF Files Only", FilterMode::Pdf),
        (ID_RADIO_OFFICEPDF, "Office + PDF Files (RECOMMENDED)", FilterMode::OfficePdf),
    ];
    for (i, &(id, text, mode)) in radios.iter().enumerate() {
        let r = radio(hwnd, h_inst, text, lm + 20, ry + (i as i32) * 28, cw - 40, 24, id);
        set_font(r, h_font);
        if mode == current {
            SendMessageW(r, BM_SETCHECK, BST_CHECKED as WPARAM, 0);
        }
    }
    y += 165;

    // ── Apply button ────────────────────────────────────────────────────
    let b = button(hwnd, h_inst, "Apply Configuration", lm, y, cw, 38, ID_BTN_APPLY);
    set_font(b, h_font_bold);
    y += 55;

    // ── Service Control group ───────────────────────────────────────────
    let gb2 = create_groupbox(hwnd, h_inst, " Service Power Controls ", lm, y, cw, 100);
    set_font(gb2, h_font_bold);

    let by = y + 25;
    let bw = (cw - 30) / 3;
    for (i, &(id, text)) in [
        (ID_BTN_START, "Start"),
        (ID_BTN_STOP, "Stop"),
        (ID_BTN_RESTART, "Restart"),
    ].iter().enumerate() {
        let b = button(hwnd, h_inst, text, lm + 10 + (i as i32) * (bw + 5), by, bw, 30, id);
        set_font(b, h_font);
    }

    let by2 = by + 35;
    let bw2 = (cw - 20) / 2;
    let bi = button(hwnd, h_inst, "Install Service", lm + 10, by2, bw2 - 5, 30, ID_BTN_INSTALL);
    set_font(bi, h_font);
    let bu = button(hwnd, h_inst, "Uninstall Service", lm + 10 + bw2 + 5, by2, bw2 - 5, 30, ID_BTN_UNINSTALL);
    set_font(bu, h_font);

    // ── Store state ─────────────────────────────────────────────────────
    let state = Box::new(GuiState {
        h_font,
        h_font_title,
        h_font_bold,
        h_status,
        h_mode,
    });
    SetWindowLongPtrW(hwnd, 0, Box::into_raw(state) as LONG_PTR);

    // ── Start auto-refresh timer (every 3 seconds) ──────────────────────
    SetTimer(hwnd, ID_TIMER_REFRESH, 3_000, None);

    // ── Initial status refresh ──────────────────────────────────────────
    refresh_status(hwnd);
}

// ── WM_COMMAND handler ──────────────────────────────────────────────────────

unsafe fn on_command(hwnd: HWND, id: u16, _notification: u16) {
    match id {
        ID_BTN_APPLY => cmd_apply(hwnd),
        ID_BTN_START => {
            match installer::start_service() {
                Ok(()) => msgbox(hwnd, "Service started.", "USB Security Guard", MB_ICONINFORMATION),
                Err(e) => msgbox(hwnd, &format!("Failed to start:\n{}", e), "Error", MB_ICONERROR),
            }
            refresh_status(hwnd);
        }
        ID_BTN_STOP => {
            match installer::stop_service() {
                Ok(()) => msgbox(hwnd, "Service stopped.\n\nYour system is now unprotected.", "Warning", MB_ICONWARNING),
                Err(e) => msgbox(hwnd, &format!("Failed to stop:\n{}", e), "Error", MB_ICONERROR),
            }
            refresh_status(hwnd);
        }
        ID_BTN_RESTART => {
            match installer::restart_service() {
                Ok(()) => msgbox(hwnd, "Service restarted.", "USB Security Guard", MB_ICONINFORMATION),
                Err(e) => msgbox(hwnd, &format!("Failed to restart:\n{}", e), "Error", MB_ICONERROR),
            }
            refresh_status(hwnd);
        }
        ID_BTN_INSTALL => {
            match installer::install() {
                Ok(()) => msgbox(hwnd, "Service installed and started successfully.", "USB Security Guard", MB_ICONINFORMATION),
                Err(e) => msgbox(hwnd, &format!("Installation failed:\n{}", e), "Error", MB_ICONERROR),
            }
            refresh_status(hwnd);
        }
        ID_BTN_UNINSTALL => {
            let res = MessageBoxW(
                hwnd,
                w("Completely uninstall the USB Security Service?\n\nThis will remove the service, scheduled tasks, and registry entries.\n\nYour system will be UNPROTECTED.").as_ptr(),
                w("Confirm Uninstall").as_ptr(),
                MB_YESNO | MB_ICONWARNING,
            );
            if res == IDYES {
                match installer::uninstall() {
                    Ok(()) => msgbox(hwnd, "Service uninstalled completely.", "USB Security Guard", MB_ICONINFORMATION),
                    Err(e) => msgbox(hwnd, &format!("Uninstall failed:\n{}", e), "Error", MB_ICONERROR),
                }
                refresh_status(hwnd);
            }
        }
        _ => {}
    }
}

/// Apply button: read selected radio, save config, restart service.
unsafe fn cmd_apply(hwnd: HWND) {
    let mode = get_selected_mode(hwnd);

    // Save the new filter mode directly to the config file.
    if let Err(e) = save_mode(mode) {
        msgbox(hwnd, &format!("Failed to save config:\n{}", e), "Error", MB_ICONERROR);
        return;
    }

    // Restart service to pick up new config.
    match installer::restart_service() {
        Ok(()) => msgbox(
            hwnd,
            &format!("Configuration applied!\n\nMode: {}\nService restarted.", mode.display_name()),
            "USB Security Guard",
            MB_ICONINFORMATION,
        ),
        Err(e) => msgbox(
            hwnd,
            &format!("Config saved but restart failed:\n{}", e),
            "Warning",
            MB_ICONWARNING,
        ),
    }
    refresh_status(hwnd);
}

/// Write a new filter mode to the config file.
fn save_mode(mode: FilterMode) -> std::io::Result<()> {
    let path = Config::config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = format!(
        "; USB File Overwriter Configuration\n; Valid modes: all, office, pdf, office_pdf\n[FileFilter]\nmode = {}\n",
        mode.as_str()
    );
    std::fs::write(&path, content)
}

/// Read which radio button is currently selected.
unsafe fn get_selected_mode(hwnd: HWND) -> FilterMode {
    let check = |id: u16| -> bool {
        let ctl = GetDlgItem(hwnd, id as i32);
        if ctl.is_null() { return false; }
        SendMessageW(ctl, BM_GETCHECK, 0, 0) == BST_CHECKED as isize
    };

    if check(ID_RADIO_OFFICE) { FilterMode::Office }
    else if check(ID_RADIO_PDF) { FilterMode::Pdf }
    else if check(ID_RADIO_OFFICEPDF) { FilterMode::OfficePdf }
    else { FilterMode::All }
}

// ── Status refresh ──────────────────────────────────────────────────────────

unsafe fn refresh_status(hwnd: HWND) {
    let state_ptr = GetWindowLongPtrW(hwnd, 0) as *const GuiState;
    if state_ptr.is_null() {
        return;
    }
    let state = &*state_ptr;

    // Service status
    let is_running = installer::is_running();
    let status_text = if is_running {
        "PROTECTION: ACTIVE \u{25CF}"
    } else {
        "PROTECTION: DISABLED \u{25CB}"
    };
    set_text(state.h_status, status_text);

    // Current mode
    let config = Config::load();
    let mode_text = format!("Target: {}", config.filter_mode.display_name());
    set_text(state.h_mode, &mode_text);
    
    // Invalidate the rect of the status label to force a repaint with the correct color
    InvalidateRect(state.h_status, ptr::null(), TRUE);
}

// ── Win32 helper functions ──────────────────────────────────────────────────

unsafe fn label(parent: HWND, inst: HINSTANCE, text: &str, x: i32, y: i32, width: i32, h: i32, id: u16) -> HWND {
    let cls = w("STATIC");
    let txt = w(text);
    CreateWindowExW(0, cls.as_ptr(), txt.as_ptr(), WS_CHILD | WS_VISIBLE | SS_LEFT, x, y, width, h, parent, id as usize as HMENU, inst, ptr::null_mut())
}

unsafe fn button(parent: HWND, inst: HINSTANCE, text: &str, x: i32, y: i32, bw: i32, bh: i32, id: u16) -> HWND {
    let cls = w("BUTTON");
    let txt = w(text);
    CreateWindowExW(0, cls.as_ptr(), txt.as_ptr(), WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON, x, y, bw, bh, parent, id as usize as HMENU, inst, ptr::null_mut())
}

unsafe fn radio(parent: HWND, inst: HINSTANCE, text: &str, x: i32, y: i32, rw: i32, rh: i32, id: u16) -> HWND {
    let cls = w("BUTTON");
    let txt = w(text);
    let style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_AUTORADIOBUTTON;
    CreateWindowExW(0, cls.as_ptr(), txt.as_ptr(), style, x, y, rw, rh, parent, id as usize as HMENU, inst, ptr::null_mut())
}

unsafe fn create_groupbox(parent: HWND, inst: HINSTANCE, text: &str, x: i32, y: i32, gw: i32, gh: i32) -> HWND {
    let cls = w("BUTTON");
    let txt = w(text);
    CreateWindowExW(0, cls.as_ptr(), txt.as_ptr(), WS_CHILD | WS_VISIBLE | BS_GROUPBOX, x, y, gw, gh, parent, 0 as HMENU, inst, ptr::null_mut())
}

unsafe fn create_font(face: &str, size: i32, bold: bool) -> HFONT {
    let face_w = w(face);
    CreateFontW(
        -size, 0, 0, 0,
        if bold { FW_BOLD } else { FW_NORMAL },
        0, 0, 0,
        DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
        CLEARTYPE_QUALITY, DEFAULT_PITCH | FF_DONTCARE,
        face_w.as_ptr(),
    )
}

unsafe fn set_font(hwnd: HWND, font: HFONT) {
    SendMessageW(hwnd, WM_SETFONT, font as WPARAM, TRUE as LPARAM);
}

unsafe fn set_text(hwnd: HWND, text: &str) {
    let t = w(text);
    SetWindowTextW(hwnd, t.as_ptr());
}

unsafe fn msgbox(hwnd: HWND, text: &str, title: &str, flags: UINT) {
    MessageBoxW(hwnd, w(text).as_ptr(), w(title).as_ptr(), flags);
}

/// Encode `&str` → null-terminated UTF-16.
fn w(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}
