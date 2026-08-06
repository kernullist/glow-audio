// Per-application volume control.
//
// Each Windows audio session exposes ISimpleAudioVolume (the same per-app level
// the Volume Mixer shows). We enumerate render sessions, group them by exe, and
// let the user adjust an app's volume/mute. "Remembered" apps store a VolumeRule
// that is auto-applied once when a matching session next appears.
//
// A raw session enumeration is too unstable to drive a UI list directly: a
// Chromium browser releases its render stream a while after playback stops, and
// a Bluetooth endpoint that disconnects takes every session bound to it out of
// the enumeration at once. Both make running apps vanish from the mixer. So the
// list is served from a SessionCache that keeps recently seen apps around while
// their process is still alive. See
// docs/research/app-volume-session-visibility.md.
//
// All functions here must run on a COM-initialized (MTA) thread.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use windows::core::Interface;
use windows::Win32::Foundation::BOOL;
use windows::Win32::Media::Audio::{
    eRender, AudioSessionStateActive, IAudioSessionControl2, IAudioSessionManager2,
    IMMDeviceEnumerator, ISimpleAudioVolume, DEVICE_STATE, DEVICE_STATE_ACTIVE,
    DEVICE_STATE_UNPLUGGED,
};
use windows::Win32::System::Com::CLSCTX_ALL;

// Endpoints we look for sessions on. UNPLUGGED is deliberate: a Bluetooth
// headset that drops out leaves its endpoint in that state, and every session
// bound to it would otherwise disappear from the mixer while the owning apps
// keep running.
const SESSION_STATE_MASK: DEVICE_STATE =
    DEVICE_STATE(DEVICE_STATE_ACTIVE.0 | DEVICE_STATE_UNPLUGGED.0);

// How often the "is this exe still running" scan may run. The UI polls every
// 1.5s and a full process snapshot on every poll is wasted work.
const LIVENESS_INTERVAL: Duration = Duration::from_secs(3);

// How long an app stays listed after its last session disappeared.
pub const DEFAULT_IDLE_TTL_SECS: u64 = 300;
pub const MIN_IDLE_TTL_SECS: u64 = 0;
pub const MAX_IDLE_TTL_SECS: u64 = 3600;

#[derive(Clone, Debug, Serialize)]
pub struct AppSession
{
    pub exe: String,          // lowercase exe name, the grouping key
    pub display_name: String, // prettified name for the UI
    pub volume: f32,          // 0.0 - 1.0
    pub muted: bool,
    pub session_count: u32,
    // True while the app holds a render session. False means it was seen within
    // the idle TTL and its process is still alive, so the row stays put and
    // edits are persisted as a rule instead of being applied live.
    pub active: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VolumeRule
{
    pub match_exe: String, // lowercase
    pub volume: f32,       // 0.0 - 1.0
    pub muted: bool,
    pub enabled: bool,
}

// One live render session.
struct SessionEntry
{
    pid: u32,
    active: bool,
    // exe name recovered from the session identifier, used when sysinfo cannot
    // see the process.
    exe_hint: Option<String>,
    volume: ISimpleAudioVolume,
}

// Session identifiers look like
//   {0.0.0.00000000}.{guid}|\Device\HarddiskVolume3\...\whale.exe%b{guid}
// The middle field is the full NT path of the owning exe, so a session can be
// named without opening its process at all. System-sounds sessions carry "#"
// there instead and yield None.
fn exe_from_session_id(id: &str) -> Option<String>
{
    let middle = id.split('|').nth(1)?;
    let path = match middle.find("%b")
    {
        Some(i) =>
        {
            &middle[..i]
        }
        None =>
        {
            middle
        }
    };
    let file = path.rsplit(['\\', '/']).next()?;
    let lower = file.to_lowercase();
    if lower.ends_with(".exe")
    {
        Some(lower)
    }
    else
    {
        None
    }
}

// Enumerate every render session on an active or unplugged endpoint.
unsafe fn enumerate_sessions(enumerator: &IMMDeviceEnumerator) -> Vec<SessionEntry>
{
    let mut out = Vec::new();
    let devices = match enumerator.EnumAudioEndpoints(eRender, SESSION_STATE_MASK)
    {
        Ok(d) => d,
        Err(e) =>
        {
            log::warn!("[volume] EnumAudioEndpoints failed: {e}");
            return out;
        }
    };
    let count = devices.GetCount().unwrap_or(0);
    for i in 0..count
    {
        let device = match devices.Item(i)
        {
            Ok(d) => d,
            Err(e) =>
            {
                log::debug!("[volume] endpoint {i} unavailable: {e}");
                continue;
            }
        };
        let mgr: IAudioSessionManager2 = match device.Activate(CLSCTX_ALL, None)
        {
            Ok(m) => m,
            Err(e) =>
            {
                log::debug!("[volume] endpoint {i} session manager activate failed: {e}");
                continue;
            }
        };
        let sessions = match mgr.GetSessionEnumerator()
        {
            Ok(s) => s,
            Err(e) =>
            {
                log::debug!("[volume] endpoint {i} session enumerator failed: {e}");
                continue;
            }
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
            let active = ctrl2
                .GetState()
                .map(|st| st == AudioSessionStateActive)
                .unwrap_or(false);
            let exe_hint = ctrl2
                .GetSessionIdentifier()
                .ok()
                .and_then(|p| p.to_string().ok())
                .and_then(|id| exe_from_session_id(&id));
            if let Ok(vol) = ctrl2.cast::<ISimpleAudioVolume>()
            {
                out.push(SessionEntry {
                    pid,
                    active,
                    exe_hint,
                    volume: vol,
                });
            }
        }
    }
    out
}

// Resolve the owning exe of each session. sysinfo is the primary source; when a
// PID cannot be seen we fall back to the exe path carried in the session
// identifier, which needs no process handle. A session that resolves to neither
// used to be dropped without a trace - that silence is what made this class of
// bug so hard to diagnose in the field.
fn resolve_exes(sessions: &[SessionEntry]) -> HashMap<u32, String>
{
    let pids: Vec<Pid> = sessions.iter().map(|s| Pid::from_u32(s.pid)).collect();
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&pids),
        true,
        ProcessRefreshKind::new(),
    );

    let mut map = HashMap::new();
    for entry in sessions
    {
        let name = match sys.process(Pid::from_u32(entry.pid))
        {
            Some(p) =>
            {
                Some(p.name().to_string_lossy().to_lowercase())
            }
            None =>
            {
                entry.exe_hint.clone()
            }
        };
        if let Some(n) = name
        {
            map.insert(entry.pid, n);
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

struct CachedApp
{
    display_name: String,
    volume: f32,
    muted: bool,
    session_count: u32,
    last_seen: Instant,
}

// Keeps recently seen apps in the mixer list after their sessions go away, so a
// browser that stopped playing or an endpoint that disconnected does not make
// rows disappear underneath the user.
pub struct SessionCache
{
    apps: HashMap<String, CachedApp>,
    live_exes: HashSet<String>,
    last_liveness: Option<Instant>,
    // PIDs we already complained about, so the 1.5s poll cannot spam the log.
    warned_pids: HashSet<u32>,
}

impl SessionCache
{
    pub fn new() -> Self
    {
        Self {
            apps: HashMap::new(),
            live_exes: HashSet::new(),
            last_liveness: None,
            warned_pids: HashSet::new(),
        }
    }

    // Refresh the set of running exe names, at most once per LIVENESS_INTERVAL.
    //
    // Liveness is keyed on the exe name rather than the PID on purpose: a
    // Chromium browser renders audio from a short-lived utility child, so the
    // PID that owned the session is routinely gone while the browser the user
    // cares about is still up. The trade-off is that a second instance of the
    // same exe keeps the row alive, which is the answer the user wants anyway.
    fn refresh_live_exes(&mut self)
    {
        let due = match self.last_liveness
        {
            Some(t) =>
            {
                t.elapsed() >= LIVENESS_INTERVAL
            }
            None =>
            {
                true
            }
        };
        if !due
        {
            return;
        }
        let mut sys = System::new();
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::new());
        self.live_exes = sys
            .processes()
            .values()
            .map(|p| p.name().to_string_lossy().to_lowercase())
            .collect();
        self.last_liveness = Some(Instant::now());
    }
}

impl Default for SessionCache
{
    fn default() -> Self
    {
        Self::new()
    }
}

// Current per-app sessions, grouped by exe, merged with recently seen apps that
// are still running. `idle_ttl_secs` of 0 disables the cache entirely and
// restores the old "live sessions only" behaviour.
pub fn list_app_sessions(
    enumerator: &IMMDeviceEnumerator,
    cache: &mut SessionCache,
    idle_ttl_secs: u64,
) -> Vec<AppSession>
{
    let sessions = unsafe { enumerate_sessions(enumerator) };
    let names = resolve_exes(&sessions);

    // exe -> (volume, muted, count, active); first session wins for the level.
    let mut live: HashMap<String, (f32, bool, u32, bool)> = HashMap::new();
    for entry in &sessions
    {
        let exe = match names.get(&entry.pid)
        {
            Some(n) => n.clone(),
            None =>
            {
                if cache.warned_pids.insert(entry.pid)
                {
                    log::warn!(
                        "[volume] session pid {} has no resolvable exe, dropped from the list",
                        entry.pid
                    );
                }
                continue;
            }
        };
        let v = unsafe { entry.volume.GetMasterVolume() }.unwrap_or(0.0);
        let m = unsafe { entry.volume.GetMute() }
            .map(|b| b.as_bool())
            .unwrap_or(false);
        let slot = live.entry(exe).or_insert((v, m, 0, false));
        slot.2 += 1;
        slot.3 |= entry.active;
    }

    let seen_pids: HashSet<u32> = sessions.iter().map(|s| s.pid).collect();
    cache.warned_pids.retain(|p| seen_pids.contains(p));

    let now = Instant::now();
    let ttl = Duration::from_secs(idle_ttl_secs);

    for (exe, (volume, muted, count, _)) in &live
    {
        cache.apps.insert(
            exe.clone(),
            CachedApp {
                display_name: prettify(exe),
                volume: *volume,
                muted: *muted,
                session_count: *count,
                last_seen: now,
            },
        );
    }

    // Drop expired entries before paying for a process scan.
    cache
        .apps
        .retain(|exe, app| live.contains_key(exe) || now.duration_since(app.last_seen) <= ttl);

    // Anything still listed without a live session has to prove its process is
    // alive, otherwise a closed app would linger for the whole TTL.
    if cache.apps.keys().any(|e| !live.contains_key(e))
    {
        cache.refresh_live_exes();
        let live_exes = std::mem::take(&mut cache.live_exes);
        cache
            .apps
            .retain(|exe, _| live.contains_key(exe) || live_exes.contains(exe));
        cache.live_exes = live_exes;
    }

    let mut out: Vec<AppSession> = cache
        .apps
        .iter()
        .map(|(exe, app)| AppSession {
            exe: exe.clone(),
            display_name: app.display_name.clone(),
            volume: app.volume,
            muted: app.muted,
            session_count: app.session_count,
            active: live.get(exe).map(|s| s.3).unwrap_or(false),
        })
        .collect();
    // Playing apps first, then alphabetical, so the list does not reshuffle as
    // sessions come and go.
    out.sort_by(|a, b| {
        b.active
            .cmp(&a.active)
            .then_with(|| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()))
    });
    out
}

// Set the volume of every session belonging to `exe_lower`. Returns the number
// of sessions touched; 0 means the app is idle and only its remembered rule can
// carry the change forward.
pub fn set_app_volume(
    enumerator: &IMMDeviceEnumerator,
    exe_lower: &str,
    volume: f32,
) -> Result<u32, String>
{
    let sessions = unsafe { enumerate_sessions(enumerator) };
    let names = resolve_exes(&sessions);
    let v = volume.clamp(0.0, 1.0);
    let mut hit = 0;
    for entry in &sessions
    {
        if names.get(&entry.pid).map(|s| s.as_str()) == Some(exe_lower)
        {
            unsafe {
                let _ = entry.volume.SetMasterVolume(v, std::ptr::null());
            }
            hit += 1;
        }
    }
    Ok(hit)
}

// Mute/unmute every session belonging to `exe_lower`. Returns sessions touched.
pub fn set_app_mute(
    enumerator: &IMMDeviceEnumerator,
    exe_lower: &str,
    mute: bool,
) -> Result<u32, String>
{
    let sessions = unsafe { enumerate_sessions(enumerator) };
    let names = resolve_exes(&sessions);
    let mut hit = 0;
    for entry in &sessions
    {
        if names.get(&entry.pid).map(|s| s.as_str()) == Some(exe_lower)
        {
            unsafe {
                let _ = entry.volume.SetMute(BOOL::from(mute), std::ptr::null());
            }
            hit += 1;
        }
    }
    Ok(hit)
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
        let names = resolve_exes(&sessions);
        let live: HashSet<u32> = sessions.iter().map(|s| s.pid).collect();

        let mut newly: Vec<u32> = Vec::new();
        for entry in &sessions
        {
            if self.applied.contains(&entry.pid)
            {
                continue;
            }
            let exe = match names.get(&entry.pid)
            {
                Some(n) => n,
                None => continue,
            };
            if let Some(rule) = rules.iter().find(|r| r.enabled && &r.match_exe == exe)
            {
                unsafe {
                    let _ = entry
                        .volume
                        .SetMasterVolume(rule.volume.clamp(0.0, 1.0), std::ptr::null());
                    let _ = entry.volume.SetMute(BOOL::from(rule.muted), std::ptr::null());
                }
                log::debug!("[volume] applied remembered level to {exe} (pid {})", entry.pid);
                newly.push(entry.pid);
            }
        }
        for p in newly
        {
            self.applied.insert(p);
        }
        self.applied.retain(|p| live.contains(p));
    }
}

#[cfg(test)]
mod tests
{
    use super::*;

    #[test]
    fn parses_exe_from_session_identifier()
    {
        let id = "{0.0.0.00000000}.{3fc2224a-6961-4dff-b2a8-a393cdf33ea6}|\
                  \\Device\\HarddiskVolume3\\Program Files\\Naver\\Naver Whale\\\
                  Application\\4.39.410.6\\whale.exe%b{00000000-0000-0000-0000-000000000000}";
        assert_eq!(exe_from_session_id(id), Some("whale.exe".to_string()));
    }

    #[test]
    fn rejects_system_sounds_session_identifier()
    {
        let id = "{0.0.0.00000000}.{85b9b434-ee41-4df7-b1e2-c3b288657a4a}|\
                  #%b{A9EF3FD9-4240-455E-A4D5-F2B3301887B2}";
        assert_eq!(exe_from_session_id(id), None);
    }

    #[test]
    fn rejects_malformed_session_identifier()
    {
        assert_eq!(exe_from_session_id("no-pipe-here"), None);
    }

    // End-to-end check of the idle cache against real audio state. Needs a
    // working render endpoint and briefly plays a system sound, so it is not
    // part of the default suite. Run with:
    //   cargo test -- --ignored --nocapture
    #[test]
    #[ignore]
    fn idle_app_stays_listed_until_its_process_exits()
    {
        use windows::Win32::Media::Audio::MMDeviceEnumerator;
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, COINIT_MULTITHREADED,
        };

        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let en: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                .expect("render enumerator");

        let mut cache = SessionCache::new();
        let ttl = 300;

        // Liveness is name-based, so the probe needs a name nothing else on the
        // machine shares - running this under "pwsh.exe" would see the shell
        // that launched the test and never observe the row being dropped.
        let probe = std::env::temp_dir()
            .join(format!("glowaudio_probe_{}.exe", std::process::id()));
        std::fs::copy(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe", &probe)
            .expect("copy probe host");
        let target = probe
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.to_lowercase())
            .expect("probe name");
        let target = target.as_str();

        // Plays a short sound, then stays alive and silent - the same shape as a
        // browser that stopped playing.
        let mut child = std::process::Command::new(&probe)
            .args([
                "-NoProfile",
                "-Command",
                "$p = New-Object System.Media.SoundPlayer 'C:\\Windows\\Media\\Ring01.wav'; \
                 $p.PlaySync(); Start-Sleep -Seconds 40",
            ])
            .spawn()
            .expect("spawn sound player");

        let find = |list: &[AppSession]| -> Option<AppSession> {
            list.iter().find(|s| s.exe == target).cloned()
        };

        // Run the three phases first and assert afterwards, so a failure still
        // leaves the machine clean (no orphaned player, no stray exe copy).
        let mut outcome: Result<(), String> = Ok(());

        // 1. It shows up while playing.
        let mut seen_playing = false;
        for _ in 0..60
        {
            let list = list_app_sessions(&en, &mut cache, ttl);
            if let Some(s) = find(&list)
            {
                if s.active
                {
                    // Silence it right away; the rest of the test does not need
                    // to be audible.
                    let _ = set_app_mute(&en, target, true);
                    seen_playing = true;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        if !seen_playing
        {
            outcome = Err("player never appeared as an active session".into());
        }

        // 2. Playback ends and the session goes away, but the row must survive
        //    because the process is still running. This is the actual fix.
        if outcome.is_ok()
        {
            let mut went_idle = false;
            for _ in 0..80
            {
                let list = list_app_sessions(&en, &mut cache, ttl);
                match find(&list)
                {
                    Some(s) =>
                    {
                        if !s.active
                        {
                            went_idle = true;
                            break;
                        }
                    }
                    None =>
                    {
                        outcome =
                            Err("row disappeared while the process was still alive".into());
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            if outcome.is_ok() && !went_idle
            {
                outcome = Err("player never went idle within the timeout".into());
            }
        }

        // 3. Once the process is gone the row must disappear promptly rather
        //    than linger for the whole TTL.
        let _ = child.kill();
        let _ = child.wait();
        if outcome.is_ok()
        {
            let mut gone = false;
            for _ in 0..40
            {
                let list = list_app_sessions(&en, &mut cache, ttl);
                if find(&list).is_none()
                {
                    gone = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            if !gone
            {
                outcome = Err("dead process still listed after the liveness scan".into());
            }
        }

        let _ = std::fs::remove_file(&probe);
        if let Err(e) = outcome
        {
            panic!("{e}");
        }
    }

    // The App Volume tab polls this path every 1.5s for as long as the app is
    // open, so it has to stay flat over tens of thousands of calls. Guards
    // against COM object or process-handle leaks creeping in.
    // Run with: cargo test --release -- --ignored --nocapture
    #[test]
    #[ignore]
    fn enumeration_does_not_leak_over_a_long_session()
    {
        use windows::Win32::Media::Audio::MMDeviceEnumerator;
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, COINIT_MULTITHREADED,
        };
        use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};

        fn handles() -> u32
        {
            let mut c = 0u32;
            unsafe {
                let _ = GetProcessHandleCount(GetCurrentProcess(), &mut c);
            }
            c
        }

        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }

        let iters = 5000;
        let mut cache = SessionCache::new();
        // Warm up so one-time COM allocations are not counted as growth.
        for _ in 0..50
        {
            let en: IMMDeviceEnumerator =
                unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                    .expect("render enumerator");
            let _ = list_app_sessions(&en, &mut cache, 300);
        }

        let baseline = handles();
        let start = Instant::now();
        for _ in 0..iters
        {
            // A fresh enumerator per call, exactly like the command path.
            let en: IMMDeviceEnumerator =
                unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                    .expect("render enumerator");
            let _ = list_app_sessions(&en, &mut cache, 300);
        }
        let after = handles();
        let per_call = start.elapsed() / iters;
        println!(
            "soak: {iters} calls, {:?} total, {per_call:?}/call, handles {baseline} -> {after}",
            start.elapsed()
        );

        assert!(
            after <= baseline + 64,
            "handle count grew from {baseline} to {after} over {iters} calls"
        );
    }
}
