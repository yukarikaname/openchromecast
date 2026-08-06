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
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use tracing::{error, info};
use winit::application::ApplicationHandler;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};

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
    start_item: Option<MenuItem>,
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
        let start_item = MenuItem::with_id(ID_START, "Start with system", true, None);
        if let Some(a) = &self.auto {
            start_item.set_checked(a.is_enabled().unwrap_or(false));
        }
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
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("OpenChromecast")
            .with_icon(make_icon())
            .build()?;
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
        .set_app_path(exe.to_string_lossy().to_string())
        .set_args(&args)
        .build()
        .ok()
}

/// A simple 64x64 blue "cast" dot used as the tray icon.
fn make_icon() -> Icon {
    const SIZE: u32 = 64;
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    let cx = SIZE as f32 / 2.0;
    let cy = SIZE as f32 / 2.0;
    let r = 27.0;
    let inner = 16.0;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            let idx = ((y * SIZE + x) * 4) as usize;
            if dist <= r {
                // Outer filled circle; punch a white inner ring for contrast.
                rgba[idx] = 0x42;
                rgba[idx + 1] = 0x85;
                rgba[idx + 2] = 0xF4;
                rgba[idx + 3] = 255;
                if dist > inner {
                    let ring = ((dist - inner) / (r - inner) * 255.0) as u8;
                    rgba[idx] = 255 - (255 - rgba[idx]) * ring / 255;
                    rgba[idx + 1] = 255 - (255 - rgba[idx + 1]) * ring / 255;
                    rgba[idx + 2] = 255 - (255 - rgba[idx + 2]) * ring / 255;
                }
            }
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).expect("valid RGBA icon")
}
