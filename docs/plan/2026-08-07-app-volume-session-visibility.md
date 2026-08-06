# 작업 계획서: App Volume 목록 안정화 (사라지는 앱 문제) + 릴리스 로깅 복구

- 작성일: 2026-08-07
- 상태: 승인됨 (2026-08-07)
- 관련 문서: [[docs/research/app-volume-session-visibility]]

## 1. 목표 / 배경

실행 중인 앱(재현 사례: Naver Whale)이 App Volume 탭에서 사라진다. 조사 결과 열거 코드
버그가 아니라 **가시성 조건이 너무 좁아서** 생기는 구조적 문제다. 현재 목록에 뜨려면
앱이 (1) 지금 WASAPI 렌더 스트림을 잡고 있고 (2) 그 스트림이 걸린 엔드포인트가 지금
`DEVICE_STATE_ACTIVE` 여야 한다.

이 환경에서 두 조건이 모두 자주 깨진다.

- Chromium 계열(Whale/Chrome)은 재생이 멈추면 스트림을 반납 -> 세션이 열거자에서 제거됨
- 블루투스 이어버드 `헤드폰(3- QCY-T13)` 이 `ACTIVE` <-> `UNPLUGGED` 를 오감 ->
  그 엔드포인트의 **모든 앱 세션이 한꺼번에** 목록에서 증발

결과적으로 "볼륨을 조절하려는 순간에 그 앱이 목록에 없는" 상태가 반복된다. GlowAudio는
믹서를 넘어 "앱별 볼륨을 기억해 두는 도구"를 표방하므로, 앱이 조용할 때 미리 설정해 둘 수
없다는 건 기능 자체의 결함이다.

부수적으로, 이 조사에 품이 많이 든 직접적 이유는 **릴리스 빌드에서 파일 로깅이 아예 동작하지
않아서** 현장 진단 근거가 0이었기 때문이다. 같이 고친다.

## 2. 범위

- 포함:
  - 최근 본 앱을 일정 시간 목록에 유지하는 세션 캐시(TTL) + 프로세스 생존 확인
  - 앱이 지금 재생 중인지 여부를 UI에 표시 (playing / idle)
  - `DEVICE_STATE_ACTIVE` 외 `UNPLUGGED` 엔드포인트도 열거 대상에 포함
  - 이름 해석 폴백: `IAudioSessionControl2::GetSessionIdentifier()` 의 exe 경로 사용
  - 열거/이름해석 실패 시 로그 남기기 (현재는 무로그 드롭)
  - 릴리스 빌드 `tauri-plugin-log` LogDir 타깃 복구
- 제외(이번에 안 하는 것):
  - `IAudioSessionNotification` / `IMMNotificationClient` 콜백 기반 전환 (폴링 유지)
  - System Sounds(pid 0) 세션 노출
  - 캡처(eCapture) 세션
  - 라우팅(v2) 쪽 동작 변경
- 전제 조건 / 의존성: 없음. 기존 크레이트 버전 유지 (windows 0.58, sysinfo 0.32).

## 3. 접근안 비교

| 안 | 요약 | 장점 | 단점 | 리스크 |
|---|---|---|---|---|
| A | 현행 유지 + 문서화만 | 코드 변경 0 | 문제 그대로. "앱이 사라진다"는 계속 발생 | 사용자가 기능을 신뢰 못 함 |
| B | 열거 시 `UNPLUGGED` 엔드포인트도 포함 | 한 줄 수정, 블루투스 플랩 케이스 해결 | Chromium idle 케이스는 여전히 미해결 | 낮음 |
| C | B + 최근 본 앱 TTL 캐시 (프로세스 생존 확인 후 유지) | 두 원인 모두 덮음. UI가 안정적 | 캐시 상태 관리 필요, "지금 재생 중" 구분 표시 필요 | 죽은 앱이 잔류하지 않도록 PID 검증 필수 |
| D | `IAudioSessionNotification` 콜백 기반 전면 재작성 | 즉시성 최고, 폴링 제거 | COM 콜백 수명/스레딩 복잡, 회귀 위험 큼 | 높음. 지금 규모에 과함 |

**선택: C (B 포함)**

선택 근거:

- A는 실제 사용 시나리오(이어버드 쓰는 환경)에서 기능이 사실상 안 쓰인다. 기각.
- B만으로는 부족하다. 실험 5에서 확인했듯 Whale은 이어버드가 빠진 뒤 **다른 엔드포인트에도
  세션이 없었다.** 즉 엔드포인트 문제와 Chromium idle 문제는 별개이고 둘 다 발생한다.
- D는 올바른 최종 형태지만, 지금 증상은 폴링 주기(1.5초)가 아니라 **가시성 범위**의 문제다.
  콜백으로 바꿔도 "세션이 없는 앱"은 여전히 안 보인다. 즉 D는 이 버그를 못 고친다.
  리스크만 크므로 지금은 기각하고 별도 과제로 남긴다.
- C는 세션 유무와 무관하게 "최근 소리를 낸, 아직 살아 있는 앱"을 보여준다. Windows 기본
  믹서보다 나은 지점이자 GlowAudio의 "기억" 컨셉과 맞는다.

## 4. 구현 단계

1. `audio_volume.rs`: `enumerate_sessions` 의 상태 마스크를
   `DEVICE_STATE_ACTIVE | DEVICE_STATE_UNPLUGGED` 로 확장. 세션 반환 튜플에
   `AudioSessionState` 를 추가해 재생 여부를 상위로 전달.
2. `audio_volume.rs`: 이름 해석 폴백 추가.
   `names_for` 가 실패한 PID는 `IAudioSessionControl2::GetSessionIdentifier()` 문자열에서
   `\...\<name>.exe` 를 파싱해 사용. 둘 다 실패하면 `log::warn!` 후 드롭.
   (현재 `audio_volume.rs:142` 는 무로그 드롭 -> 이번 조사가 어려웠던 원인 중 하나)
3. `audio_volume.rs`: `SessionCache` 도입.
   - `exe -> (last_seen: Instant, last_pid: u32, volume, muted)` 유지
   - 매 열거마다 갱신, TTL(기본 5분) 이내이고 **해당 exe의 프로세스가 아직 살아 있으면**
     목록에 유지
   - `AppSession` 에 `active: bool` (지금 세션 보유) 필드 추가
4. `lib.rs`: `list_app_sessions` 가 캐시를 통과하도록 배선. 캐시는 프로세스 전역
   (`SharedState` 에 `Mutex<SessionCache>`) 으로 두어 커맨드/워커가 공유.
5. `App.tsx` / `api.ts`: `AppSession.active` 반영. 비활성 행은 흐리게 + "idle" 배지.
   슬라이더는 계속 조작 가능하되, 세션이 없으면 볼륨 규칙(`VolumeRule`)에만 기록되고
   다음 세션 생성 시 `VolumeApplier` 가 적용하도록 안내 문구 조정.
6. 릴리스 로깅 복구: 콘솔 붙여 릴리스 빌드를 직접 실행해 stdout 타깃도 죽는지 확인 ->
   플러그인 setup 실패면 원인 제거, 아니면 LogDir 경로/권한 문제 확인.
   복구 후 `list_app_sessions` 결과 요약을 debug 레벨로 남긴다.
7. `docs/CHANGELOG.md` 갱신.

## 5. 위험 및 실패 경로

- 위험: TTL 캐시가 죽은 앱을 계속 보여준다.
  - 완화: 목록에 유지하기 전 `sysinfo` 로 해당 exe 이름의 프로세스 생존을 매번 확인.
    PID가 아니라 exe 이름 기준(Chromium은 세션 PID가 자식 프로세스라 자주 바뀜).
- 위험: `UNPLUGGED` 엔드포인트에서 `Activate(IAudioSessionManager2)` 가 실패하거나 느릴 수 있다.
  - 완화: 실패는 기존대로 `continue`. 단 이번엔 `log::debug!` 를 남긴다.
    1.5초 폴링에 영향을 줄 만큼 느리면 상태 마스크 확장을 되돌린다(B만 롤백 가능하게 분리 커밋).
- 위험: 세션 없는 앱에 `SetMasterVolume` 을 걸면 아무 효과가 없어 사용자가 혼란스럽다.
  - 완화: `active=false` 행은 "다음 재생 때 적용됨"으로 명시하고 규칙에 저장.
- 실패 시 증상 / 탐지 방법: 앱이 목록에서 사라지거나(회귀), 종료한 앱이 남아 있거나,
  App Volume 탭 폴링이 눈에 띄게 느려짐. 6단계에서 살린 로그로 확인.
- 롤백: 1~2단계와 3~5단계를 별도 커밋으로 나눠 개별 revert 가능하게 한다.
- 호환성: Windows 10 1803+ 에서 `DEVICE_STATE_UNPLUGGED` 열거는 표준 동작. 드라이버 서명 무관.

## 6. 검증 방법

- 재현 절차 A (블루투스 플랩):
  1. QCY-T13 연결 상태에서 Whale로 오디오 재생 -> App Volume에 Whale 표시 확인
  2. 이어버드 전원 off (엔드포인트가 `UNPLUGGED` 로 전이)
  3. **기대: Whale이 계속 목록에 남아 있고 "idle" 표시**. (현재는 사라짐)
- 재현 절차 B (Chromium idle):
  1. Whale에서 재생 -> 표시 확인
  2. 재생 중지 후 Chromium이 스트림 반납할 때까지 대기 (수십 초~수 분)
  3. **기대: TTL 5분 이내 목록 유지, "idle" 표시. Whale 종료 시 즉시 사라짐**
- 재현 절차 C (이름 해석 폴백):
  `names_for` 를 강제로 빈 맵을 반환하도록 임시 패치한 디버그 빌드에서도
  세션 식별자 파싱으로 `whale.exe` 가 표시되는지 확인.
- 성공 기준:
  - A, B에서 Whale이 목록에서 사라지지 않는다.
  - Whale 프로세스를 완전히 종료하면 다음 폴링(1.5초) 내에 사라진다.
  - 릴리스 빌드 실행 시 `glow-audio.log` 에 startup 라인이 실제로 기록된다.
  - 15,000회 soak 재실행에서 핸들 수와 실패 카운트가 기존과 동일하게 평탄하다.

## 7. 오픈 이슈

- [x] TTL 기본값 5분이 적절한가. 사용자 설정으로 뺄 것인가.
      -> **2026-08-07 결정: 설정으로 노출.** 환경마다 블루투스 플랩 주기와 Chromium 반납
      시점이 달라 고정값으로는 맞출 수 없다. 기본 5분, Settings에서 조절.
- [x] `active=false` 인 앱의 슬라이더를 조작 가능하게 둘 것인가, 비활성화할 것인가.
      -> **2026-08-07 결정: 조작 가능.** 비활성화하면 "조용할 때 미리 설정해 둔다"는
      이 작업의 목적 자체가 사라진다. 조작 시 `VolumeRule` 에 저장되고 다음 세션 생성 때
      `VolumeApplier` 가 적용한다.
- [~] 릴리스 로깅이 죽은 원인 -> 새 릴리스 빌드는 정상 기록됨을 확인(2026-08-07 01:05).
      기존 인스턴스가 침묵한 이유는 미확정이며 바이너리 교체로 사라지는 문제라 보류.
      상세는 [[docs/research/app-volume-session-visibility]] 의 "미해결" 참조.
- [ ] `IAudioSessionNotification` 전환은 별도 과제로 남길지 ADR로 기록할지.

## 8. 진행 로그

- 2026-08-07: 원인 규명 완료. 열거 코드 버그/캐시 함정/핸들 누수/구버전 바이너리 가설을
  모두 실험으로 반증하고, 엔드포인트 `UNPLUGGED` 전이 + Chromium 스트림 반납이 실제 원인임을
  확인. 상세 실험 기록은 [[docs/research/app-volume-session-visibility]].
  릴리스 빌드 파일 로깅이 동작하지 않는다는 부수 결함도 발견. 계획서 작성, 승인 대기.
- 2026-08-07: 오픈 이슈 2건 결정(TTL 설정화 / idle 슬라이더 조작 가능), 안 C 구현 완료.
  - `audio_volume.rs` 재작성: 상태 마스크에 `DEVICE_STATE_UNPLUGGED` 추가,
    `SessionCache`(TTL + exe 이름 기준 생존 확인), 세션 식별자 기반 이름 폴백,
    실패 경로 로깅. `set_app_volume`/`set_app_mute` 는 적용된 세션 수를 반환.
  - `lib.rs`: `session_cache` / `idle_ttl_secs` 를 `SharedState` 에 추가하고
    `glow_settings.json` 에 영속화. `get_idle_ttl` / `set_idle_ttl` 커맨드 추가.
  - UI: `idle` 배지 + 흐린 표시, Settings에 "App Volume Idle Timeout",
    유휴 상태에서 조작하면 `VolumeRule` 을 자동 저장(그래야 값이 어디든 남는다).
  - 검증: 실기기 e2e 테스트 `idle_app_stays_listed_until_its_process_exits` 통과.
    재생 중 등장 -> 세션 소멸 후에도 `idle` 로 잔류 -> 프로세스 종료 시 즉시 제거.
    누수 가드 `enumeration_does_not_leak_over_a_long_session`: 5,000회 / 6.2ms per call,
    핸들 213 -> 215. (구현 전 대비 호출당 비용은 오히려 감소 — `ProcessRefreshKind::new()`
    로 최소 갱신만 하도록 바꾼 효과)
  - e2e 테스트 1차 실패에서 배운 것: 생존 확인이 exe 이름 기준이라, 테스트를 `pwsh.exe` 로
    돌리면 테스트를 띄운 셸 자신이 잡혀서 "종료 후 제거" 단계를 검증할 수 없다.
    고유 이름으로 복사한 프로브 실행 파일을 쓰도록 수정. 이름 기준 생존 확인 자체는
    의도된 설계(Chromium은 세션 PID가 자식 프로세스라 수시로 바뀐다).
  - 남은 작업: 사용자 환경의 `C:\Tools\glow-audio.exe` 교체 및 재시작(사용자 확인 필요).
