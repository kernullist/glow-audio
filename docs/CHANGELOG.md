# CHANGELOG

의미 있는 변경만 기록한다. 날짜는 절대 표기(YYYY-MM-DD).

## v0.6.2 - 2026-08-10

### Changed

- **인스톨러 마법사에 브랜드 아트 적용** (2026-08-10). v0.6.1 에서 설치 파일 아이콘까지는
  바꿨지만 마법사 화면 자체는 NSIS/WiX 기본 이미지였다.
  - NSIS: `headerImage`(150x57), `sidebarImage`(164x314)
  - WiX(MSI): `bannerPath`(493x58), `dialogImagePath`(493x312)
  - 네 장 모두 `python tools/make_icon.py --installer` 로 생성한다.
    산출물은 `src-tauri/installer/*.bmp`. **알파 없는 24bpp BMP 고정** —
    NSIS/WiX 가 PNG 나 32bpp 알파 BMP 를 받지 않는다.
  - 큰 두 장(사이드바 / WiX 다이얼로그)은 앱과 같은 다크 네온 패널이고, 작은 두 장은
    설치 마법사의 흰 크롬 위에 얹히므로 라이트 배경으로 만들었다. WiX 다이얼로그는
    WixUI 가 본문 텍스트를 그리는 영역을 피해 **좌측 164px 만** 아트 패널이다.
  - 검증: `installer.nsi` 의 `HEADERIMAGE` / `SIDEBARIMAGE` 정의와, MSI Binary 테이블의
    `WixUI_Bmp_Banner`(85,894 B) / `WixUI_Bmp_Dialog`(461,814 B) 가 생성물과 바이트 일치.

## v0.6.1 - 2026-08-10

### Changed

- **앱 아이콘 신규 제작** (2026-08-10). 기본 Tauri 로고를 걷어내고 앱 UI 와 같은
  네온 톤(cyan `#00f0ff` → purple `#b026ff`)의 볼륨 다이얼 링 + 이퀄라이저 바
  마크로 교체했다. 작업표시줄 / 트레이 / exe 리소스 / 인스톨러에 모두 적용된다.
  - 생성 스크립트를 리포에 커밋: [tools/make_icon.py](../tools/make_icon.py).
    시안 비교(`--sheet`), 1024px 마스터(`--master`), 소형 사이즈 보정(`--assets`)을 담당한다.
  - `icon.ico` 를 직접 작성해 16/20/24/32/40/48/64/128/256 **9종**을 담았다
    (`tauri icon` 기본 출력은 16/32/128/256 4종뿐이라 48px 계열이 확대되어 뭉갰다).
    16~32px 항목은 글로우를 줄이고 스트로크를 굵힌 별도 렌더를 쓴다.
  - NSIS 설치 파일(`GlowAudio_*_x64-setup.exe`) 자체도 기본 NSIS 아이콘을 달고
    있어서 `bundle.windows.nsis.installerIcon` 을 지정했다. MSI 는 Windows Installer
    아이콘이 강제되어 변경 불가.
  - 웹 favicon 을 `public/icon.png` 로 교체하고 `index.html` 의 스캐폴딩 잔재
    (`vite.svg` favicon, "Tauri + React + Typescript" 타이틀)를 정리했다.
    사용되지 않던 `public/vite.svg`, `public/tauri.svg` 삭제.
  - 배경/근거: [docs/plan/2026-08-10-app-icon-redesign.md](plan/2026-08-10-app-icon-redesign.md)

## v0.6.0 - 2026-08-07

### Fixed

- **App Volume에서 실행 중인 앱이 사라지던 문제** (2026-08-07).
  세션 열거가 `DEVICE_STATE_ACTIVE` 엔드포인트만 대상으로 하고 있어서, 블루투스
  엔드포인트가 `UNPLUGGED` 로 전이하면 거기 붙어 있던 **모든 앱 세션이 한꺼번에**
  목록에서 사라졌다. 여기에 Chromium 계열이 재생 중지 후 렌더 스트림을 반납하면서
  세션 자체가 제거되는 동작이 겹쳤다.
  - 열거 대상에 `DEVICE_STATE_UNPLUGGED` 추가
  - 최근 본 앱을 유휴 TTL 동안 목록에 유지하는 `SessionCache` 도입.
    프로세스가 살아 있는 동안만 유지되고, 종료되면 즉시 사라진다.
  - 원인 분석: [docs/research/app-volume-session-visibility.md](research/app-volume-session-visibility.md)
- 세션의 exe 이름을 sysinfo로 해석하지 못하면 **아무 로그 없이 목록에서 드롭**되던 문제.
  `IAudioSessionControl2::GetSessionIdentifier()` 에 들어 있는 exe 경로를 폴백으로
  사용하고(프로세스 핸들 불필요), 둘 다 실패하면 PID당 한 번 경고를 남긴다.
- 볼륨 열거 경로 전반에 로그 추가. 이전에는 실패가 전부 조용히 `continue` 되어
  현장에서 진단할 근거가 남지 않았다.

### Added

- Settings에 **App Volume Idle Timeout** 항목. 유휴 앱이 목록에 남는 시간(초)을
  조절한다. 기본 300초, 범위 0~3600. 0이면 지금 재생 중인 앱만 표시(기존 동작).
- App Volume 행에 `idle` 배지. 실행 중이지만 오디오 세션이 없는 상태를 구분한다.
  유휴 상태에서도 슬라이더/뮤트는 그대로 조작 가능하며, 변경값은 `VolumeRule` 에
  저장됐다가 다음 세션 생성 시 `VolumeApplier` 가 적용한다.
- `get_idle_ttl` / `set_idle_ttl` 커맨드.

### Changed

- `set_app_volume` / `set_app_mute` 가 적용된 라이브 세션 수(`u32`)를 반환한다.
  0이면 유휴 상태라는 뜻이고, UI는 이때 규칙 저장으로 대체한다.
- App Volume 목록 정렬: 재생 중인 앱 우선, 그다음 이름순. 세션이 붙었다 떨어질 때
  행 순서가 흔들리지 않는다.

## v0.5.0 - 2026-07-11

- GitHub Actions CI(tsc + 프론트 빌드 + cargo check) 및 수동 트리거 릴리스 빌드 추가
- `tauri-plugin-log` 도입: stdout + OS 앱 로그 디렉터리로 런타임 로그 기록
  (주의: 릴리스 빌드에서 파일 타깃이 동작하지 않는 문제 확인됨 — 조사 진행 중)
- Routing 탭에 현재 재생 중인 앱을 칩으로 노출(클릭 시 exe 필드 자동 입력)
- 트레이 메뉴에 "Cycle Audio Device" 추가
- 자동 시작 인스턴스는 `--minimized` 로 트레이에서 조용히 기동
- 기억된 볼륨 자동 적용이 라우팅 엔진 가용성과 분리됨. 라우팅 가용성은 tri-state로
  바뀌어 기동 중 "unavailable" 배너가 깜빡이지 않는다

## v0.4.x - 2026-07-03 ~ 07-11

- 자동 시작 토글, 슬라이더 스로틀링, 설정 파일 원자적 쓰기
- 자동 전환 시 HUD 표시, 종료 시 라우팅 되돌리기, UI 값 stale 문제 수정

## v0.3.0 - 2026-06-16

- 앱별 볼륨 조절 및 기억된 볼륨 자동 복원 기능 추가
