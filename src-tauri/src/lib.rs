// GlowAudio Desktop - Tauri backend entry point.
// Wires together the audio engine, the process monitor, global hotkey cycling
// and the floating HUD overlay window, and exposes commands to the React UI.

mod audio;
mod monitor;

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, State, WebviewUrl, WebviewWindowBuilder,
    WindowEvent,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use audio::AudioDevice;

const DEFAULT_HOTKEY: &str = "Ctrl+Shift+A";
const HUD_WIDTH: f64 = 340.0;
const HUD_HEIGHT: f64 = 98.0;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileEntry
{
    pub device_id: String,
    pub device_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfileItem
{
    pub app: String,
    pub device_id: String,
    pub device_name: String,
}

#[derive(Clone, Serialize)]
pub struct AutoSwitchPayload
{
    pub app: String,
    pub device_name: String,
}

#[derive(Clone, Serialize)]
struct HudPayload
{
    title: String,
    subtitle: String,
    volume: i32,
}

pub struct SharedState
{
    pub profiles: Mutex<HashMap<String, ProfileEntry>>,
    pub hotkey: Mutex<String>,
    pub last_detected: Mutex<Option<String>>,
    pub monitor_running: AtomicBool,
    pub config_dir: Mutex<PathBuf>,
}

impl SharedState
{
    fn profiles_path(&self) -> PathBuf
    {
        self.config_dir.lock().join("glow_profiles.json")
    }

    fn settings_path(&self) -> PathBuf
    {
        self.config_dir.lock().join("glow_settings.json")
    }

    fn save_profiles(&self)
    {
        let guard = self.profiles.lock();
        if let Ok(json) = serde_json::to_string_pretty(&*guard)
        {
            let _ = std::fs::write(self.profiles_path(), json);
        }
    }

    fn save_settings(&self)
    {
        let hotkey = self.hotkey.lock().clone();
        let mut map = serde_json::Map::new();
        map.insert("hotkey".into(), serde_json::Value::String(hotkey));
        if let Ok(json) = serde_json::to_string_pretty(&serde_json::Value::Object(map))
        {
            let _ = std::fs::write(self.settings_path(), json);
        }
    }

    fn load_from_disk(&self)
    {
        if let Ok(text) = std::fs::read_to_string(self.profiles_path())
        {
            if let Ok(parsed) = serde_json::from_str::<HashMap<String, ProfileEntry>>(&text)
            {
                *self.profiles.lock() = parsed;
            }
        }
        if let Ok(text) = std::fs::read_to_string(self.settings_path())
        {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text)
            {
                if let Some(hk) = parsed.get("hotkey").and_then(|v| v.as_str())
                {
                    *self.hotkey.lock() = hk.to_string();
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Audio commands
// ---------------------------------------------------------------------------

#[tauri::command]
fn list_devices() -> Vec<AudioDevice>
{
    audio::get_devices()
}

#[tauri::command]
fn set_default(device_id: String) -> Result<(), String>
{
    // Console role (games / default sound) plus communications (voice apps).
    audio::set_default_device(&device_id, 0)?;
    let _ = audio::set_default_device(&device_id, 2);
    Ok(())
}

#[tauri::command]
fn set_volume(device_id: String, volume: f32) -> Result<(), String>
{
    audio::set_device_volume(&device_id, volume)
}

#[tauri::command]
fn set_mute(device_id: String, mute: bool) -> Result<(), String>
{
    audio::set_device_mute(&device_id, mute)
}

#[tauri::command]
fn peaks(device_ids: Vec<String>) -> Vec<f32>
{
    device_ids.iter().map(|id| audio::get_device_peak(id)).collect()
}

// ---------------------------------------------------------------------------
// Profile commands
// ---------------------------------------------------------------------------

#[tauri::command]
fn get_profiles(state: State<Arc<SharedState>>) -> Vec<ProfileItem>
{
    let guard = state.profiles.lock();
    guard
        .iter()
        .map(|(app, entry)| ProfileItem {
            app: app.clone(),
            device_id: entry.device_id.clone(),
            device_name: entry.device_name.clone(),
        })
        .collect()
}

#[tauri::command]
fn add_profile(
    state: State<Arc<SharedState>>,
    app_name: String,
    device_id: String,
    device_name: String,
) -> Vec<ProfileItem>
{
    {
        let mut guard = state.profiles.lock();
        guard.insert(
            app_name.to_lowercase(),
            ProfileEntry {
                device_id,
                device_name,
            },
        );
    }
    state.save_profiles();
    get_profiles(state)
}

#[tauri::command]
fn remove_profile(state: State<Arc<SharedState>>, app_name: String) -> Vec<ProfileItem>
{
    {
        let mut guard = state.profiles.lock();
        guard.remove(&app_name.to_lowercase());
    }
    state.save_profiles();
    get_profiles(state)
}

// ---------------------------------------------------------------------------
// Settings / hotkey commands
// ---------------------------------------------------------------------------

#[tauri::command]
fn get_hotkey(state: State<Arc<SharedState>>) -> String
{
    state.hotkey.lock().clone()
}

#[tauri::command]
fn set_hotkey(app: AppHandle, state: State<Arc<SharedState>>, hotkey: String) -> Result<(), String>
{
    let new_shortcut = Shortcut::from_str(&hotkey)
        .map_err(|e| format!("Invalid hotkey '{hotkey}': {e}"))?;

    let gs = app.global_shortcut();

    // Drop the previous binding first (ignore failure if it was not registered).
    let old = state.hotkey.lock().clone();
    if let Ok(old_shortcut) = Shortcut::from_str(&old)
    {
        let _ = gs.unregister(old_shortcut);
    }

    // Register the new binding. If it fails (e.g. the combo is already claimed
    // by another app), roll back to the previous binding so we never end up
    // with no hotkey registered while the saved state says otherwise.
    if let Err(e) = gs.register(new_shortcut)
    {
        if let Ok(old_shortcut) = Shortcut::from_str(&old)
        {
            let _ = gs.register(old_shortcut);
        }
        return Err(format!("Failed to register hotkey: {e}"));
    }

    *state.hotkey.lock() = hotkey;
    state.save_settings();
    Ok(())
}

// ---------------------------------------------------------------------------
// Hotkey cycling + HUD
// ---------------------------------------------------------------------------

// Cycle the default playback device to the next active endpoint. Returns the
// new device name and its volume percentage on success.
fn cycle_default_device() -> Option<(String, i32)>
{
    let devices = audio::get_devices();
    let active: Vec<&AudioDevice> = devices.iter().filter(|d| d.state == "Active").collect();
    if active.len() < 2
    {
        return None;
    }

    let current_idx = active.iter().position(|d| d.is_default_audio);
    let next = match current_idx
    {
        Some(idx) =>
        {
            active[(idx + 1) % active.len()]
        }
        None =>
        {
            active[0]
        }
    };

    if audio::set_default_device(&next.id, 0).is_ok()
    {
        let _ = audio::set_default_device(&next.id, 2);
        Some((next.name.clone(), (next.volume * 100.0).round() as i32))
    }
    else
    {
        None
    }
}

// Position the HUD at the bottom-right of the primary monitor and show it.
fn show_hud(app: &AppHandle, title: &str, subtitle: &str, volume: i32)
{
    if let Some(hud) = app.get_webview_window("hud")
    {
        let _ = hud.emit(
            "hud-update",
            HudPayload {
                title: title.to_string(),
                subtitle: subtitle.to_string(),
                volume,
            },
        );

        if let Ok(Some(monitor)) = app.primary_monitor()
        {
            let size = monitor.size();
            let scale = monitor.scale_factor();
            let w = HUD_WIDTH * scale;
            let h = HUD_HEIGHT * scale;
            let margin = 24.0 * scale;
            let taskbar = 48.0 * scale;
            let x = (size.width as f64 - w - margin) as i32;
            let y = (size.height as f64 - h - taskbar) as i32;
            let _ = hud.set_position(PhysicalPosition::new(x, y));
        }

        let _ = hud.show();
        let _ = hud.set_always_on_top(true);
    }
}

#[tauri::command]
fn hide_hud(app: AppHandle)
{
    if let Some(hud) = app.get_webview_window("hud")
    {
        let _ = hud.hide();
    }
}

// ---------------------------------------------------------------------------
// System tray
// ---------------------------------------------------------------------------

fn show_main_window(app: &AppHandle)
{
    if let Some(win) = app.get_webview_window("main")
    {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

// Build the tray icon, its context menu and event handlers.
fn setup_tray(app: &AppHandle) -> tauri::Result<()>
{
    let show_item = MenuItem::with_id(app, "show", "Show GlowAudio", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&show_item, &separator, &quit_item])?;

    let mut builder = TrayIconBuilder::with_id("glow-tray")
        .tooltip("GlowAudio Desktop")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref()
        {
            "show" =>
            {
                show_main_window(app);
            }
            "quit" =>
            {
                app.exit(0);
            }
            _ =>
            {
            }
        })
        .on_tray_icon_event(|tray, event| {
            // Left click restores the main window.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });

    // Reuse the application icon for the tray when available.
    if let Some(icon) = app.default_window_icon()
    {
        builder = builder.icon(icon.clone());
    }

    builder.build(app)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// App bootstrap
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run()
{
    let state = Arc::new(SharedState {
        profiles: Mutex::new(HashMap::new()),
        hotkey: Mutex::new(DEFAULT_HOTKEY.to_string()),
        last_detected: Mutex::new(None),
        monitor_running: AtomicBool::new(true),
        config_dir: Mutex::new(PathBuf::from(".")),
    });

    let setup_state = state.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed
                    {
                        if let Some((name, volume)) = cycle_default_device()
                        {
                            show_hud(app, "GlowAudio Active", &name, volume);
                            let _ = app.emit("devices-changed", ());
                        }
                    }
                })
                .build(),
        )
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            list_devices,
            set_default,
            set_volume,
            set_mute,
            peaks,
            get_profiles,
            add_profile,
            remove_profile,
            get_hotkey,
            set_hotkey,
            hide_hud,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();

            // Resolve config directory and load persisted state.
            if let Ok(dir) = handle.path().app_config_dir()
            {
                let _ = std::fs::create_dir_all(&dir);
                *setup_state.config_dir.lock() = dir;
            }
            setup_state.load_from_disk();

            // Stamp the title bar with the version (synced from the config) and author.
            let version = handle.package_info().version.to_string();
            if let Some(win) = handle.get_webview_window("main")
            {
                let _ = win.set_title(&format!("GlowAudio Desktop  v{version}  -  by kernullist"));
            }

            // Build the hidden HUD overlay window up front for instant display.
            let _ = WebviewWindowBuilder::new(
                app,
                "hud",
                WebviewUrl::App("index.html?view=hud".into()),
            )
            .title("GlowAudio HUD")
            .inner_size(HUD_WIDTH, HUD_HEIGHT)
            .decorations(false)
            .transparent(true)
            .always_on_top(true)
            .skip_taskbar(true)
            .resizable(false)
            .shadow(false)
            .focused(false)
            .visible(false)
            .build();

            // Register the persisted global hotkey.
            let hotkey = setup_state.hotkey.lock().clone();
            if let Ok(shortcut) = Shortcut::from_str(&hotkey)
            {
                let _ = handle.global_shortcut().register(shortcut);
            }

            // Set up the system tray icon and menu.
            if let Err(e) = setup_tray(&handle)
            {
                eprintln!("[GlowAudio] tray setup failed: {e}");
            }

            // Launch the background process monitor.
            monitor::spawn(handle, setup_state.clone());

            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the main window hides it to the tray instead of quitting;
            // the app keeps running until the tray "Quit" item is chosen.
            if window.label() == "main"
            {
                if let WindowEvent::CloseRequested { api, .. } = event
                {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
