/// Build script for USB Security Guard.
///
/// Embeds the Windows application manifest (`resources/app.manifest`) into the
/// compiled binary. The manifest requests:
/// - Administrator elevation (requireAdministrator)
/// - DPI awareness
/// - Windows 10/11 compatibility GUIDs
fn main() {
    #[cfg(target_os = "windows")]
    {
        let mut res = winres::WindowsResource::new();
        res.set_manifest_file("resources/app.manifest");
        res.compile().expect("Failed to compile Windows resources");
    }
}
