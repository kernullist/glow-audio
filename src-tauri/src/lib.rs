// GlowAudio Desktop - Tauri backend entry point.
// Wires together the audio engine, the process monitor, global hotkey cycling
// and the floating HUD overlay window, and exposes commands to the React UI.

mod audio;
mod audio_router;
mod audio_volume;
mod monitor;

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sysinfo::System;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, State, WebviewUrl, WebviewWindowBuilder,
    WindowEvent,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use windows::Win32::Media::Audio::{IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};

use audio::AudioDevice;
use audio_router::{Reconciler, RoutingEngine, RoutingRule};
use audio_volume::{AppSession, VolumeApplier, VolumeRule};

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
    // v2 per-session routing
    pub routing_rules: Mutex<Vec<RoutingRule>>,
    pub routing_available: AtomicBool,
    // per-app remembered volume rules
    pub volume_rules: Mutex<Vec<VolumeRule>>,
}

// Channel handle to the MTA audio worker. Wrapped in a Mutex so it is Sync and
// can be managed as Tauri state (mpsc::Sender is Send but not Sync).
pub struct AudioTx(pub Mutex<Sender<AudioCommand>>);

pub enum AudioCommand
{
    Reconcile, // re-evaluate all routing rules and apply diffs
    ClearAll,  // revert applied routes + nuke the OS persisted store
    Shutdown,  // revert applied routes and exit the worker
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

    fn routing_path(&self) -> PathBuf
    {
        self.config_dir.lock().join("glow_routing.json")
    }

    fn volume_path(&self) -> PathBuf
    {
        self.config_dir.lock().join("glow_volume.json")
    }

    fn save_routing(&self)
    {
        let guard = self.routing_rules.lock();
        if let Ok(json) = serde_json::to_string_pretty(&*guard)
        {
            let _ = std::fs::write(self.routing_path(), json);
        }
    }

    fn save_volume(&self)
    {
        let guard = self.volume_rules.lock();
        if let Ok(json) = serde_json::to_string_pretty(&*guard)
        {
            let _ = std::fs::write(self.volume_path(), json);
        }
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
        if let Ok(text) = std::fs::read_to_string(self.routing_path())
        {
            if let Ok(parsed) = serde_json::from_str::<Vec<RoutingRule>>(&text)
            {
                *self.routing_rules.lock() = parsed;
            }
        }
        if let Ok(text) = std::fs::read_to_string(self.volume_path())
        {
            if let Ok(parsed) = serde_json::from_str::<Vec<VolumeRule>>(&text)
            {
                *self.volume_rules.lock() = parsed;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// v2 audio worker (MTA / WinRT thread)
// ---------------------------------------------------------------------------

fn spawn_audio_worker(app: AppHandle, state: Arc<SharedState>) -> Sender<AudioCommand>
{
    let (tx, rx) = channel::<AudioCommand>();
    let tx_for_ticker = tx.clone();
    std::thread::spawn(move || {
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }
        audio_worker_loop(app, state, rx, tx_for_ticker);
        unsafe {
            RoUninitialize();
        }
    });
    tx
}

fn audio_worker_loop(
    app: AppHandle,
    state: Arc<SharedState>,
    rx: Receiver<AudioCommand>,
    tx: Sender<AudioCommand>,
)
{
    let enumerator: IMMDeviceEnumerator =
        match unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        {
            Ok(e) => e,
            Err(e) =>
            {
                eprintln!("[GlowAudio][v2-routing] enumerator failed: {e}");
                return;
            }
        };

    // Activate the per-session routing factory. On failure, leave routing
    // disabled and let the UI fall back to v1 default switching.
    let engine = match RoutingEngine::new()
    {
        Ok(e) =>
        {
            eprintln!("[GlowAudio][v2-routing] engine ready [{}]", e.which());
            e
        }
        Err(e) =>
        {
            eprintln!("[GlowAudio][v2-routing] UNAVAILABLE: {e}");
            state.routing_available.store(false, Ordering::Relaxed);
            let _ = app.emit("routing-unavailable", format!("{e}"));
            return;
        }
    };

    state.routing_available.store(true, Ordering::Relaxed);
    let _ = app.emit("routing-available", ());

    let mut reconciler = Reconciler::new(engine);
    let mut volume_applier = VolumeApplier::new();
    let mut sys = System::new();
    // Signature of the present render-endpoint set, to detect hotplug between
    // ticks and notify the UI (we have no IMMNotificationClient yet).
    let mut last_device_sig: Option<String> = None;

    // Periodic safety-net reconcile (process start without session notifications).
    let ticker = tx.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(2));
        if ticker.send(AudioCommand::Reconcile).is_err()
        {
            break;
        }
    });

    loop
    {
        match rx.recv()
        {
            Ok(AudioCommand::Reconcile) =>
            {
                sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

                // Detect device hotplug by comparing the present endpoint set and
                // notify the UI so device lists/dropdowns refresh promptly.
                if let Ok(devs) = audio_router::list_render_devices(&enumerator)
                {
                    let mut ids: Vec<String> = devs.into_iter().map(|(id, _)| id).collect();
                    ids.sort();
                    let sig = ids.join("|");
                    if last_device_sig.as_deref() != Some(sig.as_str())
                    {
                        if last_device_sig.is_some()
                        {
                            let _ = app.emit("devices-changed", ());
                        }
                        last_device_sig = Some(sig);
                    }
                }

                let rules = state.routing_rules.lock().clone();
                if let Err(e) = reconciler.reconcile(&rules, &sys, &enumerator)
                {
                    eprintln!("[GlowAudio][v2-routing] reconcile failed: {e}");
                }

                // Apply remembered per-app volumes to newly-seen sessions.
                let vol_rules = state.volume_rules.lock().clone();
                volume_applier.apply(&enumerator, &vol_rules);
            }
            Ok(AudioCommand::ClearAll) =>
            {
                sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
                let _ = reconciler.revert_all(&sys);
                let _ = reconciler.clear_os_store();
            }
            Ok(AudioCommand::Shutdown) | Err(_) =>
            {
                sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
                let _ = reconciler.revert_all(&sys);
                break;
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
// v2 routing commands
// ---------------------------------------------------------------------------

#[tauri::command]
fn routing_available(state: State<Arc<SharedState>>) -> bool
{
    state.routing_available.load(Ordering::Relaxed)
}

#[tauri::command]
fn get_routing_rules(state: State<Arc<SharedState>>) -> Vec<RoutingRule>
{
    state.routing_rules.lock().clone()
}

#[tauri::command]
fn set_routing_rule(
    state: State<Arc<SharedState>>,
    tx: State<AudioTx>,
    rule: RoutingRule,
) -> Result<Vec<RoutingRule>, String>
{
    let mut rule = rule;
    rule.match_exe = rule.match_exe.trim().to_lowercase();
    if rule.match_exe.is_empty()
    {
        return Err("exe name is empty".into());
    }
    {
        let mut rules = state.routing_rules.lock();
        match rules.iter_mut().find(|r| r.match_exe == rule.match_exe)
        {
            Some(existing) =>
            {
                *existing = rule;
            }
            None =>
            {
                rules.push(rule);
            }
        }
    }
    state.save_routing();
    let _ = tx.0.lock().send(AudioCommand::Reconcile);
    Ok(state.routing_rules.lock().clone())
}

#[tauri::command]
fn remove_routing_rule(
    state: State<Arc<SharedState>>,
    tx: State<AudioTx>,
    match_exe: String,
) -> Vec<RoutingRule>
{
    let exe = match_exe.trim().to_lowercase();
    state.routing_rules.lock().retain(|r| r.match_exe != exe);
    state.save_routing();
    // reconcile reverts the now-unmatched PIDs back to system default.
    let _ = tx.0.lock().send(AudioCommand::Reconcile);
    state.routing_rules.lock().clone()
}

#[tauri::command]
fn clear_routing(state: State<Arc<SharedState>>, tx: State<AudioTx>)
{
    state.routing_rules.lock().clear();
    state.save_routing();
    let _ = tx.0.lock().send(AudioCommand::ClearAll);
}

// ---------------------------------------------------------------------------
// Per-app volume commands
// ---------------------------------------------------------------------------

// Build a render device enumerator on the current (COM-initialized) thread.
fn render_enumerator() -> Option<IMMDeviceEnumerator>
{
    audio::ensure_com();
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.ok()
}

#[tauri::command]
fn list_app_sessions() -> Vec<AppSession>
{
    match render_enumerator()
    {
        Some(en) => audio_volume::list_app_sessions(&en),
        None => Vec::new(),
    }
}

#[tauri::command]
fn set_app_volume(exe: String, volume: f32) -> Result<(), String>
{
    let en = render_enumerator().ok_or("audio enumerator unavailable")?;
    audio_volume::set_app_volume(&en, &exe.to_lowercase(), volume)
}

#[tauri::command]
fn set_app_mute(exe: String, mute: bool) -> Result<(), String>
{
    let en = render_enumerator().ok_or("audio enumerator unavailable")?;
    audio_volume::set_app_mute(&en, &exe.to_lowercase(), mute)
}

#[tauri::command]
fn get_volume_rules(state: State<Arc<SharedState>>) -> Vec<VolumeRule>
{
    state.volume_rules.lock().clone()
}

#[tauri::command]
fn set_volume_rule(
    state: State<Arc<SharedState>>,
    tx: State<AudioTx>,
    rule: VolumeRule,
) -> Vec<VolumeRule>
{
    let mut rule = rule;
    rule.match_exe = rule.match_exe.trim().to_lowercase();
    {
        let mut rules = state.volume_rules.lock();
        match rules.iter_mut().find(|r| r.match_exe == rule.match_exe)
        {
            Some(existing) =>
            {
                *existing = rule;
            }
            None =>
            {
                rules.push(rule);
            }
        }
    }
    state.save_volume();
    // Nudge the worker so a currently-running matching app gets the value now.
    let _ = tx.0.lock().send(AudioCommand::Reconcile);
    state.volume_rules.lock().clone()
}

#[tauri::command]
fn remove_volume_rule(state: State<Arc<SharedState>>, match_exe: String) -> Vec<VolumeRule>
{
    let exe = match_exe.trim().to_lowercase();
    state.volume_rules.lock().retain(|r| r.match_exe != exe);
    state.save_volume();
    state.volume_rules.lock().clone()
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
pub(crate) fn show_hud(app: &AppHandle, title: &str, subtitle: &str, volume: i32)
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
                // Ask the audio worker to revert per-app routes before exiting so
                // no ghost routing lingers in the OS persisted store. The rules
                // stay saved and re-apply on next launch.
                let sent = app
                    .try_state::<AudioTx>()
                    .map(|tx| tx.0.lock().send(AudioCommand::Shutdown).is_ok())
                    .unwrap_or(false);
                if sent
                {
                    let handle = app.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(600));
                        handle.exit(0);
                    });
                }
                else
                {
                    app.exit(0);
                }
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
        routing_rules: Mutex::new(Vec::new()),
        routing_available: AtomicBool::new(false),
        volume_rules: Mutex::new(Vec::new()),
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
            routing_available,
            get_routing_rules,
            set_routing_rule,
            remove_routing_rule,
            clear_routing,
            list_app_sessions,
            set_app_volume,
            set_app_mute,
            get_volume_rules,
            set_volume_rule,
            remove_volume_rule,
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
            monitor::spawn(handle.clone(), setup_state.clone());

            // Launch the v2 per-session routing worker on a dedicated MTA/WinRT
            // thread. It activates IAudioPolicyConfigFactory (modern/legacy IID)
            // and, on failure, emits "routing-unavailable" so the UI falls back
            // to v1 default switching.
            let audio_tx = spawn_audio_worker(handle, setup_state.clone());
            app.manage(AudioTx(Mutex::new(audio_tx)));

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
