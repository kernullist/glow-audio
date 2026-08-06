# CHANGELOG

의미 있는 변경만 기록한다. 날짜는 절대 표기(YYYY-MM-DD).

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
