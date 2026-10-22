mod autostart;
mod model;
mod systemd;
mod tray;
mod ui;

use adw::prelude::*;
use adw::{gio, glib};
use anyhow::{Context, Result};
use libadwaita as adw;
use model::{Controller, SharedState, APP_ID, APP_NAME};
use notify_rust::Notification;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const USAGE: &str = "\
services-tray - control systemd services from the system tray

Usage:
  services-tray          start the tray app
  services-tray --show   also open the Manage Services window
  services-tray --version
  services-tray --help

Click the tray icon to open the service menu, then use the Start, Restart and
Stop items of each service, or one of its web links. Add, edit or remove
services and their links from the same menu. System services may ask for
administrator authentication. Running it again while it runs opens the Manage
Services window.
";

/// Seconds without a tray host before the user is told that none is running.
const TRAY_NOTICE_DELAY: u32 = 30;
/// Registration attempts while a tray host is present but not yet answering.
const TRAY_ATTEMPTS: u32 = 5;

static QUIT_SIGNAL: AtomicBool = AtomicBool::new(false);

fn main() -> Result<()> {
    let mut show_on_start = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--help" | "-h" => {
                print!("{USAGE}");
                return Ok(());
            }
            "--show" => show_on_start = true,
            other => anyhow::bail!("unknown argument: {other}\n\n{USAGE}"),
        }
    }

    install_signal_handlers();

    let app = adw::Application::builder().application_id(APP_ID).build();
    app.register(gio::Cancellable::NONE)
        .context("cannot register with the session bus")?;
    let empty: [&str; 0] = [];
    if app.is_remote() {
        // Already running: that instance opens its Manage Services window, and
        // no second tray icon is created.
        app.run_with_args(&empty);
        return Ok(());
    }

    let services = match model::load_services() {
        Ok(services) => services,
        Err(error) => {
            notify_error(
                "Could not load Services Tray settings",
                &format!("{error:#}"),
            );
            show_on_start = true;
            Vec::new()
        }
    };

    let state: SharedState = Arc::default();
    state.lock().unwrap().services = services;
    let controller = Arc::new(Controller::new(state));
    controller.poll();

    // At login the session can start this app before the tray host is on the
    // bus, so the icon is created once the host appears instead of failing.
    let tray_controller = controller.clone();
    gio::bus_watch_name(
        gio::BusType::Session,
        tray::WATCHER_NAME,
        gio::BusNameWatcherFlags::NONE,
        move |_, _, _| start_tray(&tray_controller, 1),
        |_, _| {},
    );
    let notice_controller = controller.clone();
    glib::timeout_add_seconds_local_once(TRAY_NOTICE_DELAY, move || {
        if !notice_controller.has_tray() {
            notify_error(
                "No system tray found",
                "Services Tray is running and its icon will appear once a system tray is available. On GNOME, enable the AppIndicator extension.",
            );
        }
    });

    if show_on_start {
        controller.request_manage();
    }

    let poll_controller = controller.clone();
    std::thread::spawn(move || {
        while !poll_controller.should_quit() {
            if poll_controller.poll() {
                poll_controller.refresh_tray();
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });

    let activate_controller = controller.clone();
    app.connect_activate(move |app| {
        ui::present(app, activate_controller.clone());
    });

    app.run_with_args(&empty);
    controller.quit();
    Ok(())
}

fn start_tray(controller: &Arc<Controller>, attempt: u32) {
    if controller.has_tray() {
        return;
    }
    match tray::spawn(controller.clone()) {
        Ok(handle) => controller.set_tray(handle),
        Err(error) => {
            eprintln!("cannot create the tray icon (attempt {attempt}/{TRAY_ATTEMPTS}): {error}");
            if attempt < TRAY_ATTEMPTS {
                let controller = controller.clone();
                glib::timeout_add_seconds_local_once(2, move || {
                    start_tray(&controller, attempt + 1);
                });
            }
        }
    }
}

pub fn take_quit_signal() -> bool {
    QUIT_SIGNAL.swap(false, Ordering::SeqCst)
}

pub fn notify_error(summary: &str, body: &str) {
    let _ = Notification::new()
        .summary(summary)
        .body(body)
        .icon("dialog-error-symbolic")
        .appname(APP_NAME)
        .show();
}

extern "C" fn on_quit_signal(_signal: libc::c_int) {
    QUIT_SIGNAL.store(true, Ordering::SeqCst);
}

fn install_signal_handlers() {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_quit_signal as *const () as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        for signal in [libc::SIGTERM, libc::SIGINT] {
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}
