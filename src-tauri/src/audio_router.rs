// GlowAudio v2 - per-session audio routing engine.
//
// Routes individual processes to specific output endpoints via the undocumented
// IAudioPolicyConfigFactory (the same mechanism EarTrumpet uses). This interface
// is IInspectable-based and build-dependent: its IID, vtable layout, and the
// device-interface id format must be verified at runtime on the target Windows
// build before anything is built on top. `probe()` does that verification.
//
// All functions here must be called on a WinRT/MTA-initialized worker thread.

// PascalCase COM method names; suppress the snake_case lints for the whole module.
#![allow(non_snake_case)]

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sysinfo::{Pid, System};
use windows::core::{Interface, BSTR, HRESULT, HSTRING};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    eCommunications, eConsole, eMultimedia, eRender, EDataFlow, ERole, IAudioSessionControl2,
    IAudioSessionManager2, IMMDevice, IMMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{CLSCTX_ALL, STGM_READ};
use windows::Win32::System::WinRT::RoGetActivationFactory;

// IAudioPolicyConfigFactory is really IInspectable-based, so its vtable is
// IUnknown(3) + IInspectable(3) + 3 methods. The #[interface] macro cannot
// synthesize the IInspectable_Impl companion for a built-in parent, so instead
// we model it on IUnknown and pad the three IInspectable slots
// (GetIids / GetRuntimeClassName / GetTrustLevel) as dummy entries. The padded
// layout matches the real object byte-for-byte; we only ever call the last three.
//
// Two IIDs exist for this factory and which one a given Windows build accepts
// varies. We try the modern one first and fall back to the legacy one on
// E_NOINTERFACE. The 3-method layout is the same for both.
//   modern: 2a59116d-6c4f-45e0-a74f-707e3fef9258
//   legacy: ab3d4648-e242-459f-b02f-541c70306324
#[windows_core::interface("2a59116d-6c4f-45e0-a74f-707e3fef9258")]
unsafe trait IAudioPolicyConfigFactory: windows_core::IUnknown
{
    unsafe fn GetIids(&self) -> HRESULT;
    unsafe fn GetRuntimeClassName(&self) -> HRESULT;
    unsafe fn GetTrustLevel(&self) -> HRESULT;
    unsafe fn SetPersistedDefaultAudioEndpoint(
        &self, process_id: u32, flow: EDataFlow, role: ERole, device_id: HSTRING) -> HRESULT;
    unsafe fn GetPersistedDefaultAudioEndpoint(
        &self, process_id: u32, flow: EDataFlow, role: ERole, device_id: *mut HSTRING) -> HRESULT;
    unsafe fn ClearAllPersistedApplicationDefaultEndpoints(&self) -> HRESULT;
}

// Windows 11 (21H2+) variant. SetPersistedDefaultAudioEndpoint sits AFTER 19
// extra interface methods, so we pad IInspectable(3) + those 19 = 22 slots
// before the three methods we actually use. Padding names/order mirror
// SoundSwitch's IAudioPolicyConfigFactoryVariant21H2Windows11 so the vtable
// index of SetPersistedDefaultAudioEndpoint (slot 25 overall) lines up exactly.
#[windows_core::interface("ab3d4648-e242-459f-b02f-541c70306324")]
unsafe trait IAudioPolicyConfigFactoryLegacy: windows_core::IUnknown
{
    // IInspectable (slots 3..6)
    unsafe fn GetIids(&self) -> HRESULT;
    unsafe fn GetRuntimeClassName(&self) -> HRESULT;
    unsafe fn GetTrustLevel(&self) -> HRESULT;
    // 19 unused interface methods (slots 6..25)
    unsafe fn pad_add_CtxVolumeChange(&self) -> HRESULT;
    unsafe fn pad_remove_CtxVolumeChanged(&self) -> HRESULT;
    unsafe fn pad_add_RingerVibrateStateChanged(&self) -> HRESULT;
    unsafe fn pad_remove_RingerVibrateStateChange(&self) -> HRESULT;
    unsafe fn pad_SetVolumeGroupGainForId(&self) -> HRESULT;
    unsafe fn pad_GetVolumeGroupGainForId(&self) -> HRESULT;
    unsafe fn pad_GetActiveVolumeGroupForEndpointId(&self) -> HRESULT;
    unsafe fn pad_GetVolumeGroupsForEndpoint(&self) -> HRESULT;
    unsafe fn pad_GetCurrentVolumeContext(&self) -> HRESULT;
    unsafe fn pad_SetVolumeGroupMuteForId(&self) -> HRESULT;
    unsafe fn pad_GetVolumeGroupMuteForId(&self) -> HRESULT;
    unsafe fn pad_SetRingerVibrateState(&self) -> HRESULT;
    unsafe fn pad_GetRingerVibrateState(&self) -> HRESULT;
    unsafe fn pad_SetPreferredChatApplication(&self) -> HRESULT;
    unsafe fn pad_ResetPreferredChatApplication(&self) -> HRESULT;
    unsafe fn pad_GetPreferredChatApplication(&self) -> HRESULT;
    unsafe fn pad_GetCurrentChatApplications(&self) -> HRESULT;
    unsafe fn pad_add_ChatContextChanged(&self) -> HRESULT;
    unsafe fn pad_remove_ChatContextChanged(&self) -> HRESULT;
    // The three methods we use (slots 25..28)
    unsafe fn SetPersistedDefaultAudioEndpoint(
        &self, process_id: u32, flow: EDataFlow, role: ERole, device_id: HSTRING) -> HRESULT;
    unsafe fn GetPersistedDefaultAudioEndpoint(
        &self, process_id: u32, flow: EDataFlow, role: ERole, device_id: *mut HSTRING) -> HRESULT;
    unsafe fn ClearAllPersistedApplicationDefaultEndpoints(&self) -> HRESULT;
}

const RUNTIME_CLASS: &str = "Windows.Media.Internal.AudioPolicyConfig";

// EarTrumpet-confirmed device-interface path format:
//   \\?\SWD#MMDEVAPI#{mmdevice_id}#{interface_guid}
// Getting this format wrong yields a silent failure: the call returns S_OK but
// no routing happens.
const DEVINTERFACE_RENDER: &str = "{e6327cad-dcec-4949-ae8a-991e976a79d2}";
const DEVINTERFACE_CAPTURE: &str = "{2eef81be-33fa-4800-9670-1cd474972c3f}";

fn make_endpoint_id(mmdevice_id: &str, flow: EDataFlow) -> HSTRING
{
    // Empty -> system default for this process.
    if mmdevice_id.is_empty()
    {
        return HSTRING::new();
    }
    let guid = if flow == eRender
    {
        DEVINTERFACE_RENDER
    }
    else
    {
        DEVINTERFACE_CAPTURE
    };
    HSTRING::from(format!("\\\\?\\SWD#MMDEVAPI#{}#{}", mmdevice_id, guid))
}

// Whichever factory IID the current build accepts.
enum Factory
{
    Modern(IAudioPolicyConfigFactory),
    Legacy(IAudioPolicyConfigFactoryLegacy),
}

pub struct RoutingEngine
{
    factory: Factory,
}

impl RoutingEngine
{
    // Activate the PolicyConfig runtime class. Must run on an MTA/WinRT thread.
    // Tries the modern IID, then falls back to the legacy IID on E_NOINTERFACE.
    pub fn new() -> windows::core::Result<Self>
    {
        let class_id = HSTRING::from(RUNTIME_CLASS);
        match unsafe { RoGetActivationFactory::<IAudioPolicyConfigFactory>(&class_id) }
        {
            Ok(f) =>
            {
                Ok(Self { factory: Factory::Modern(f) })
            }
            Err(_) =>
            {
                let f: IAudioPolicyConfigFactoryLegacy =
                    unsafe { RoGetActivationFactory(&class_id)? };
                Ok(Self { factory: Factory::Legacy(f) })
            }
        }
    }

    // Which IID variant activated (for diagnostics).
    pub fn which(&self) -> &'static str
    {
        match self.factory
        {
            Factory::Modern(_) =>
            {
                "modern (2a59116d)"
            }
            Factory::Legacy(_) =>
            {
                "legacy (ab3d4648)"
            }
        }
    }

    // Route one PID to a device. mmdevice_id = None reverts it to system default.
    // Console + Multimedia cover normal playback; Communications is added for
    // comms apps so voice routing follows too.
    pub fn route_process(
        &self,
        pid: u32,
        mmdevice_id: Option<&str>,
        is_comms: bool,
    ) -> windows::core::Result<()>
    {
        let endpoint = make_endpoint_id(mmdevice_id.unwrap_or(""), eRender);

        let mut roles: Vec<ERole> = vec![eConsole, eMultimedia];
        if is_comms
        {
            roles.push(eCommunications);
        }

        let mut result: windows::core::Result<()> = Ok(());
        for role in roles
        {
            // Clone per call: each call consumes one caller-owned [in] HSTRING.
            let hs = endpoint.clone();
            let hr = unsafe {
                match &self.factory
                {
                    Factory::Modern(f) =>
                    {
                        f.SetPersistedDefaultAudioEndpoint(pid, eRender, role, hs)
                    }
                    Factory::Legacy(f) =>
                    {
                        f.SetPersistedDefaultAudioEndpoint(pid, eRender, role, hs)
                    }
                }
            };
            if hr.is_err()
            {
                result = hr.ok();
            }
        }
        result
    }

    // The device-interface path currently persisted for this PID ("" = default).
    // Exposed for diagnostics / future UI; not yet wired to a command.
    #[allow(dead_code)]
    pub fn query_process(&self, pid: u32) -> windows::core::Result<String>
    {
        let mut out = HSTRING::new();
        unsafe {
            match &self.factory
            {
                Factory::Modern(f) =>
                {
                    f.GetPersistedDefaultAudioEndpoint(pid, eRender, eConsole, &mut out)
                }
                Factory::Legacy(f) =>
                {
                    f.GetPersistedDefaultAudioEndpoint(pid, eRender, eConsole, &mut out)
                }
            }
            .ok()?;
        }
        Ok(out.to_string())
    }

    // Remove every per-app override from the OS persisted store. Bind this to a
    // visible "Reset routing" action so overrides never linger as ghost routes.
    pub fn clear_all(&self) -> windows::core::Result<()>
    {
        unsafe {
            match &self.factory
            {
                Factory::Modern(f) =>
                {
                    f.ClearAllPersistedApplicationDefaultEndpoints()
                }
                Factory::Legacy(f) =>
                {
                    f.ClearAllPersistedApplicationDefaultEndpoints()
                }
            }
            .ok()
        }
    }
}

// ---------------------------------------------------------------------------
// Audio session / device discovery
// ---------------------------------------------------------------------------

// Distinct PIDs that currently own an active render session across all devices.
// Audio often comes from a renderer/child process (Chrome/Discord/Electron), not
// the main PID, so routing is only applied to PIDs that actually have a session.
pub fn collect_audio_pids(enumerator: &IMMDeviceEnumerator) -> windows::core::Result<Vec<u32>>
{
    let mut pids: Vec<u32> = Vec::new();
    unsafe {
        let devices = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        let count = devices.GetCount()?;
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
                if let Ok(ctrl2) = ctrl.cast::<IAudioSessionControl2>()
                {
                    if let Ok(pid) = ctrl2.GetProcessId()
                    {
                        if pid != 0
                        {
                            pids.push(pid);
                        }
                    }
                }
            }
        }
    }
    pids.sort_unstable();
    pids.dedup();
    Ok(pids)
}

// Read PKEY_Device_FriendlyName from a device property store.
fn friendly_name_of(device: &IMMDevice) -> Option<String>
{
    unsafe {
        let store = device.OpenPropertyStore(STGM_READ).ok()?;
        let prop = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
        let bstr = BSTR::try_from(&prop).ok()?;
        Some(bstr.to_string())
    }
}

// Resolve a current MMDevice id by friendly name (raw GetId string).
pub fn resolve_by_name(enumerator: &IMMDeviceEnumerator, friendly: &str) -> Option<String>
{
    unsafe {
        let devices = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE).ok()?;
        let count = devices.GetCount().ok()?;
        for i in 0..count
        {
            let device = devices.Item(i).ok()?;
            if friendly_name_of(&device).as_deref() == Some(friendly)
            {
                let id = device.GetId().ok()?;
                return id.to_string().ok();
            }
        }
    }
    None
}

// Currently present render endpoints as (mmdevice_id, friendly_name).
pub fn list_render_devices(
    enumerator: &IMMDeviceEnumerator,
) -> windows::core::Result<Vec<(String, String)>>
{
    let mut out = Vec::new();
    unsafe {
        let devices = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        let count = devices.GetCount()?;
        for i in 0..count
        {
            let device = devices.Item(i)?;
            let id = device.GetId()?.to_string()?;
            let name = friendly_name_of(&device).unwrap_or_else(|| id.clone());
            out.push((id, name));
        }
    }
    Ok(out)
}

// All PIDs whose process name matches the rule (case-insensitive). Child
// renderers usually share the parent's exe name, so a name match intersected
// with the audio-PID set (in the reconciler) catches them.
pub fn pids_for_exe(sys: &System, exe_lower: &str) -> Vec<u32>
{
    let mut out = Vec::new();
    for (pid, proc_) in sys.processes()
    {
        let name = proc_.name().to_string_lossy().to_lowercase();
        if name == exe_lower
        {
            out.push(pid.as_u32());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Rule model + reconciler
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoutingRule
{
    pub match_exe: String,                  // lowercase, e.g. "discord.exe"
    pub target_device_id: Option<String>,   // None -> system default
    pub target_device_name: String,         // friendly fallback if id drifts
    pub is_comms: bool,
    pub enabled: bool,
}

pub struct Reconciler
{
    engine: RoutingEngine,
    applied: HashMap<u32, String>, // pid -> last applied id ("" = default)
}

impl Reconciler
{
    pub fn new(engine: RoutingEngine) -> Self
    {
        Self { engine, applied: HashMap::new() }
    }

    // Recompute desired routing and apply only the diffs.
    pub fn reconcile(
        &mut self,
        rules: &[RoutingRule],
        sys: &System,
        enumerator: &IMMDeviceEnumerator,
    ) -> windows::core::Result<()>
    {
        // 1. PIDs that actually have audio, and ids currently present.
        let mut audio_pids = collect_audio_pids(enumerator)?;
        audio_pids.sort_unstable();
        let present_ids: Vec<String> = list_render_devices(enumerator)?
            .into_iter()
            .map(|(id, _)| id)
            .collect();

        // 2. Desired: pid -> (device_id, is_comms).
        let mut desired: HashMap<u32, (String, bool)> = HashMap::new();
        for rule in rules.iter().filter(|r| r.enabled)
        {
            let target = match &rule.target_device_id
            {
                None => String::new(), // explicit system default
                Some(id) if present_ids.iter().any(|p| p == id) => id.clone(),
                Some(_) =>
                {
                    // Stored id vanished (driver reinstall etc). Try name match.
                    match resolve_by_name(enumerator, &rule.target_device_name)
                    {
                        Some(id) => id,
                        None => continue,
                    }
                }
            };

            for pid in pids_for_exe(sys, &rule.match_exe)
            {
                if audio_pids.binary_search(&pid).is_ok()
                {
                    desired.insert(pid, (target.clone(), rule.is_comms));
                }
            }
        }

        // 3. Apply diffs only.
        for (pid, (id, comms)) in &desired
        {
            let changed = self.applied.get(pid).map(|p| p.as_str()) != Some(id.as_str());
            if changed
            {
                let opt = if id.is_empty() { None } else { Some(id.as_str()) };
                self.engine.route_process(*pid, opt, *comms)?;
                self.applied.insert(*pid, id.clone());
            }
        }

        // 4. Revert PIDs that no longer match any rule.
        let stale: Vec<u32> = self
            .applied
            .keys()
            .filter(|pid| !desired.contains_key(pid))
            .copied()
            .collect();
        for pid in stale
        {
            if sys.process(Pid::from_u32(pid)).is_some()
            {
                self.engine.route_process(pid, None, false)?;
            }
            self.applied.remove(&pid);
        }

        Ok(())
    }

    // Revert everything we applied (shutdown / reset).
    pub fn revert_all(&mut self, sys: &System) -> windows::core::Result<()>
    {
        let pids: Vec<u32> = self.applied.keys().copied().collect();
        for pid in pids
        {
            if sys.process(Pid::from_u32(pid)).is_some()
            {
                let _ = self.engine.route_process(pid, None, false);
            }
        }
        self.applied.clear();
        Ok(())
    }

    // Nuke the OS-level persisted store (Reset routing).
    pub fn clear_os_store(&self) -> windows::core::Result<()>
    {
        self.engine.clear_all()
    }
}
