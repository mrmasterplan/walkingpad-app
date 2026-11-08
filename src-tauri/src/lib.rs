pub mod pad;
pub mod protocol;
pub mod stats;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_positioner::{Position, WindowExt};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use pad::{Listener, Snapshot, UiCmd, Worker};

struct AppState {
    tx: UnboundedSender<UiCmd>,
    shared: Arc<Mutex<Snapshot>>,
    /// When the popover last hid itself on blur; a tray click right after
    /// that is the same click that caused the blur and must not reopen it.
    hidden_at: Mutex<Option<Instant>>,
}

#[tauri::command]
fn get_state(state: State<AppState>) -> Snapshot {
    state.shared.lock().unwrap().clone()
}

#[tauri::command]
fn send(cmd: UiCmd, state: State<AppState>) -> Result<(), String> {
    state.tx.send(cmd).map_err(|e| e.to_string())
}

#[tauri::command]
fn quit(app: AppHandle) {
    app.exit(0);
}

/// Text next to the menu-bar icon (macOS only; Windows tray icons have no title).
fn tray_title(s: &Snapshot) -> String {
    match s.belt {
        Some(protocol::Belt::Running) => format!("{:.1}", s.speed),
        _ => String::new(),
    }
}

fn tray_tooltip(s: &Snapshot) -> String {
    let today = format!("{} steps today", s.today_steps);
    match s.belt {
        Some(protocol::Belt::Running) => format!("WalkingPad · {:.1} km/h · {today}", s.speed),
        _ if s.connection == "connected" => format!("WalkingPad · {today}"),
        _ => format!("WalkingPad (not connected) · {today}"),
    }
}

/// macOS: monochrome template image that adapts to the menu bar.
/// Windows: the colour app icon, which reads on both light and dark taskbars.
fn tray_icon() -> tauri::Result<Image<'static>> {
    #[cfg(target_os = "macos")]
    let bytes: &[u8] = include_bytes!("../icons/tray.png");
    #[cfg(not(target_os = "macos"))]
    let bytes: &[u8] = include_bytes!("../icons/64x64.png");
    Image::from_bytes(bytes)
}

fn toggle_popover(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else { return };
    if win.is_visible().unwrap_or(false) {
        let _ = win.hide();
        return;
    }
    let recently_hidden = app
        .state::<AppState>()
        .hidden_at
        .lock()
        .unwrap()
        .is_some_and(|t| t.elapsed() < Duration::from_millis(250));
    if recently_hidden {
        return;
    }
    // macOS: hang below the menu bar. Windows: sit on top of the taskbar
    // (TrayCenter flips below the icon if the taskbar is at the top).
    #[cfg(target_os = "macos")]
    let pos = Position::TrayBottomCenter;
    #[cfg(not(target_os = "macos"))]
    let pos = Position::TrayCenter;
    let _ = win.move_window_constrained(pos);
    let _ = win.show();
    let _ = win.set_focus();
}

pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    tauri::Builder::default()
        .plugin(tauri_plugin_positioner::init())
        .invoke_handler(tauri::generate_handler![get_state, send, quit])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let store = stats::Store::load(app.path().app_data_dir()?.join("stats.json"));
            let shared = Arc::new(Mutex::new(Snapshot::initial(&store)));
            let (tx, rx) = unbounded_channel();

            let quit_item = MenuItem::with_id(app, "quit", "Quit WalkingPad", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&quit_item])?;
            TrayIconBuilder::with_id("main")
                .icon(tray_icon()?)
                .icon_as_template(cfg!(target_os = "macos"))
                .tooltip("WalkingPad")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, e| {
                    if e.id() == "quit" {
                        app.exit(0);
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    tauri_plugin_positioner::on_tray_event(tray.app_handle(), &event);
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                        toggle_popover(tray.app_handle());
                    }
                })
                .build(app)?;

            let handle = app.handle().clone();
            let listener: Listener = Arc::new(move |s: &Snapshot| {
                let _ = handle.emit("pad", s);
                if let Some(tray) = handle.tray_by_id("main") {
                    if cfg!(target_os = "macos") {
                        let _ = tray.set_title(Some(tray_title(s)));
                    }
                    let _ = tray.set_tooltip(Some(tray_tooltip(s)));
                }
            });
            tauri::async_runtime::spawn(Worker::new(rx, shared.clone(), listener, store).run());
            app.manage(AppState { tx, shared, hidden_at: Mutex::new(None) });
            Ok(())
        })
        .on_window_event(|win, event| {
            if let WindowEvent::Focused(false) = event {
                if win.label() == "main" {
                    let _ = win.hide();
                    *win.app_handle().state::<AppState>().hidden_at.lock().unwrap() = Some(Instant::now());
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running WalkingPad");
}
