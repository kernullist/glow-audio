# ✨ GlowAudio Desktop (Tauri Edition) 🎧

[English](README.md) · **한국어**

[![CI](https://github.com/kernullist/glow-audio/actions/workflows/ci.yml/badge.svg)](https://github.com/kernullist/glow-audio/actions/workflows/ci.yml)

> **Windows 실제 재생 장치를 직접 제어하고, 게임 감지 자동 전환과 전역 단축키 HUD를 제공하는 네온 오디오 유틸리티 — Tauri v2 + React + Rust 재구현판**

기존 Python(CustomTkinter) 프로토타입을 Tauri 기반 네이티브 데스크톱 앱으로 재구현했습니다.
백엔드 오디오 제어는 Python/pycaw 대신 **Rust + `windows` 크레이트로 WASAPI/Core Audio COM을 직접 호출**하며,
UI는 React + TypeScript로 동일한 사이버펑크 네온/글래스모피즘 디자인을 재현합니다.

## 🖼️ 스크린샷

![GlowAudio Desktop 메인 화면](docs/screenshot.png)

장치 전환 시 나타나는 플로팅 HUD 오버레이:

![GlowAudio HUD 오버레이](docs/hud.png)

---

## 🌟 주요 기능

1. **실제 Windows 오디오 제어** — `IMMDeviceEnumerator`로 재생 장치 열거, `IAudioEndpointVolume`로 볼륨/뮤트,
   비공개 COM 인터페이스 `IPolicyConfig::SetDefaultEndpoint`로 기본 장치를 Console·Communications 역할 모두 전환.
2. **실시간 Peak 미터** — `IAudioMeterInformation`을 100ms 주기로 폴링해 장치 카드의 네온 그래디언트 바로 시각화.
3. **전역 백그라운드 단축키** — `tauri-plugin-global-shortcut`으로 OS 레벨 단축키(기본 `Ctrl+Shift+A`)를 등록,
   포커스를 빼앗지 않고 활성 장치를 순환 전환 + HUD 표시.
4. **게임 자동 라우팅 엔진** — `sysinfo` 기반 백그라운드 스레드가 2초마다 프로세스를 스캔,
   등록된 게임 실행 시 지정 장치로 자동 전환.
5. **플로팅 HUD 오버레이** — 무테·투명·항상 위 별도 윈도우를 우하단에 페이드 인/아웃.
6. **앱별 오디오 라우팅 (v2)** — 비공개 COM `IAudioPolicyConfigFactory`로 개별 앱(Chrome·Discord·게임)을 *동시에* 서로 다른 출력 장치로 라우팅. 오디오 세션 PID 기준으로 적용하며, 미지원 빌드에선 기본 장치 전환으로 자동 강등.
7. **앱별 볼륨 조절** — `ISimpleAudioVolume`로 앱마다 볼륨/뮤트를 Windows 볼륨 믹서처럼 조절. 앱을 "기억"해두면 다음 실행 시 저장된 볼륨을 자동 복원.
8. **Windows 시작 시 자동 실행** — Global Settings의 토글로 로그인 시 트레이에 조용히 자동 기동. 트레이 메뉴에서 원클릭 장치 순환도 지원합니다.
9. **파일 로그** — 런타임 로그를 OS 앱 로그 디렉터리에 기록(`tauri-plugin-log`)해 릴리스 빌드에서도 문제를 진단할 수 있습니다.

---

## 📂 구조

```
glow-audio/
├─ src/                     # React + TypeScript 프론트엔드
│  ├─ App.tsx               # 메인 UI (Devices / Profiles / Routing / Volume / Settings 5탭)
│  ├─ Hud.tsx               # 플로팅 HUD 오버레이 컴포넌트
│  ├─ api.ts                # Rust command 타입 래퍼
│  ├─ main.tsx              # ?view=hud 쿼리로 메인/HUD 분기
│  └─ styles.css            # 네온/글래스모피즘 테마
└─ src-tauri/               # Rust 백엔드
   └─ src/
      ├─ audio.rs           # WASAPI/COM 오디오 엔진 + IPolicyConfig 정의
      ├─ audio_router.rs    # v2 앱별 라우팅 (IAudioPolicyConfigFactory)
      ├─ audio_volume.rs    # 앱별 볼륨 제어 (ISimpleAudioVolume)
      ├─ monitor.rs         # 프로세스 감시 + 자동 라우팅 스레드
      └─ lib.rs             # command, 전역 단축키, HUD, 라우팅 워커, 영속화
```

설정/프로필은 OS 앱 설정 디렉터리(`%APPDATA%\com.kernullist.glowaudio\`)의
`glow_settings.json` / `glow_profiles.json`에 저장됩니다.

---

## 🚀 개발 / 빌드

### 사전 요구사항
- Node.js + npm
- Rust (stable, MSVC 타깃) + Visual Studio C++ Build Tools
- WebView2 런타임 (Windows 11 기본 포함)

### 명령
```powershell
npm install            # 프론트엔드 의존성
npm run tauri dev      # 개발 모드 (HMR + Rust 핫리빌드)
npm run tauri build    # 릴리스 빌드 (단일 exe + 설치 관리자)
```

> ⚠️ 개발 중에는 반드시 `npm run tauri dev`로 실행하세요. 디버그 exe(`src-tauri/target/debug/glow-audio.exe`)를
> 단독 실행하면 Vite 개발 서버(localhost:1420)에 붙지 못해 연결 거부 화면이 뜹니다.
> 더블클릭으로 동작하는 단독 실행 파일은 `tauri build`(아래 스크립트)로 만들어야 합니다.

### 빌드 스크립트 (`build.ps1`)
cargo PATH 처리·prerequisite 점검·산출물 경로 출력까지 자동화한 PowerShell 스크립트입니다.
```powershell
.\build.ps1              # 단일 exe + 설치 관리자(msi/nsis) 전체 빌드
.\build.ps1 -NoBundle    # 단일 exe만 (설치 관리자 생략, 더 빠름)
.\build.ps1 -SkipInstall # npm install 건너뛰기
```
산출물:
- 단독 exe: `src-tauri/target/release/glow-audio.exe`
- 설치 관리자: `src-tauri/target/release/bundle/{msi,nsis}/`

npm 별칭도 추가되어 있습니다: `npm run app:build`(전체), `npm run app:exe`(exe만), `npm run app:dev`(개발).

### 단축키
- 기본 전역 단축키: **`Ctrl+Shift+A`** — 활성 장치 순환 전환 + HUD 팝업.
- `Global Settings` 탭에서 `Ctrl+Alt+S` 같은 형식으로 변경 가능 (modifier + key, `+`로 연결).
