// Per-application volume control.
//
// Each Windows audio session exposes ISimpleAudioVolume (the same per-app level
// the Volume Mixer shows). We enumerate render sessions, group them by exe, and
// let the user adjust an app's volume/mute. "Remembered" apps store a VolumeRule
// that is auto-applied once when a matching session next appears.
//
// All functions here must run on a COM-initialized (MTA) thread.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessesToUpdate, System};
use windows::core::Interface;
use windows::Win32::Foundation::BOOL;
use windows::Win32::Media::Audio::{
    eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, ISimpleAudioVolume,
    DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::CLSCTX_ALL;

#[derive(Clone, Debug, Serialize)]
pub struct AppSession
{
    pub exe: String,          // lowercase exe name, the grouping key
    pub display_name: String, // prettified name for the UI
    pub volume: f32,          // 0.0 - 1.0
    pub muted: bool,
    pub session_count: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VolumeRule
{
    pub match_exe: String, // lowercase
    pub volume: f32,       // 0.0 - 1.0
    pub muted: bool,
    pub enabled: bool,
}

// Enumerate every active render session as (pid, ISimpleAudioVolume).
unsafe fn enumerate_sessions(enumerator: &IMMDeviceEnumerator) -> Vec<(u32, ISimpleAudioVolume)>
{
    let mut out = Vec::new();
    let devices = match enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
    {
        Ok(d) => d,
        Err(_) => return out,
    };
    let count = devices.GetCount().unwrap_or(0);
    for i in 0..count
    {
        let device = match devices.Item(i)
        {
            Ok(d) => d,
            Err(_) => continue,
        };
        let mgr: IAudioSessionManager2 = match device.Activate(CLSCTX_ALL, None)
        {
            Ok(m) => m,
            Err(_) => continue,
        };
        let sessions = match mgr.GetSessionEnumerator()
        {
            Ok(s) => s,
            Err(_) => continue,
        };
        let scount = sessions.GetCount().unwrap_or(0);
        for s in 0..scount
        {
            let ctrl = match sessions.GetSession(s)
            {
                Ok(c) => c,
                Err(_) => continue,
            };
            let ctrl2: IAudioSessionControl2 = match ctrl.cast()
            {
                Ok(c) => c,
                Err(_) => continue,
            };
            let pid = ctrl2.GetProcessId().unwrap_or(0);
            if pid == 0
            {
                continue;
            }
            if let Ok(vol) = ctrl2.cast::<ISimpleAudioVolume>()
            {
                out.push((pid, vol));
            }
        }
    }
    out
}

// Resolve process names for a set of PIDs (refreshing only those PIDs).
fn names_for(pids: &[u32]) -> HashMap<u32, String>
{
    let pid_list: Vec<Pid> = pids.iter().map(|&p| Pid::from_u32(p)).collect();
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&pid_list), true);

    let mut map = HashMap::new();
    for &p in pids
    {
        if let Some(proc_) = sys.process(Pid::from_u32(p))
        {
            map.insert(p, proc_.name().to_string_lossy().to_lowercase());
        }
    }
    map
}

// "chrome.exe" -> "Chrome"
fn prettify(exe: &str) -> String
{
    let base = exe.strip_suffix(".exe").unwrap_or(exe);
    let mut chars = base.chars();
    match chars.next()
    {
        Some(first) =>
        {
            first.to_uppercase().collect::<String>() + chars.as_str()
        }
        None =>
        {
            base.to_string()
        }
    }
}

// Current per-app sessions, grouped by exe.
pub fn list_app_sessions(enumerator: &IMMDeviceEnumerator) -> Vec<AppSession>
{
    let sessions = unsafe { enumerate_sessions(enumerator) };
    let pids: Vec<u32> = sessions.iter().map(|(p, _)| *p).collect();
    let names = names_for(&pids);

    // exe -> (volume, muted, count); first session wins for the displayed level.
    let mut by_exe: HashMap<String, (f32, bool, u32)> = HashMap::new();
    for (pid, vol) in &sessions
    {
        let exe = match names.get(pid)
        {
            Some(n) => n.clone(),
            None => continue,
        };
        let v = unsafe { vol.GetMasterVolume() }.unwrap_or(0.0);
        let m = unsafe { vol.GetMute() }.map(|b| b.as_bool()).unwrap_or(false);
        let entry = by_exe.entry(exe).or_insert((v, m, 0));
        entry.2 += 1;
    }

    let mut out: Vec<AppSession> = by_exe
        .into_iter()
        .map(|(exe, (volume, muted, session_count))| AppSession {
            display_name: prettify(&exe),
            exe,
            volume,
            muted,
            session_count,
        })
        .collect();
    out.sort_by(|a, b| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()));
    out
}

// Set the volume of every session belonging to `exe_lower`.
pub fn set_app_volume(enumerator: &IMMDeviceEnumerator, exe_lower: &str, volume: f32) -> Result<(), String>
{
    let sessions = unsafe { enumerate_sessions(enumerator) };
    let pids: Vec<u32> = sessions.iter().map(|(p, _)| *p).collect();
    let names = names_for(&pids);
    let v = volume.clamp(0.0, 1.0);
    for (pid, vol) in &sessions
    {
        if names.get(pid).map(|s| s.as_str()) == Some(exe_lower)
        {
            unsafe {
                let _ = vol.SetMasterVolume(v, std::ptr::null());
            }
        }
    }
    Ok(())
}

// Mute/unmute every session belonging to `exe_lower`.
pub fn set_app_mute(enumerator: &IMMDeviceEnumerator, exe_lower: &str, mute: bool) -> Result<(), String>
{
    let sessions = unsafe { enumerate_sessions(enumerator) };
    let pids: Vec<u32> = sessions.iter().map(|(p, _)| *p).collect();
    let names = names_for(&pids);
    for (pid, vol) in &sessions
    {
        if names.get(pid).map(|s| s.as_str()) == Some(exe_lower)
        {
            unsafe {
                let _ = vol.SetMute(BOOL::from(mute), std::ptr::null());
            }
        }
    }
    Ok(())
}

// Applies remembered volume rules once per session lifetime.
pub struct VolumeApplier
{
    applied: HashSet<u32>,
}

impl VolumeApplier
{
    pub fn new() -> Self
    {
        Self { applied: HashSet::new() }
    }

    // For each live session whose exe matches an enabled rule and that has not
    // been handled yet, set its volume/mute to the remembered values. Dead PIDs
    // are forgotten so a relaunch re-applies.
    pub fn apply(&mut self, enumerator: &IMMDeviceEnumerator, rules: &[VolumeRule])
    {
        if rules.is_empty() && self.applied.is_empty()
        {
            return;
        }

        let sessions = unsafe { enumerate_sessions(enumerator) };
        let pids: Vec<u32> = sessions.iter().map(|(p, _)| *p).collect();
        let live: HashSet<u32> = pids.iter().copied().collect();
        let names = names_for(&pids);

        let mut newly: Vec<u32> = Vec::new();
        for (pid, vol) in &sessions
        {
            if self.applied.contains(pid)
            {
                continue;
            }
            let exe = match names.get(pid)
            {
                Some(n) => n,
                None => continue,
            };
            if let Some(rule) = rules.iter().find(|r| r.enabled && &r.match_exe == exe)
            {
                unsafe {
                    let _ = vol.SetMasterVolume(rule.volume.clamp(0.0, 1.0), std::ptr::null());
                    let _ = vol.SetMute(BOOL::from(rule.muted), std::ptr::null());
                }
                newly.push(*pid);
            }
        }
        for p in newly
        {
            self.applied.insert(p);
        }
        self.applied.retain(|p| live.contains(p));
    }
}
