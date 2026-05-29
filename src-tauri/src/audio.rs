// Audio engine: enumerates Windows render endpoints and controls them through
// WASAPI / Core Audio COM interfaces. Default-device switching uses the
// undocumented IPolicyConfig interface, defined manually below so the vtable
// offset of SetDefaultEndpoint (slot 10 after IUnknown) lines up correctly.

use core::ffi::c_void;
use serde::Serialize;

use windows::core::{BSTR, PCWSTR, PWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::Endpoints::{IAudioEndpointVolume, IAudioMeterInformation};
use windows::Win32::Media::Audio::{
    eCommunications, eConsole, eRender, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
    DEVICE_STATE, DEVICE_STATE_ACTIVE, DEVICE_STATE_DISABLED, DEVICE_STATE_NOTPRESENT,
    DEVICE_STATE_UNPLUGGED,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};

// IPolicyConfig {f8679f50-850a-41cf-9c72-430f290290c8}
// Only SetDefaultEndpoint is actually called; the preceding ten entries are
// declared purely to align the vtable layout. Wrapped in a module so the inner
// allow can silence the COM PascalCase naming lints without passing extra
// attributes into the #[interface] macro (which rejects unknown attributes).
// The COM call is kept inside the module because the interface's generated
// methods inherit private visibility and are not callable from the outside.
mod policy_config
{
    #![allow(non_snake_case)]
    use windows::core::PCWSTR;
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
    use windows_core::GUID;

    // PolicyConfig client CLSID {870af99c-171d-4f9e-af0d-e63df40c2bc9}
    const CLSID_POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

    #[windows_core::interface("f8679f50-850a-41cf-9c72-430f290290c8")]
    unsafe trait IPolicyConfig: windows_core::IUnknown {
        unsafe fn GetMixFormat(&self) -> windows_core::HRESULT;
        unsafe fn GetDeviceFormat(&self) -> windows_core::HRESULT;
        unsafe fn ResetDeviceFormat(&self) -> windows_core::HRESULT;
        unsafe fn SetDeviceFormat(&self) -> windows_core::HRESULT;
        unsafe fn GetProcessingPeriod(&self) -> windows_core::HRESULT;
        unsafe fn SetProcessingPeriod(&self) -> windows_core::HRESULT;
        unsafe fn GetShareMode(&self) -> windows_core::HRESULT;
        unsafe fn SetShareMode(&self) -> windows_core::HRESULT;
        unsafe fn GetPropertyValue(&self) -> windows_core::HRESULT;
        unsafe fn SetPropertyValue(&self) -> windows_core::HRESULT;
        unsafe fn SetDefaultEndpoint(&self, device_id: PCWSTR, role: i32) -> windows_core::HRESULT;
        unsafe fn SetEndpointVisibility(&self) -> windows_core::HRESULT;
    }

    // Create the PolicyConfig client and switch the default endpoint for a role.
    pub(crate) fn set_default_endpoint(device_id: &str, role: i32) -> Result<(), String>
    {
        unsafe {
            let policy: IPolicyConfig = CoCreateInstance(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL)
                .map_err(|e| format!("CoCreateInstance(PolicyConfig) failed: {e}"))?;
            let wide: Vec<u16> = device_id.encode_utf16().chain(std::iter::once(0)).collect();
            let hr = policy.SetDefaultEndpoint(PCWSTR(wide.as_ptr()), role);
            if hr.is_ok()
            {
                Ok(())
            }
            else
            {
                Err(format!("SetDefaultEndpoint failed: 0x{:08X}", hr.0))
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioDevice
{
    pub id: String,
    pub name: String,
    pub state: String,
    pub volume: f32,
    pub muted: bool,
    pub is_default_audio: bool,
    pub is_default_comm: bool,
}

// Initialize COM once per thread. Tauri may dispatch commands on different
// worker threads, so we rely on a thread-local guard. RPC_E_CHANGED_MODE (when
// the thread was already initialized as STA) is harmless and ignored.
thread_local! {
    static COM_GUARD: () = {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
    };
}

fn ensure_com()
{
    COM_GUARD.with(|_| {});
}

// Convert a COM-allocated PWSTR into an owned String and free it.
unsafe fn take_pwstr(p: PWSTR) -> String
{
    if p.is_null()
    {
        return String::new();
    }
    let s = p.to_string().unwrap_or_default();
    CoTaskMemFree(Some(p.0 as *const c_void));
    s
}

fn state_to_string(state: DEVICE_STATE) -> &'static str
{
    if state == DEVICE_STATE_ACTIVE
    {
        "Active"
    }
    else if state == DEVICE_STATE_DISABLED
    {
        "Disabled"
    }
    else if state == DEVICE_STATE_NOTPRESENT
    {
        "NotPresent"
    }
    else if state == DEVICE_STATE_UNPLUGGED
    {
        "Unplugged"
    }
    else
    {
        "Unknown"
    }
}

// Read the friendly name from a device's property store.
unsafe fn device_friendly_name(device: &IMMDevice) -> String
{
    let result = (|| -> windows_core::Result<String> {
        let store = device.OpenPropertyStore(STGM_READ)?;
        // windows-core's PROPVARIANT owns and drops itself; convert the
        // VT_LPWSTR friendly name to a BSTR via PropVariantToBSTR.
        let prop = store.GetValue(&PKEY_Device_FriendlyName)?;
        let bstr = BSTR::try_from(&prop)?;
        Ok(bstr.to_string())
    })();

    match result
    {
        Ok(name) if !name.is_empty() =>
        {
            name
        }
        _ =>
        {
            "Unknown Audio Device".to_string()
        }
    }
}

// Resolve the device id strings of the current default console / comm endpoints.
unsafe fn default_endpoint_id(enumerator: &IMMDeviceEnumerator, role: windows::Win32::Media::Audio::ERole) -> String
{
    match enumerator.GetDefaultAudioEndpoint(eRender, role)
    {
        Ok(device) =>
        {
            match device.GetId()
            {
                Ok(id) =>
                {
                    take_pwstr(id)
                }
                Err(_) =>
                {
                    String::new()
                }
            }
        }
        Err(_) =>
        {
            String::new()
        }
    }
}

// Look up a device by its endpoint id string.
unsafe fn device_by_id(enumerator: &IMMDeviceEnumerator, id: &str) -> windows_core::Result<IMMDevice>
{
    let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
    enumerator.GetDevice(PCWSTR(wide.as_ptr()))
}

pub fn get_devices() -> Vec<AudioDevice>
{
    ensure_com();
    let mut out: Vec<AudioDevice> = Vec::new();

    unsafe {
        let enumerator: IMMDeviceEnumerator =
            match CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            {
                Ok(e) =>
                {
                    e
                }
                Err(_) =>
                {
                    return out;
                }
            };

        let default_console = default_endpoint_id(&enumerator, eConsole);
        let default_comm = default_endpoint_id(&enumerator, eCommunications);

        // 0x0f = mask for all device states (active|disabled|notpresent|unplugged)
        let collection = match enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE(0x0f))
        {
            Ok(c) =>
            {
                c
            }
            Err(_) =>
            {
                return out;
            }
        };

        let count = collection.GetCount().unwrap_or(0);
        for i in 0..count
        {
            let device = match collection.Item(i)
            {
                Ok(d) =>
                {
                    d
                }
                Err(_) =>
                {
                    continue;
                }
            };

            let id = match device.GetId()
            {
                Ok(p) =>
                {
                    take_pwstr(p)
                }
                Err(_) =>
                {
                    continue;
                }
            };

            let state = device.GetState().unwrap_or(DEVICE_STATE(0));
            let state_str = state_to_string(state);
            let name = device_friendly_name(&device);

            let mut volume = 0.0f32;
            let mut muted = false;
            if state == DEVICE_STATE_ACTIVE
            {
                if let Ok(vol) = device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
                {
                    volume = vol.GetMasterVolumeLevelScalar().unwrap_or(0.0);
                    muted = vol.GetMute().map(|b| b.as_bool()).unwrap_or(false);
                }
            }

            out.push(AudioDevice {
                is_default_audio: id == default_console,
                is_default_comm: id == default_comm,
                id,
                name,
                state: state_str.to_string(),
                volume,
                muted,
            });
        }
    }

    out
}

pub fn set_default_device(device_id: &str, role: i32) -> Result<(), String>
{
    ensure_com();
    policy_config::set_default_endpoint(device_id, role)
}

pub fn set_device_volume(device_id: &str, scalar: f32) -> Result<(), String>
{
    ensure_com();
    let scalar = scalar.clamp(0.0, 1.0);
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|e| format!("enumerator failed: {e}"))?;
        let device = device_by_id(&enumerator, device_id).map_err(|e| format!("device not found: {e}"))?;
        let vol = device
            .Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
            .map_err(|e| format!("activate volume failed: {e}"))?;
        vol.SetMasterVolumeLevelScalar(scalar, std::ptr::null())
            .map_err(|e| format!("set volume failed: {e}"))?;
    }
    Ok(())
}

pub fn set_device_mute(device_id: &str, mute: bool) -> Result<(), String>
{
    ensure_com();
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|e| format!("enumerator failed: {e}"))?;
        let device = device_by_id(&enumerator, device_id).map_err(|e| format!("device not found: {e}"))?;
        let vol = device
            .Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
            .map_err(|e| format!("activate volume failed: {e}"))?;
        vol.SetMute(mute, std::ptr::null())
            .map_err(|e| format!("set mute failed: {e}"))?;
    }
    Ok(())
}

pub fn get_device_peak(device_id: &str) -> f32
{
    ensure_com();
    unsafe {
        let enumerator: IMMDeviceEnumerator = match CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
        {
            Ok(e) =>
            {
                e
            }
            Err(_) =>
            {
                return 0.0;
            }
        };
        let device = match device_by_id(&enumerator, device_id)
        {
            Ok(d) =>
            {
                d
            }
            Err(_) =>
            {
                return 0.0;
            }
        };
        match device.Activate::<IAudioMeterInformation>(CLSCTX_ALL, None)
        {
            Ok(meter) =>
            {
                meter.GetPeakValue().unwrap_or(0.0)
            }
            Err(_) =>
            {
                0.0
            }
        }
    }
}
