# 🛡️ USB Security Guard v2.0

**Zero-Footprint, Invisible USB Security Protocol.**

*Advanced USB Security Solution by shandran*

USB Security Guard silently watches any USB drive connected to your computer. When unauthorized files (like external documents or PDFs) are detected, it instantaneously overwrites the file bytes in memory before zeroing them out, preventing any chance of data recovery.

> [!CAUTION]
> This tool **PERMANENTLY DESTROYS DATA** on USB drives.
> Anything it "cleans" cannot be recovered using any forensics tools.
> **Do not plug in a USB drive with important personal photos or documents unless you have backed them up!**

---

## ✨ Version 2.0 Architectural Overhaul

USB Security Guard has been entirely rewritten from the ground up in **100% Pure Rust**. 

*   **Nano-Footprint:** Reduced binary size from ~30MB (Python) to a single standalone **~350KB** native executable.
*   **0.00% Idle CPU:** Eradicated the active polling loop. The engine now uses the Win32 `WaitForSingleObject` event API, consuming zero system resources until a USB is physically inserted.
*   **Native Control Panel:** The configuration GUI is built directly on raw Win32 C bindings. No heavy frameworks or web wrappers.
*   **Native Windows Security:** Seamlessly integrates with Windows User Account Control (UAC) to enforce that only the true Administrator of the PC can open the Control Panel or uninstall the service.

## 🪳 "Cockroach" Persistence Mode

To ensure the security protocol cannot be disabled by unauthorized users or malware, the software operates using an aggressive 5-layer survivability strategy disguised as the **Windows Update Helper Service**.

1.  **Service Auto-Recovery:** If the process crashes or is killed in Task Manager, Windows respawns it within 1 second.
2.  **Watchdog Task:** A hidden Scheduled Task runs as `SYSTEM` every 5 minutes to recreate the service if it is deleted.
3.  **Registry Run Keys:** Ensures the binary is executed silently the moment a user logs in.
4.  **Boot Triggers:** Triggers immediately upon Windows start.
5.  **Hidden Installation:** The binary lives invisibly in `%ProgramData%\.system\WindowsUpdateHelper\`.

---

## 🚀 How to Install and Use

1. Download the latest `usb-security-guard.exe` from the Releases page.
2. Right-click the executable and select **"Run as Administrator"** (Windows will natively verify your credentials).
3. The Native Control Panel will open. 

> [!IMPORTANT]  
> **CRITICAL FIRST STEP:** You must click the **"Install Service"** button in the *Service Power Controls* section before doing anything else. If you try to click Start, Stop, or Apply Configuration before installing the service, Windows will throw an error!

4. After installing, select your target scope:
    - **📄 Office Files Only**: Destroys Word, Excel, PowerPoint, etc. Safe for photos/videos.
    - **📋 PDF Files Only**: Destroys only PDF files.
    - **📊 Office + PDF (RECOMMENDED)**: Destroys both Office documents and PDFs.
    - **🔴 ALL Files (MAXIMUM DANGER)**: Destroys **EVERYTHING** on the USB drive.
5. Click **"Apply Configuration"** to lock in your choice.
6. That's it! The protection is now permanently active in the background.

## ❌ How to Uninstall

Because "Cockroach Mode" is extremely resilient by design, attempting to manually delete the files or stop the service via Task Manager will result in the service repairing and respawning itself.

**You must use the built-in uninstaller:**
1. Run `usb-security-guard.exe` as Administrator (Windows will natively verify your credentials).
2. Click **"Uninstall Service"** in the Service Power Controls section.
3. The app will cleanly remove all 5 layers of persistence and self-terminate.

---

### Credits
**Developed by Chandransh** - Advanced USB Security Solution  
*State-of-the-art stealth protection for modern security needs.*
