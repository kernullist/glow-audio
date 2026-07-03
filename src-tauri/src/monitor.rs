// Background process monitor. Scans running processes on an interval and, when
// a process matching a registered game profile appears, routes the default
// playback device to the mapped endpoint and notifies the UI / HUD.

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use sysinfo::{ProcessesToUpdate, System};
use tauri::{AppHandle, Emitter};

use crate::{audio, AutoSwitchPayload, SharedState};

const SCAN_INTERVAL: Duration = Duration::from_secs(2);

pub fn spawn(app: AppHandle, state: Arc<SharedState>)
{
    std::thread::spawn(move || {
        let mut sys = System::new();

        loop
        {
            if !state.monitor_running.load(Ordering::Relaxed)
            {
                break;
            }

            sys.refresh_processes(ProcessesToUpdate::All, true);

            let mut active: HashSet<String> = HashSet::new();
            for (_pid, proc_) in sys.processes()
            {
                let name = proc_.name().to_string_lossy().to_lowercase();
                if !name.is_empty()
                {
                    active.insert(name);
                }
            }

            // Snapshot the profile map so we do not hold the lock across COM calls.
            let profiles: Vec<(String, String, String)> = {
                let guard = state.profiles.lock();
                guard
                    .iter()
                    .map(|(app_name, entry)| {
                        (app_name.clone(), entry.device_id.clone(), entry.device_name.clone())
                    })
                    .collect()
            };

            let last = state.last_detected.lock().clone();
            let mut matched_now: Option<String> = None;

            for (app_name, device_id, device_name) in &profiles
            {
                if !active.contains(app_name)
                {
                    continue;
                }

                matched_now = Some(app_name.clone());

                // Only act when this is a newly detected process.
                if last.as_deref() == Some(app_name.as_str())
                {
                    break;
                }

                // Confirm the target endpoint is active and not already default.
                let devices = audio::get_devices();
                let target = devices
                    .iter()
                    .find(|d| &d.id == device_id && d.state == "Active");

                if let Some(target) = target
                {
                    if !target.is_default_audio
                    {
                        // Switch console (games / system sound) and communications roles.
                        let _ = audio::set_default_device(device_id, 0);
                        let _ = audio::set_default_device(device_id, 2);

                        *state.last_detected.lock() = Some(app_name.clone());

                        let _ = app.emit(
                            "auto-switch",
                            AutoSwitchPayload {
                                app: app_name.clone(),
                                device_name: device_name.clone(),
                            },
                        );
                        let _ = app.emit("devices-changed", ());

                        // Show the HUD overlay so the switch is visible in-game.
                        let volume = (target.volume * 100.0).round() as i32;
                        crate::show_hud(
                            &app,
                            &format!("App Launch: {app_name}"),
                            device_name,
                            volume,
                        );
                    }
                    else
                    {
                        // Already routed; just remember it so we do not re-trigger.
                        *state.last_detected.lock() = Some(app_name.clone());
                    }
                }
                break;
            }

            // Reset the trigger once the mapped process is gone so a relaunch can fire again.
            if matched_now.is_none()
            {
                *state.last_detected.lock() = None;
            }

            std::thread::sleep(SCAN_INTERVAL);
        }
    });
}
