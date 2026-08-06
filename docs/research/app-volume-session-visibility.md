# 연구 노트: App Volume 목록에서 실행 중인 앱이 사라지는 조건

- 작성일: 2026-08-07
- 상태: 결론 도출
- 관련: [[docs/plan/2026-08-07-app-volume-session-visibility]]

## 질문

Naver Whale이 실행 중인데도 App Volume 탭에 나타나지 않는 현상의 원인은 무엇인가.
`audio_volume.rs`의 세션 열거 로직 버그인가, 아니면 WASAPI 세션 수명 특성인가.

## 결론 (먼저 쓴다)

**열거 코드의 버그가 아니다.** GlowAudio의 App Volume 목록은 두 조건을 동시에 만족하는 앱만 보여준다.

1. 그 앱이 **지금 WASAPI 렌더 스트림을 잡고 있어야** 한다 (세션이 열거자에 존재).
2. 그 스트림이 걸린 엔드포인트가 **지금 `DEVICE_STATE_ACTIVE` 여야** 한다.

이 사용자 환경에서는 두 조건이 모두 수시로 깨진다.

- Whale/Chrome 같은 Chromium 계열은 재생이 멈추면 오디오 스트림을 반납하고, 그러면 세션이 열거자에서 **완전히 제거**된다 (Inactive 상태로 남는 게 아니라 사라진다).
- 이 PC의 기본 재생 장치 중 하나가 블루투스 이어버드 `헤드폰(3- QCY-T13)` 이고, 이 엔드포인트가 `ACTIVE` <-> `UNPLUGGED` 를 오간다. `UNPLUGGED` 가 되는 순간 **그 엔드포인트에 붙어 있던 모든 앱 세션이 목록에서 한꺼번에 사라진다.**

즉 사용자 입장에서는 "실행 중인 앱이 믹서에서 없어졌다"로 보이고, 앱을 재시작하면 그 사이 상태가 바뀌어 다시 보이므로 "재시작하니 고쳐졌다"로 오인된다.

## 환경

- OS: Windows 11 Pro 10.0.26200
- glow-audio: v0.5.0 릴리스 빌드, `C:\Tools\glow-audio.exe`
  (SHA256 `73F31755597A9C5D42F293603BCCB577A1C98DF76ECAC044C5DECF8C2D598140`,
  `src-tauri/target/release/glow-audio.exe` 와 동일 = HEAD 빌드 맞음)
- 대상: Naver Whale 4.39.410.6 (프로세스 25개, 오디오 세션 보유 PID 29392)
- 도구: windows-rs 0.58 / sysinfo 0.32 재현 크레이트, PowerShell + C# COM interop 덤프

## 실험 및 관찰

### 실험 1: Whale이 실제로 오디오 세션을 갖고 있는가

- 가설: Whale에 세션이 아예 없어서 안 보이는 것이다.
- 절차: C# COM interop으로 `IMMDeviceEnumerator::EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)`
  -> `IAudioSessionManager2::GetSessionEnumerator` -> 각 세션의 `GetProcessId`/`GetState` 덤프.
- 관찰 (2026-08-07 00:31):
  ```
  render endpoints: 3
  [dev 0] {0.0.0.00000000}.{3fc2224a-6961-4dff-b2a8-a393cdf33ea6}  sessions=2
     pid=29392 state=1 proc=whale
     sid=...|\Device\HarddiskVolume3\Program Files\Naver\Naver Whale\...\whale.exe%b{...}
     pid=0 state=0 (System Sounds)
  ```
- 해석: 그 시점에는 세션이 **있었고 Active** 였다. 세션 부재가 상시 원인은 아니다.

### 실험 2: 현재 소스의 열거 로직 그대로 재현

- 가설: `enumerate_sessions` / `names_for` 에 버그가 있어 특정 PID를 흘린다.
- 절차: `audio_volume.rs:42` 의 `enumerate_sessions` 와 `names_for` 를 동일 크레이트 버전
  (windows 0.58, sysinfo 0.32), 동일 조건(MTA COM, 동일 유저/세션, 비상승)으로 복제 실행.
- 관찰:
  ```
  collected pids: [29392, 26240, 24696]
  names: {29392: "whale.exe", 26240: "mstsc.exe", 24696: "spotifywidgetprovider.exe"}
  ```
- 해석: 열거도 이름 해석도 정상. 코드 버그 아님.

### 실험 3: 장시간 폴링에서 새 세션이 잡히는가 (WASAPI 세션 캐시 의혹)

- 가설: `IAudioSessionEnumerator` 가 최초 스냅샷을 캐시해서, 프로세스 시작 후 생성된 세션을
  영영 못 본다 (널리 알려진 WASAPI 함정).
- 절차: 2초 간격 30회 폴링(앱과 동일하게 매 tick `IMMDeviceEnumerator` 재생성) 도중,
  별도 프로세스(`pwsh` + `SoundPlayer`)를 띄워 새 세션을 만든다.
- 관찰:
  ```
  tick   9: mstsc.exe(26240 st=0) potplayer64.exe(11968 st=1) spotifywidgetprovider.exe(24696 st=0) whale.exe(29392 st=1)
  tick  10: mstsc.exe(26240 st=0) potplayer64.exe(11968 st=1) pwsh.exe(17372 st=1) spotifywidgetprovider.exe(24696 st=0) whale.exe(29392 st=1)
  ```
- 해석: 새 세션(`pwsh.exe`)이 tick 10에 즉시 잡혔다. **캐시 가설 반증.**
  매 tick 새 enumerator + 새 `Activate` 를 하면 목록은 최신이다.

### 실험 4: 핸들 누수 / 장시간 열화 (가속 soak)

- 가설: 1.5초 폴링 x 장시간 -> sysinfo의 `OpenProcess` 핸들 누수 -> 이름 해석 실패 ->
  `audio_volume.rs:142` 의 `None => continue` 로 세션이 조용히 드롭된다.
- 절차: `list_app_sessions` 전체 경로를 무지연으로 반복하며 `GetProcessHandleCount` 추적.
- 관찰 (30,000회 완주 / 326.9초, 약 10.9ms/회):
  ```
  start handles=146
  iter      0 sessions=1 handles=202 fails[endp=0 act=0 senum=0 ctrl=0 namemiss=0]
  iter  15000 sessions=2 handles=204 fails[endp=0 act=0 senum=0 ctrl=0 namemiss=0]
  iter  29500 sessions=1 handles=206 fails[endp=0 act=0 senum=0 ctrl=0 namemiss=0]
  DONE          handles=206 elapsed=326.9267876s fails[endp=0 act=0 senum=0 ctrl=0 namemiss=0]
  ```
- 해석: 핸들 146->206에서 평탄(전 구간 최대 +60, 증가 추세 없음), COM 실패 0건,
  이름 미스 0건. **누수 가설 반증.**
  30,000회는 UI 폴링(1.5초) 기준 약 12.5시간 분량이며, 그동안 열화 징후가 전혀 없다.

### 실험 5: 세션이 사라지는 실제 순간 포착

- 절차: 실험 1과 동일한 덤프를 시간차로 반복 + 전체 엔드포인트 상태를 `DEVICE_STATEMASK_ALL` 로 덤프.
- 관찰 (2026-08-07 00:52, Whale 프로세스 12개 여전히 실행 중):
  ```
  render endpoints: 2          <-- 직전 3개에서 감소
  [dev 0] {...85b9b434...}  sessions=1   (System Sounds only)
  [dev 1] {...9a7c675a...}  sessions=2   (SpotifyWidgetProvider, System Sounds)
  ```
  전체 엔드포인트 덤프:
  ```
  [UNPLUGGED ] 헤드폰(3- QCY-T13)
               {0.0.0.00000000}.{3fc2224a-6961-4dff-b2a8-a393cdf33ea6}
  [ACTIVE    ] Optix AG32C(NVIDIA High Definition Audio)
               {0.0.0.00000000}.{85b9b434-ee41-4df7-b1e2-c3b288657a4a}
  [ACTIVE    ] Digital Audio (S/PDIF)(High Definition Audio Device)  <== DEFAULT
               {0.0.0.00000000}.{9a7c675a-0e24-4ca2-8b2e-1fa40d3c39d1}
  ```
- 해석: **결정적.** Whale/Chrome/PotPlayer/mstsc의 세션이 전부 붙어 있던
  `{3fc2224a-...}` 는 블루투스 이어버드 `헤드폰(3- QCY-T13)` 이고, 지금 `UNPLUGGED` 다.
  `EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)` 가 이 엔드포인트를 더 이상 돌려주지
  않으므로, 그 위의 모든 세션이 App Volume에서 통째로 증발한다.
  동시에 Whale은 재생을 멈춘 뒤 스트림을 반납해 다른 엔드포인트에도 세션이 없다.

## 반증 / 실패한 시도

같은 길을 다시 파지 않도록 기록한다.

- **`audio_volume.rs` 열거 코드 버그** — 아님 (실험 2). 코드를 그대로 복제해도 정상 동작.
- **`IAudioSessionEnumerator` 스냅샷 캐시** — 아님 (실험 3). 매 회 `Activate` 하면 최신 목록이 온다.
- **sysinfo 핸들 누수로 인한 이름 해석 실패** — 아님 (실험 4). 15,000회에도 핸들/실패 평탄.
- **설치된 바이너리가 구버전** — 아님. `C:\Tools\glow-audio.exe` 와
  `target\release\glow-audio.exe` 의 SHA256 동일.
- **권한/무결성 수준 차이** — 아님. glow-audio(pid 4328), whale(pid 29392), 테스트 셸 모두
  `elevated=0`, 동일 유저(`kern\kernullist`), 동일 터미널 세션(SessionId 2).
- **"앱 재시작으로 고쳐졌다"** — 인과가 아니다. 재시작 사이에 이어버드 연결 상태와
  Whale 재생 상태가 바뀌었을 뿐이다. 재시작은 증상을 가리는 우연이다.

## 부수 발견: 릴리스 빌드에서 파일 로깅이 동작하지 않음

5627de2에서 추가된 `tauri-plugin-log` 2.8.0의 `TargetKind::LogDir` 이 릴리스 빌드에서
한 줄도 쓰지 않는다.

```
C:\Users\kernullist\AppData\Local\com.kernullist.glowaudio\logs\glow-audio.log
  91 bytes, LastWriteTime 2026-07-11 00:25
  [2026-07-10][15:25:10][glow_audio_lib][INFO] [v2-routing] engine ready [legacy (ab3d4648)]
```

남아 있는 한 줄은 debug 빌드(`target\debug`, 00:25:06 빌드)가 쓴 것이다. 릴리스 빌드는
00:28:11에 만들어졌고, 그 이후 두 번의 신규 실행(pid 4328 @ 00:26:44, pid 24120 @ 00:43:20)이
모두 아무것도 추가하지 않았다. `lib.rs:992` 의 워커 spawn은 무조건 실행되고
`lib.rs:246/258/267` 중 하나는 반드시 찍혀야 하므로, LogDir 타깃 자체가 죽어 있다.

**영향: 현장에서 이런 증상이 나와도 진단할 근거가 전혀 남지 않는다.** 이번 조사에 이만큼
품이 든 직접적인 이유다.

## 레퍼런스

- `src-tauri/src/audio_volume.rs:42` `enumerate_sessions` — `DEVICE_STATE_ACTIVE` 로만 엔드포인트 열거
- `src-tauri/src/audio_volume.rs:142` — 이름 해석 실패 시 `None => continue` (무로그 드롭)
- `src-tauri/src/lib.rs:606` `render_enumerator` — 커맨드 호출마다 enumerator 재생성
- `src-tauri/src/lib.rs:872` — `tauri_plugin_log` 설정
- `src/App.tsx:594` — App Volume 탭 1.5초 폴링 (`document.hidden` 이면 건너뜀)
- `IAudioSessionControl2::GetSessionIdentifier` 반환 문자열에 exe 전체 경로가 들어 있음:
  `{endpoint}|\Device\HarddiskVolumeN\...\whale.exe%b{guid}` — 프로세스 핸들 없이 쓸 수 있는
  이름 해석 폴백 소스

## 미해결

- [ ] ~~릴리스 빌드에서 파일 로깅이 죽는다~~ -> **부분 해소, 원인은 미확정.**
      2026-08-07 01:05, 동일 소스로 새로 만든 릴리스 빌드를 `--minimized` 로 띄웠더니
      로그 파일에 정상적으로 기록됐다:
      ```
      before: 91 bytes, 2026-07-11 00:25:10
      after:  182 bytes, 2026-08-07 01:05:49
      [2026-08-06][16:05:49][glow_audio_lib][INFO] [v2-routing] engine ready [legacy (ab3d4648)]
      ```
      즉 `TargetKind::LogDir` 자체는 릴리스에서 동작한다. 그런데 같은 해시의 바이너리로
      돌던 인스턴스(pid 4328 @ 00:26:44, pid 24120 @ 00:43:20)는 22분이 지나도록 한 줄도
      쓰지 않았다. 로그 디렉터리에 로테이션된 파일도 없다(`glow-audio.log` 단일 파일).
      해당 인스턴스가 왜 침묵했는지는 규명하지 못했고, 바이너리를 교체하면 사라지는
      문제라 더 파지 않았다. 재발하면 그 프로세스에 디버거를 붙여 워커 스레드가
      `RoutingEngine::new()` 에서 살아 있는지부터 확인할 것.
      참고: `main.rs` 의 `windows_subsystem = "windows"` 때문에 릴리스에서 Stdout 타깃은
      원래 아무 데도 안 나온다. 진단은 파일 타깃에만 의존한다.
- [ ] 엔드포인트가 `UNPLUGGED` 로 갔다가 돌아올 때 Chromium이 세션을 재생성하는 지연 시간
      (블루투스 재연결 후 몇 초 만에 다시 잡히는지) 미측정.
