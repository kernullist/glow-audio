# GlowAudio v2 - Per-Session Audio Routing Engine 설계

> 기존 v1("기본 출력 장치 전환기")을 유지한 채, 여러 앱을 **동시에** 서로 다른 출력 장치로
> 라우팅하는 per-session 엔진을 얹는 설계 문서. Rust(`windows` crate) + Tauri v2 기준.

---

## 0. 핵심 요약

- v1 = `IPolicyConfig::SetDefaultEndpoint` -> 시스템 **기본 출력 엔드포인트** 전환. 한 시점에 활성 출력 1개.
- v2 = `IAudioPolicyConfigFactory::SetPersistedDefaultAudioEndpoint` -> **프로세스별** 엔드포인트 지정. 동시 N개.
- v2는 v1을 대체하지 않는다. 프로파일에서 "default 모드" / "per-app 라우팅 모드"를 선택하게 하고,
  per-app 활성화 실패(미지원 빌드) 시 v1으로 자동 강등(graceful fallback)한다.

### 결정적 사실 (먼저 못 박는다)

1. `IAudioPolicyConfigFactory`는 **IInspectable 기반**(IUnknown 아님)이다. vtable 앞에
   `IUnknown(3) + IInspectable(3) = 6` 슬롯이 먼저 온다. IUnknown 기반으로 정의하면 즉시 크래시.
2. 메서드는 3개: `SetPersistedDefaultAudioEndpoint`, `GetPersistedDefaultAudioEndpoint`,
   `ClearAllPersistedApplicationDefaultEndpoints`.
3. 런타임 클래스 `Windows.Media.Internal.AudioPolicyConfig`를 `RoGetActivationFactory`로 활성화해 얻는다.
4. `Set...`이 받는 device id는 raw `IMMDevice::GetId()`가 아니라 **device-interface path** 형식이다.
   형식을 틀리면 호출은 `S_OK`인데 라우팅이 안 되는 **조용한 실패**가 난다.

> 인터페이스가 문서화되지 않은 비공개 API이므로, IID와 vtable 레이아웃은 Windows 빌드에 따라
> 달라질 수 있다. 반드시 EarTrumpet의 현재 소스(`EarTrumpet/Interop/MMDeviceAPI/`)와 대조해
> 타겟 빌드에서 검증한다.

---

## 1. 아키텍처

```
[Tauri UI thread (STA)] --command channel--> [Audio Worker thread (MTA)]
                        <--event emit-------
                                                |
                         +----------------------+-----------------------+
                         |                                              |
                 v1: Default switcher                     v2: Per-session router  (NEW)
                 IPolicyConfig::SetDefaultEndpoint         IAudioPolicyConfigFactory
                 (system-wide, 1 active)                   ::SetPersistedDefaultAudioEndpoint
                                                           (per-process, N concurrent)
                                                |
                         +----------------------+-----------------------+
                         |                      |                       |
                 Session enumerator      Reconciler loop         Device hotplug
                 IAudioSessionManager2    (rules -> diffs)        IMMNotificationClient
```

### API 역할 맵

| 목적 | 인터페이스 / 메서드 |
| --- | --- |
| 장치 열거 (v1과 공유) | `IMMDeviceEnumerator::EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)`, `IMMDevice::GetId` |
| per-app 라우팅 적용 | `IAudioPolicyConfigFactory::SetPersistedDefaultAudioEndpoint(pid, flow, role, hstring)` |
| per-app 현재값 조회 | `IAudioPolicyConfigFactory::GetPersistedDefaultAudioEndpoint(...)` |
| 전체 초기화 | `IAudioPolicyConfigFactory::ClearAllPersistedApplicationDefaultEndpoints()` |
| 실제 오디오 PID 발견 | `IAudioSessionManager2::GetSessionEnumerator` -> `IAudioSessionControl2::GetProcessId` |
| 디바이스 핫플러그 | `IMMNotificationClient` |
| 세션 생성 즉시 적용(옵션) | `IAudioSessionManager2::RegisterSessionNotification` (+ priming quirk) |

---

## 2. 스레딩 모델 (가장 먼저 잡는다)

per-session 라우팅과 세션 알림은 **MTA 전용 워커 스레드**에서 돌린다. Tauri 메인 스레드는 WebView2 STA라
거기서 `IAudioPolicyConfigFactory`를 활성화하면 콜백 아파트먼트가 꼬인다. UI <-> 워커는 채널로 통신한다.

```rust
// lib.rs - audio worker bootstrap

use std::sync::mpsc::{channel, Sender, Receiver};

pub enum AudioCommand
{
    Reconcile,                          // re-evaluate all routing rules
    SetRule(RoutingRule),               // upsert one rule then reconcile
    RemoveRule(String),                 // by match_exe, then revert + reconcile
    ClearAll,                           // nuke all per-app overrides
    Shutdown,                           // revert applied rules and exit
}

pub fn spawn_audio_worker(app: tauri::AppHandle) -> Sender<AudioCommand>
{
    let (tx, rx) = channel::<AudioCommand>();
    let tx_for_watchers = tx.clone();

    std::thread::spawn(move ||
    {
        // RO_INIT_MULTITHREADED: required for policy config + session notifications.
        unsafe
        {
            use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
            if RoInitialize(RO_INIT_MULTITHREADED).is_err()
            {
                // Fall back to plain COM MTA if WinRT was already initialized differently.
                use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
        }

        if let Err(e) = audio_worker_loop(app, rx, tx_for_watchers)
        {
            log::error!("audio worker exited with error: {:?}", e);
        }

        unsafe { windows::Win32::System::WinRT::RoUninitialize(); }
    });

    tx
}
```

---

## 3. 비공개 인터페이스 정의 + 활성화 (린치핀)

```rust
// audio_router.rs

use windows::core::{interface, IInspectable, HSTRING, HRESULT, Result, Interface};
use windows::Win32::Media::Audio::{EDataFlow, ERole, eRender, eConsole, eMultimedia, eCommunications};
use windows::Win32::System::WinRT::RoGetActivationFactory;

// IInspectable-based. Real vtable = IUnknown(3) + IInspectable(3) + these 3 methods.
// IID below is the modern (Win10 1803+/Win11) variant used by EarTrumpet.
// VERIFY against EarTrumpet's current AudioPolicyConfigFactory.cs for your target builds.
// Legacy fallback IID (older method layout): ab3d4648-e242-459f-b02f-541c70306324
#[interface("2a59116d-6c4f-45e0-a74f-707e3fef9258")]
unsafe trait IAudioPolicyConfigFactory: IInspectable
{
    // device_id is an [in] HSTRING handle passed by value (caller frees after return).
    // Empty HSTRING means "revert this process to the system default".
    unsafe fn SetPersistedDefaultAudioEndpoint(
        &self, process_id: u32, flow: EDataFlow, role: ERole, device_id: HSTRING) -> HRESULT;

    unsafe fn GetPersistedDefaultAudioEndpoint(
        &self, process_id: u32, flow: EDataFlow, role: ERole, device_id: *mut HSTRING) -> HRESULT;

    unsafe fn ClearAllPersistedApplicationDefaultEndpoints(&self) -> HRESULT;
}

const RUNTIME_CLASS: &str = "Windows.Media.Internal.AudioPolicyConfig";

fn create_factory() -> Result<IAudioPolicyConfigFactory>
{
    let class_id = HSTRING::from(RUNTIME_CLASS);
    // RoGetActivationFactory returns the requested interface directly.
    let factory: IAudioPolicyConfigFactory = unsafe { RoGetActivationFactory(&class_id)? };
    Ok(factory)
}
```

> 위 IID로 활성화는 되는데 호출이 이상하면, 타겟 빌드에서 vtable에 reserved 슬롯이 끼었거나
> legacy 레이아웃이다. legacy IID로 별도 trait을 정의해 try-fallback 한다(섹션 13의 강등 경로 참고).

---

## 4. 엔드포인트 ID 포맷 헬퍼

```rust
// audio_router.rs

// EarTrumpet-confirmed format:
//   \\?\SWD#MMDEVAPI#{mmdevice_id}#{interface_guid}
const DEVINTERFACE_RENDER:  &str = "{e6327cad-dcec-4949-ae8a-991e976a79d2}";
const DEVINTERFACE_CAPTURE: &str = "{2eef81be-33fa-4800-9670-1cd474972c3f}";

fn make_endpoint_id(mmdevice_id: &str, flow: EDataFlow) -> HSTRING
{
    // Empty -> system default for this process.
    if mmdevice_id.is_empty()
    {
        return HSTRING::new();
    }
    let guid = if flow == eRender { DEVINTERFACE_RENDER } else { DEVINTERFACE_CAPTURE };
    HSTRING::from(format!("\\\\?\\SWD#MMDEVAPI#{}#{}", mmdevice_id, guid))
}
```

---

## 5. RoutingEngine - route / revert / query

```rust
// audio_router.rs

pub struct RoutingEngine
{
    factory: IAudioPolicyConfigFactory,
}

impl RoutingEngine
{
    // Must be constructed on the MTA worker thread.
    pub fn new() -> Result<Self>
    {
        Ok(Self { factory: create_factory()? })
    }

    // Route one PID to a device. mmdevice_id = None -> revert to system default.
    // Console + Multimedia cover normal playback; Communications added for comms apps.
    pub fn route_process(&self, pid: u32, mmdevice_id: Option<&str>, is_comms: bool) -> Result<()>
    {
        let id_str = mmdevice_id.unwrap_or("");
        let endpoint = make_endpoint_id(id_str, eRender);

        let mut roles: Vec<ERole> = vec![eConsole, eMultimedia];
        if is_comms
        {
            roles.push(eCommunications);
        }

        let mut worst: Result<()> = Ok(());
        for role in roles
        {
            // Clone per call: each call consumes one [in] HSTRING (caller-owned, caller-freed).
            let hs = endpoint.clone();
            let hr = unsafe { self.factory.SetPersistedDefaultAudioEndpoint(pid, eRender, role, hs) };
            if hr.is_err()
            {
                log::warn!("SetPersistedDefaultAudioEndpoint pid={} role={:?} hr={:?}", pid, role, hr);
                worst = hr.ok();
            }
        }
        worst
    }

    // Returns the device-interface path currently persisted for this PID ("" if default).
    pub fn query_process(&self, pid: u32) -> Result<String>
    {
        let mut out = HSTRING::new();
        let hr = unsafe { self.factory.GetPersistedDefaultAudioEndpoint(pid, eRender, eConsole, &mut out) };
        hr.ok()?;
        Ok(out.to_string())
    }

    // Nuke all per-app overrides. Bind this to a visible "Reset routing" button.
    pub fn clear_all(&self) -> Result<()>
    {
        unsafe { self.factory.ClearAllPersistedApplicationDefaultEndpoints().ok() }
    }
}
```

---

## 6. 오디오 PID 발견 (Chrome/Discord 문제 해결)

per-app 라우팅이 "안 되는" 1순위 원인: **오디오는 메인 PID가 아니라 렌더러/자식 프로세스에서 난다**.
Chrome/Discord/Electron이 전부 그렇다. exe 이름만 보고 메인 PID에 적용하면 무음 실패한다. 실제 오디오
세션을 가진 PID를 찾아 교집합으로 좁힌다.

```rust
// audio_router.rs

use windows::Win32::Media::Audio::{
    IMMDeviceEnumerator, IAudioSessionManager2, IAudioSessionControl2, DEVICE_STATE_ACTIVE};
use windows::Win32::System::Com::CLSCTX_ALL;

// All distinct PIDs that currently have an active render session across all devices.
pub fn collect_audio_pids(enumerator: &IMMDeviceEnumerator) -> Result<Vec<u32>>
{
    let mut pids = Vec::new();
    let devices = unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)? };
    let count = unsafe { devices.GetCount()? };

    for i in 0..count
    {
        let device = unsafe { devices.Item(i)? };
        let mgr: IAudioSessionManager2 = unsafe { device.Activate(CLSCTX_ALL, None)? };
        let sessions = unsafe { mgr.GetSessionEnumerator()? };
        let scount = unsafe { sessions.GetCount()? };

        for s in 0..scount
        {
            let ctrl = unsafe { sessions.GetSession(s)? };
            let ctrl2: IAudioSessionControl2 = ctrl.cast()?;
            let pid = unsafe { ctrl2.GetProcessId()? };
            if pid != 0   // pid 0 == system sounds session
            {
                pids.push(pid);
            }
        }
    }

    pids.sort_unstable();
    pids.dedup();
    Ok(pids)
}
```

---

## 7. 헬퍼 - 프로세스 매칭 / 장치 이름 해결

> sysinfo API는 버전에 따라 `Process::name()` 반환형이 `&str`/`&OsStr`로 다르다. 아래는 sysinfo 0.30+
> (`&OsStr`) 기준. 사용 중인 버전에 맞춰 `.to_string_lossy()` 호출만 조정한다.

```rust
// audio_router.rs

use sysinfo::{System, Pid};

// All PIDs whose process name matches the rule (case-insensitive).
// Child renderers of Chrome/Discord/Electron usually share the parent exe name,
// so a name match + audio-PID intersection (in the reconciler) catches them.
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

// Optional: expand a PID set to include descendants (for apps whose audio child
// has a DIFFERENT exe name than the parent). Most apps do not need this.
pub fn expand_with_children(sys: &System, roots: &[u32]) -> Vec<u32>
{
    use std::collections::HashSet;
    let root_set: HashSet<u32> = roots.iter().copied().collect();
    let mut out: HashSet<u32> = root_set.clone();

    // Single pass is enough for one generation; loop if you need deep trees.
    for (pid, proc_) in sys.processes()
    {
        if let Some(parent) = proc_.parent()
        {
            if root_set.contains(&parent.as_u32())
            {
                out.insert(pid.as_u32());
            }
        }
    }

    let mut v: Vec<u32> = out.into_iter().collect();
    v.sort_unstable();
    v
}
```

장치 ID 드리프트(드라이버 재설치 시 MMDevice ID 변경) 대비, friendly name으로 재해결한다.

```rust
// audio_router.rs

use windows::Win32::Media::Audio::IMMDevice;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::System::Com::STGM_READ;
use windows::Win32::UI::Shell::PropertiesSystem::PropVariantToStringAlloc;

// Read PKEY_Device_FriendlyName from a device property store.
// NOTE: the PROPVARIANT module path varies across windows-crate versions;
// PropVariantToStringAlloc is the stable way to stringify it.
fn friendly_name_of(device: &IMMDevice) -> Option<String>
{
    let store = unsafe { device.OpenPropertyStore(STGM_READ) }.ok()?;
    let prop = unsafe { store.GetValue(&PKEY_Device_FriendlyName) }.ok()?;
    let pwstr = unsafe { PropVariantToStringAlloc(&prop) }.ok()?;
    let name = unsafe { pwstr.to_string() }.ok()?;
    unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(pwstr.0 as *const _)); }
    Some(name)
}

// Resolve a current MMDevice id by friendly name (returns the raw GetId string).
pub fn resolve_by_name(enumerator: &IMMDeviceEnumerator, friendly: &str) -> Option<String>
{
    let devices = unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }.ok()?;
    let count = unsafe { devices.GetCount() }.ok()?;

    for i in 0..count
    {
        let device = unsafe { devices.Item(i) }.ok()?;
        if friendly_name_of(&device).as_deref() == Some(friendly)
        {
            let id = unsafe { device.GetId() }.ok()?;
            return unsafe { id.to_string() }.ok();
        }
    }
    None
}

// List currently-present render endpoints as (mmdevice_id, friendly_name).
// Used by the reconciler (availability check) and by the UI device dropdown.
pub fn list_render_devices(enumerator: &IMMDeviceEnumerator) -> Result<Vec<(String, String)>>
{
    let mut out = Vec::new();
    let devices = unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)? };
    let count = unsafe { devices.GetCount()? };

    for i in 0..count
    {
        let device = unsafe { devices.Item(i)? };
        let id = unsafe { device.GetId()? };
        let id = unsafe { id.to_string()? };
        let name = friendly_name_of(&device).unwrap_or_else(|| id.clone());
        out.push((id, name));
    }
    Ok(out)
}
```

---

## 8. 규칙 모델 + Reconciler

```rust
// audio_router.rs

use std::collections::HashMap;
use serde::{Serialize, Deserialize};

#[derive(Clone, Serialize, Deserialize)]
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
    applied: HashMap<u32, String>,  // pid -> last applied id ("" = default)
}

impl Reconciler
{
    pub fn new(engine: RoutingEngine) -> Self
    {
        Self { engine, applied: HashMap::new() }
    }

    // Recompute desired routing and apply only diffs.
    // Trigger on: process start, session created, device hotplug, profile change.
    pub fn reconcile(
        &mut self,
        rules: &[RoutingRule],
        sys: &System,
        enumerator: &IMMDeviceEnumerator) -> Result<()>
    {
        // 1. Which PIDs actually have audio right now, and which ids are present.
        let audio_pids = collect_audio_pids(enumerator)?;
        let present_ids: Vec<String> = list_render_devices(enumerator)?
            .into_iter()
            .map(|(id, _name)| id)
            .collect();

        // 2. Build desired: pid -> (device_id, is_comms).
        let mut desired: HashMap<u32, (String, bool)> = HashMap::new();
        for rule in rules.iter().filter(|r| r.enabled)
        {
            // Resolve target id; fall back to name match if the stored id vanished.
            let target = match &rule.target_device_id
            {
                None => String::new(),  // explicit system default
                Some(id) if present_ids.iter().any(|p| p == id) => id.clone(),
                Some(_) =>
                {
                    // Stored id not present (driver reinstall etc). Try name match.
                    // If still unresolved, skip: app uses system default until hotplug.
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

        // 4. Revert PIDs that no longer match any rule (back to system default).
        let stale: Vec<u32> = self.applied.keys()
            .filter(|pid| !desired.contains_key(pid))
            .copied()
            .collect();

        for pid in stale
        {
            // Only revert if the process still exists; dead PIDs are dropped silently.
            if sys.process(Pid::from_u32(pid)).is_some()
            {
                self.engine.route_process(pid, None, false)?;
            }
            self.applied.remove(&pid);
        }

        Ok(())
    }

    // Revert everything we applied (call on shutdown / mode switch to v1).
    pub fn revert_all(&mut self, sys: &System) -> Result<()>
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
}
```

---

## 9. 디바이스 핫플러그 (IMMNotificationClient)

타겟 장치가 빠졌다 들어올 때 재적용하려면 알림 클라이언트가 필요하다. 콜백은 짧게 끝내고 워커로
`Reconcile`만 보낸다(디바운스는 워커에서).

```rust
// audio_router.rs

use windows::core::{implement, PCWSTR};
use windows::Win32::Media::Audio::{IMMNotificationClient, IMMNotificationClient_Impl, DEVICE_STATE};
use windows::Win32::Foundation::PROPERTYKEY;
use std::sync::mpsc::Sender;

#[implement(IMMNotificationClient)]
pub struct DeviceWatcher
{
    pub tx: Sender<AudioCommand>,
}

impl IMMNotificationClient_Impl for DeviceWatcher_Impl
{
    fn OnDeviceStateChanged(&self, _id: &PCWSTR, _state: DEVICE_STATE) -> Result<()>
    {
        let _ = self.tx.send(AudioCommand::Reconcile);
        Ok(())
    }

    fn OnDeviceAdded(&self, _id: &PCWSTR) -> Result<()>
    {
        let _ = self.tx.send(AudioCommand::Reconcile);
        Ok(())
    }

    fn OnDeviceRemoved(&self, _id: &PCWSTR) -> Result<()>
    {
        let _ = self.tx.send(AudioCommand::Reconcile);
        Ok(())
    }

    fn OnDefaultDeviceChanged(&self, _flow: EDataFlow, _role: ERole, _id: &PCWSTR) -> Result<()>
    {
        Ok(())
    }

    fn OnPropertyValueChanged(&self, _id: &PCWSTR, _key: &PROPERTYKEY) -> Result<()>
    {
        Ok(())
    }
}

// Registration (keep the IMMNotificationClient instance alive for the worker lifetime):
//   let watcher: IMMNotificationClient = DeviceWatcher { tx }.into();
//   unsafe { enumerator.RegisterEndpointNotificationCallback(&watcher)?; }
//   // ... on shutdown:
//   unsafe { enumerator.UnregisterEndpointNotificationCallback(&watcher)?; }
```

---

## 10. (옵션) 세션 생성 즉시 적용 - IAudioSessionNotification

2초 폴링 + 핫플러그만으로도 실용적으로 충분하다. 더 빠른 반응이 필요할 때만 추가한다.

> **알려진 함정**: `RegisterSessionNotification` 등록 전에 같은 매니저에서 `GetSessionEnumerator()`를
> 한 번 호출해 "prime" 하지 않으면 `OnSessionCreated`가 영영 호출되지 않는다(Windows의 오래된 버그).

```rust
// audio_router.rs

use windows::Win32::Media::Audio::{
    IAudioSessionNotification, IAudioSessionNotification_Impl, IAudioSessionControl};

#[implement(IAudioSessionNotification)]
pub struct SessionWatcher
{
    pub tx: Sender<AudioCommand>,
}

impl IAudioSessionNotification_Impl for SessionWatcher_Impl
{
    fn OnSessionCreated(&self, _new_session: windows::core::Ref<'_, IAudioSessionControl>) -> Result<()>
    {
        // New stream appeared; reconcile so its rule (if any) applies immediately.
        let _ = self.tx.send(AudioCommand::Reconcile);
        Ok(())
    }
}

// Per device manager registration:
//   let mgr: IAudioSessionManager2 = unsafe { device.Activate(CLSCTX_ALL, None)? };
//   let _prime = unsafe { mgr.GetSessionEnumerator()? }; // REQUIRED priming call
//   let notif: IAudioSessionNotification = SessionWatcher { tx }.into();
//   unsafe { mgr.RegisterSessionNotification(&notif)?; }
//   // Note: callbacks fire only while the IAudioSessionManager2 stays alive.
```

> `Ref<'_, T>` 시그니처는 windows-crate 버전에 따라 `Option<&T>` 형태일 수 있다. 사용 중인 버전의
> `IAudioSessionNotification_Impl` 시그니처에 맞춘다.

---

## 11. 워커 루프 + Tauri 커맨드 글루

```rust
// lib.rs

use windows::Win32::Media::Audio::{IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

fn audio_worker_loop(
    app: tauri::AppHandle,
    rx: Receiver<AudioCommand>,
    tx: Sender<AudioCommand>) -> Result<()>
{
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };

    // Device hotplug -> Reconcile.
    let watcher: IMMNotificationClient = DeviceWatcher { tx: tx.clone() }.into();
    unsafe { enumerator.RegisterEndpointNotificationCallback(&watcher)?; }

    // Build engine; on failure, disable v2 and let the UI fall back to v1.
    let engine = match RoutingEngine::new()
    {
        Ok(e) => e,
        Err(e) =>
        {
            log::warn!("per-app routing unavailable on this build: {:?}. Falling back to v1.", e);
            let _ = app.emit("routing-unavailable", ());
            // Still service the channel so v1 commands keep flowing if multiplexed here.
            return Ok(());
        }
    };

    let mut reconciler = Reconciler::new(engine);
    let mut sys = System::new();
    let mut rules: Vec<RoutingRule> = load_rules_from_profile(&app);

    // Periodic safety-net reconcile (process start without session notifications).
    let ticker = tx.clone();
    std::thread::spawn(move ||
    {
        loop
        {
            std::thread::sleep(std::time::Duration::from_secs(2));
            if ticker.send(AudioCommand::Reconcile).is_err()
            {
                break;
            }
        }
    });

    loop
    {
        match rx.recv()
        {
            Ok(AudioCommand::Reconcile) =>
            {
                sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
                if let Err(e) = reconciler.reconcile(&rules, &sys, &enumerator)
                {
                    log::warn!("reconcile failed: {:?}", e);
                }
            }
            Ok(AudioCommand::SetRule(rule)) =>
            {
                upsert_rule(&mut rules, rule);
                save_rules_to_profile(&app, &rules);
                let _ = tx.send(AudioCommand::Reconcile);
            }
            Ok(AudioCommand::RemoveRule(exe)) =>
            {
                rules.retain(|r| r.match_exe != exe);
                save_rules_to_profile(&app, &rules);
                sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
                // revert handled by reconcile's stale-revert path.
                let _ = tx.send(AudioCommand::Reconcile);
            }
            Ok(AudioCommand::ClearAll) =>
            {
                let _ = reconciler.revert_all(&sys);
                // Also clear the OS-level persisted store so nothing lingers.
                // (engine.clear_all is exposed via reconciler if you add a passthrough)
            }
            Ok(AudioCommand::Shutdown) =>
            {
                let _ = reconciler.revert_all(&sys);
                break;
            }
            Err(_) =>
            {
                // Channel closed: revert and exit cleanly.
                let _ = reconciler.revert_all(&sys);
                break;
            }
        }
    }

    unsafe { enumerator.UnregisterEndpointNotificationCallback(&watcher)?; }
    Ok(())
}
```

Tauri 커맨드:

```rust
// lib.rs

#[tauri::command]
fn set_routing_rule(state: tauri::State<AudioTx>, rule: RoutingRule) -> std::result::Result<(), String>
{
    state.0.send(AudioCommand::SetRule(rule)).map_err(|e| e.to_string())
}

#[tauri::command]
fn remove_routing_rule(state: tauri::State<AudioTx>, match_exe: String) -> std::result::Result<(), String>
{
    state.0.send(AudioCommand::RemoveRule(match_exe)).map_err(|e| e.to_string())
}

#[tauri::command]
fn clear_routing(state: tauri::State<AudioTx>) -> std::result::Result<(), String>
{
    state.0.send(AudioCommand::ClearAll).map_err(|e| e.to_string())
}

// AudioTx wraps Sender<AudioCommand> and is managed via app.manage(AudioTx(tx)).
pub struct AudioTx(pub Sender<AudioCommand>);
```

---

## 12. 프론트엔드 글루 (api.ts + Routing 탭 스케치)

```ts
// src/api.ts
import { invoke } from "@tauri-apps/api/core";

export interface RoutingRule
{
    match_exe: string;
    target_device_id: string | null;   // null -> system default
    target_device_name: string;
    is_comms: boolean;
    enabled: boolean;
}

export async function setRoutingRule(rule: RoutingRule): Promise<void>
{
    await invoke("set_routing_rule", { rule });
}

export async function removeRoutingRule(matchExe: string): Promise<void>
{
    await invoke("remove_routing_rule", { matchExe });
}

export async function clearRouting(): Promise<void>
{
    await invoke("clear_routing");
}
```

```tsx
// src/RoutingTab.tsx (sketch)
// Lists active audio sessions (friendly name + exe) with a per-app device dropdown.
// Persisted rules render as chips; a visible "Reset routing" button calls clearRouting().
//
// UX rules to honor:
//  - Show "uses its own device" for apps in exclusive mode (cannot be forced).
//  - "Reset routing" must be prominent: per-app overrides persist in the OS store.
//  - Comms toggle per rule (adds the Communications role for Discord/Teams/etc).
```

---

## 13. 기존 코드 통합 절차

1. `monitor.rs`의 2초 프로세스 스캐너를 재사용한다. 게임 매칭 분기 옆에 "라우팅 규칙 매칭" 분기를
   더하고, 변화 감지 시 `AudioCommand::Reconcile`을 워커로 보낸다. (또는 워커 내부 2초 ticker로 통일)
2. `lib.rs`에 MTA 워커(섹션 2)를 추가하고, 기존 v1 default-switch 호출도 이 워커로 옮겨 COM
   아파트먼트를 단일화한다.
3. `glow_profiles.json` 스키마에 `routing_rules: Vec<RoutingRule>`를 추가(기존 게임 매핑과 공존).
4. UI에 "Routing" 탭을 추가하고 `api.ts` 래퍼를 연결한다.
5. 종료 훅(window close)과 "Reset routing" 버튼에 revert/clear를 반드시 연결한다.
6. **강등 경로**: `RoutingEngine::new()` 실패 시 `routing-unavailable` 이벤트를 emit하고 UI에서
   per-app 모드를 잠근 뒤 v1 default-switch만 노출한다. 절대 패닉하지 않는다.

---

## 14. 엣지 케이스 / 실패 경로 (운영상 중요)

1. **자식 프로세스 오디오** (Chrome/Discord/Electron): 섹션 6 필수. 메인 PID만 잡으면 무음 실패.
   대부분 자식이 부모와 exe 이름을 공유하므로 name match + audio-PID 교집합으로 해결된다. 이름이
   다른 경우만 `expand_with_children`로 보강.
2. **Persisted store 오염**: `SetPersistedDefaultAudioEndpoint`는 레지스트리 백업 영구 저장소에 앱
   단위로 남는다. 규칙 삭제/앱 종료/GlowAudio 제거 시 **반드시 revert**하지 않으면 유령 라우팅이
   남는다. `clear_all()`을 눈에 보이는 버튼으로 노출한다.
3. **Exclusive mode / 자체 장치 선택 앱** (일부 게임, 프로 오디오, ASIO): persisted endpoint를
   무시한다. 강제 불가 - UI에서 "이 앱은 자체 장치를 사용 중"으로 정직하게 표시한다.
4. **적용 타이밍**: 프로세스 시작 시점(스트림 열기 전)에 미리 적용하는 게 이상적이다. 앱이 첫
   스트림을 만들 때 persisted 값을 읽기 때문. session-created 재적용은 안전망.
5. **장치 ID 드리프트**: 드라이버 재설치 시 MMDevice ID가 바뀐다. 규칙에 friendly name을 같이
   저장해 이름으로 재해결(섹션 7).
6. **버전 미지원 / 활성화 실패**: v2 토글 비활성화 + v1 강등(섹션 13-6). 패닉 금지.
7. **HSTRING 수명**: `[in]` 문자열은 호출자 소유다. 호출당 `clone()` 후 by-value 전달(섹션 5).
   한 핸들을 여러 role 호출에 돌려쓰면 double-free 위험.
8. **role 커버리지**: 일반 앱은 Console + Multimedia, 통신 앱은 Communications 추가. 규칙별 토글.
9. **세션 알림 priming**: `RegisterSessionNotification` 전에 `GetSessionEnumerator()` 1회 필수(섹션 10).

---

## 15. 검증 / 롤아웃 순서

각 단계에서 어디가 깨지는지 격리하려면 다음 순서로 올린다.

1. **인터페이스 활성화 확정**: `create_factory()` 성공 + `query_process(self_pid)` 왕복 확인.
   여기서 IID/vtable이 타겟 빌드에서 맞는지부터 못 박는다(legacy IID fallback 포함).
2. **단일 PID 수동 라우팅**: 메모장 같은 단일 프로세스 앱을 손으로 라우팅 -> 소리가 옮겨가는지.
3. **Chrome 자식 PID 라우팅**: 섹션 6 교집합 로직으로 실제 렌더러 PID에 적용되는지.
4. **Reconciler 자동화**: 규칙 추가/삭제/장치 핫플러그/앱 재실행에서 diff 적용과 revert가 맞는지.
5. **정리 보장**: 종료/Reset에서 OS persisted store가 깨끗이 복구되는지(다른 도구로 교차 확인).

---

## 16. 참고 (검증 대상)

- EarTrumpet - `IAudioPolicyConfigFactory` 인터페이스 정의 및 활성화 코드 (IID/vtable 권위 소스):
  `File-New-Project/EarTrumpet` 리포지토리 `EarTrumpet/Interop/MMDeviceAPI/`.
- 비공개 API 특성상 Microsoft가 빌드마다 변경할 수 있으므로, 타겟 Windows 빌드에서 항상 재검증한다.
