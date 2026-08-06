// Build script: on Windows, embed assets/icon.ico as the executable's icon so
// the .exe (and its taskbar entry) show the OpenChromecast cast-dot icon.
fn main() {
    #[cfg(windows)]
    {
        // Guard so `cargo build` still works before the asset exists (it is
        // generated once via `openchromecast --dump-icon-ico assets/icon.ico`).
        if std::path::Path::new("assets/icon.ico").exists() {
            embed_resource::compile("app.rc", embed_resource::NONE);
        }
    }
}
