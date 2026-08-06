//! Cross-platform system tray (Windows taskbar / macOS menu bar / Linux
//! StatusNotifier). Menu items:
//!
//! * **OpenChromecast** (title)
//! * **Start with system** — toggles launch-at-login (auto-launch)
//! * **Exit** — requests shutdown of the Cast receiver and quits
//!
//! The tray owns the winit event loop on the main thread (required on macOS);
//! the receiver itself runs on a background tokio runtime.

use crate::config::Cli;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::Notify;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use tracing::{error, info};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::WindowId;

const ID_START: &str = "start-with-system";
const ID_EXIT: &str = "exit";

/// Run the tray + receiver. Blocks until the user picks Exit.
pub fn run(cli: Cli) -> Result<()> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = TrayApp {
        cli,
        shutdown: Arc::new(Notify::new()),
        tray: None,
        start_item: None,
        auto: None,
        started: false,
        exit_requested: false,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

struct TrayApp {
    cli: Cli,
    shutdown: Arc<Notify>,
    tray: Option<TrayIcon>,
    start_item: Option<CheckMenuItem>,
    auto: Option<auto_launch::AutoLaunch>,
    started: bool,
    exit_requested: bool,
}

impl ApplicationHandler for TrayApp {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {
        if self.started {
            return;
        }
        self.started = true;
        if let Err(e) = self.setup() {
            error!("failed to start tray + receiver: {e:#}");
        }
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        _event: WindowEvent,
    ) {
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.exit_requested {
            event_loop.exit();
            return;
        }
        // Tray-icon delivers menu clicks on a global channel; poll it here.
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            self.handle_menu(event);
        }
    }
}

impl TrayApp {
    fn setup(&mut self) -> Result<()> {
        self.auto = build_auto_launch();

        // --- Menu ---
        let menu = Menu::new();
        let title = MenuItem::with_id("title", "OpenChromecast", false, None);
        let start_checked = self
            .auto
            .as_ref()
            .map(|a| a.is_enabled().unwrap_or(false))
            .unwrap_or(false);
        let start_item =
            CheckMenuItem::with_id(ID_START, "Start with system", true, start_checked, None);
        let exit_item = MenuItem::with_id(ID_EXIT, "Exit", true, None);
        menu.append_items(&[
            &title,
            &PredefinedMenuItem::separator(),
            &start_item,
            &PredefinedMenuItem::separator(),
            &exit_item,
        ])?;
        self.start_item = Some(start_item);

        // --- Tray icon ---
        let builder = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("OpenChromecast")
            .with_icon(make_icon());
        // macOS menu-bar icons are monochrome templates; the system recolours
        // them to match the menu bar (light/dark).
        #[cfg(target_os = "macos")]
        let builder = builder.with_icon_as_template(true);
        let tray = builder.build()?;
        self.tray = Some(tray);

        // --- Receiver on a background tokio runtime ---
        let cli = self.cli.clone();
        let shutdown = self.shutdown.clone();
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Runtime::new() {
                Ok(r) => r,
                Err(e) => {
                    error!("failed to start tokio runtime: {e}");
                    return;
                }
            };
            if let Err(e) = rt.block_on(crate::run_receiver(cli, shutdown)) {
                error!("receiver exited with error: {e:#}");
            }
        });

        info!("tray + receiver started (pid {})", std::process::id());
        Ok(())
    }

    fn handle_menu(&mut self, event: MenuEvent) {
        match event.id.0.as_str() {
            ID_START => {
                if let Some(a) = &self.auto {
                    let enable = !a.is_enabled().unwrap_or(false);
                    let result = if enable { a.enable() } else { a.disable() };
                    if let Err(e) = result {
                        error!("autostart toggle failed: {e}");
                    }
                    if let Some(item) = &self.start_item {
                        item.set_checked(enable);
                    }
                    info!("start with system: {enable}");
                }
            }
            ID_EXIT => {
                info!("exit requested");
                self.shutdown.notify_waiters();
                self.exit_requested = true;
            }
            other => info!("unhandled menu item: {other}"),
        }
    }
}

/// Build an auto-launch handle from the current executable, preserving the
/// process args (minus `--no-tray` so a login start still shows the tray).
fn build_auto_launch() -> Option<auto_launch::AutoLaunch> {
    let exe = std::env::current_exe().ok()?;
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--no-tray")
        .collect();
    auto_launch::AutoLaunchBuilder::new()
        .set_app_name("OpenChromecast")
        .set_app_path(&exe.to_string_lossy())
        .set_args(&args)
        .build()
        .ok()
}

/// The colorful "cast dot": a blue circle with a punched white ring. Used as
/// the Windows/Linux tray icon and as the packaged `.app` icon.
pub fn cast_icon_rgba(size: u32) -> Vec<u8> {
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let cx = size as f32 / 2.0;
    let cy = size as f32 / 2.0;
    let r = size as f32 * 0.42;
    let inner = size as f32 * 0.25;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            let idx = ((y * size + x) * 4) as usize;
            if dist <= r {
                // Outer filled circle; punch a white inner ring for contrast.
                rgba[idx] = 0x42;
                rgba[idx + 1] = 0x85;
                rgba[idx + 2] = 0xF4;
                rgba[idx + 3] = 255;
                if dist > inner {
                    // Blend the outer ring toward white. Use u32 math so the
                    // multiply can't overflow a u8 (debug builds panic).
                    let t = ((dist - inner) / (r - inner) * 255.0) as u32;
                    let blend = |c: u8| (255 - (255 - c as u32) * t / 255) as u8;
                    rgba[idx] = blend(rgba[idx]);
                    rgba[idx + 1] = blend(rgba[idx + 1]);
                    rgba[idx + 2] = blend(rgba[idx + 2]);
                }
            }
        }
    }
    rgba
}

/// macOS menu-bar icons must be monochrome "template" images (black + alpha);
/// the system recolours them for light/dark menu bars. Just a black disc.
#[cfg(target_os = "macos")]
fn macos_template_rgba(size: u32) -> Vec<u8> {
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let cx = size as f32 / 2.0;
    let cy = size as f32 / 2.0;
    let r = size as f32 * 0.40;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist <= r {
                let idx = ((y * size + x) * 4) as usize;
                rgba[idx] = 0;
                rgba[idx + 1] = 0;
                rgba[idx + 2] = 0;
                rgba[idx + 3] = 255;
            }
        }
    }
    rgba
}

fn make_icon() -> Icon {
    #[cfg(target_os = "macos")]
    let rgba = macos_template_rgba(64);
    #[cfg(not(target_os = "macos"))]
    let rgba = cast_icon_rgba(64);
    Icon::from_rgba(rgba, 64, 64).expect("valid RGBA icon")
}

/// Packaging helper: render the app icon to a 1024px PNG file.
pub fn write_icon_png(path: &std::path::Path) -> Result<()> {
    const SIZE: u32 = 1024;
    let rgba = cast_icon_rgba(SIZE);
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut encoder = png::Encoder::new(file, SIZE, SIZE);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&rgba)?;
    Ok(())
}

/// Packaging helper: write a multi-size `.ico` of the app icon (used as the
/// Windows executable's embedded icon resource, via `build.rs`/`winres`).
pub fn write_icon_ico(path: &std::path::Path) -> Result<()> {
    use ico::{IconDir, IconDirEntry, IconImage, ResourceType};
    let mut dir = IconDir::new(ResourceType::Icon);
    for size in [16u32, 24, 32, 48, 64, 128, 256] {
        let rgba = cast_icon_rgba(size);
        let img = IconImage::from_rgba_data(size, size, rgba);
        // encode() derives the entry size from the image (256 -> 0 on write).
        let entry = IconDirEntry::encode(&img)?;
        dir.add_entry(entry);
    }
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    dir.write(file)?;
    Ok(())
}
