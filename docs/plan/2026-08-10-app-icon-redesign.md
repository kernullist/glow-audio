# 작업 계획서: 앱 아이콘 신규 제작 및 적용

- 작성일: 2026-08-10
- 상태: 완료
- 관련 문서: [docs/CHANGELOG.md](../CHANGELOG.md), [tools/make_icon.py](../../tools/make_icon.py)

## 1. 목표 / 배경

현재 `src-tauri/icons/*` 는 `tauri init` 이 넣어준 **기본 Tauri 로고**(청록/노랑 무한대 마크)
그대로다. 문제:

- 작업표시줄 / 트레이 / 설치 마법사 / exe 리소스 어디에서도 GlowAudio 로 식별되지 않는다.
  다른 Tauri 앱과 아이콘이 동일해 구분이 불가능하다.
- 앱 UI 는 네온 사이버펑크(cyan `#00f0ff` / purple `#b026ff`, 배경 `#0d0e12`) 테마인데
  아이콘만 톤이 완전히 다르다.
- `index.html` 의 favicon 도 `vite.svg`, `<title>` 도 "Tauri + React + Typescript" 로
  스캐폴딩 잔재가 남아 있다.

## 2. 범위

- 포함:
  - 신규 아이콘 디자인(앱 테마와 동일한 네온 톤).
  - `src-tauri/icons/` 전체 재생성(32/128/128@2x/icon.png, `icon.ico`, `icon.icns`, Square*Logo, StoreLogo).
  - 웹 favicon(`public/icon.svg`) 및 `index.html` 의 favicon / title 정리.
  - 재현 가능한 생성 스크립트를 리포에 커밋(`tools/make_icon.py`).
- 제외(이번에 안 하는 것):
  - 트레이 전용 모노크롬 아이콘 분리(현재 코드는 `default_window_icon()` 재사용 — `src-tauri/src/lib.rs:884`).
  - README 스크린샷 재촬영, 스토어 배너/프로모 이미지.
  - 인스톨러 배너(NSIS/WiX 사이드바 BMP).
- 전제 조건 / 의존성:
  - Python 3.12 + Pillow 12 (설치 확인됨) — 래스터 렌더링.
  - `@tauri-apps/cli` (devDependency) 의 `tauri icon` — 1024px PNG 한 장에서 전 플랫폼 아이콘 파생.

## 3. 접근안 비교

### 3-1. 제작 파이프라인

| 안 | 요약 | 장점 | 단점 | 리스크 |
|---|---|---|---|---|
| A | SVG 수작업 → 외부 래스터라이저(cairosvg/Inkscape) | 벡터 원본 유지, 편집 용이 | 로컬에 래스터라이저 없음(cairosvg 미설치, Inkscape 없음) | 환경 의존, 재현 불가 |
| B | **Pillow 로 프로그램 생성(4x 슈퍼샘플링 + GaussianBlur 글로우)** | 의존성 이미 충족, 글로우/블룸 표현 자유, 스크립트가 곧 원본 | 벡터 원본이 스크립트 형태 | 낮음 |
| C | 외부 이미지 생성 서비스 | 손 안 대도 됨 | 재현 불가, 라이선스/톤 통제 불가 | 높음 |

**선택: B**
근거: 이 환경에서 유일하게 결정적(deterministic)으로 재현되는 경로다. 네온 글로우는
블러 합성이 핵심인데 Pillow 로 직접 다루는 편이 SVG 필터보다 결과 통제가 쉽다.
스크립트를 커밋해 두면 색/형태 변경 시 재실행만으로 전 사이즈가 다시 나온다.
16px 트레이용으로 디테일을 줄인 별도 렌더 경로를 넣기도 쉽다(안 A/C는 어렵다).

### 3-2. 디자인 시안

| 안 | 컨셉 | 16px 가독성 | 비고 |
|---|---|---|---|
| A `dial` | 네온 다이얼(270° 링 + 노브) 안에 EQ 바 3개 | 상 | 링 형태는 축소에 강함 |
| B `route` | 라우팅 포크(한 줄 → 두 갈래 + 노드) | 중 | 앱의 정체성(라우터)을 가장 잘 설명 |
| C `bars` | 이퀄라이저 바 5개 | 상 | 오디오임은 즉시 전달되나 흔함 |
| D `phones` | 헤드폰 + 네온 글로우 | 상 | 직관적이나 라우팅 뉘앙스 없음 |

**선택: A (`dial`)** — 2026-08-10 사용자 확정.
근거: 링은 원형이라 16px 축소에서 형태가 가장 늦게 무너지고, "Glow" 라는 이름과
링 글로우가 직결된다. 안쪽 EQ 바가 오디오 정체성을, 노브가 제어(볼륨/전환)
뉘앙스를 동시에 담는다. B 는 정체성 설명력이 가장 좋았으나 16px 트레이에서
분기 구조가 뭉개져 탈락. C/D 는 축소 가독성은 좋으나 형태가 흔해 브랜드 마크로
약하다.

초기 렌더에서 발견해 고친 것:
- 그라디언트를 캔버스 전체 기준으로 매핑해서 글리프가 전부 중간톤(파랑)으로
  나왔다. 글리프 bbox 기준으로 바꿔 cyan→purple 전 구간이 살아나게 했다.
- `dial` 초안은 링 안에 사인파를 넣었는데 링과 겹쳐 형체가 뭉개졌다. 링 반경을
  키우고 안쪽을 EQ 바 3개로 교체.

## 4. 구현 단계

1. `tools/make_icon.py` 작성 — 시안 A~D 를 512px + 32/16px 축소본과 함께 시트로 렌더.
2. 시안 확정.
3. 확정안을 1024px 마스터 PNG 로 렌더 (`src-tauri/icons/icon-source.png`).
4. `npx tauri icon src-tauri/icons/icon-source.png` 로 전 사이즈/포맷 재생성.
5. `public/icon.svg`(또는 PNG) + `index.html` 의 favicon/title 갱신.
6. `docs/CHANGELOG.md` 반영.

## 5. 위험 및 실패 경로

- 위험: 16~24px 축소 시 글로우가 뭉개져 형체가 사라짐.
  - 탐지: 렌더 시트에 16/24/32px 축소본을 항상 같이 출력해 눈으로 확인.
  - 완화: 소형 사이즈에서는 글로우 반경/스트로크 두께를 별도 파라미터로 조정.
- 위험: `tauri icon` 이 기존 파일을 덮어씀 → 되돌리기 필요.
  - 완화: git 으로 추적 중이므로 `git checkout -- src-tauri/icons` 로 롤백 가능.
- **위험(실제로 밟음): `icon.ico` 를 바꿔도 exe 에 반영되지 않는다.**
  `tauri-build` 은 아이콘에 대해 `cargo:rerun-if-changed` 를 내보내지 않는다. 따라서
  빌드 스크립트가 재실행되지 않고, 이전에 만들어 둔 `target/release/build/glow-audio-*/out/resource.lib`
  가 그대로 링크된다. 프론트엔드 변경 등으로 crate 자체는 "Compiling" 되기 때문에
  빌드 로그만 봐서는 성공한 것처럼 보인다.
  - 탐지: 빌드 후 `PrivateExtractIcons` 로 exe 에서 아이콘을 뽑아 눈으로 확인.
    또는 `resource.lib` 크기가 `icon.ico` 크기와 함께 움직이는지 확인
    (구 86,642B → 87,804B / 신 163,689B → 165,000B).
  - 완화: 아이콘 교체 후에는 `touch src-tauri/build.rs` 또는 `cargo clean -p glow-audio`.
    README(양쪽 언어)에 경고로 명시함.
- 위험: `.icns`(macOS)는 이 환경에서 육안 검증 불가.
  - 완화: 생성만 하고 검증은 macOS 빌드 시점으로 미룸. Windows 타깃에는 영향 없음.
- 호환성: `icon.ico` 는 Windows 리소스로 임베드되므로 16/24/32/48/64/128/256 다중 사이즈를
  반드시 포함해야 한다(단일 256 짜리면 트레이/작업표시줄에서 뭉개짐).

## 6. 검증 방법

- `python tools/make_icon.py --sheet` 결과 시트에서 16/24/32px 가독성 확인.
- 생성된 `icon.ico` 의 포함 사이즈 목록 확인(Pillow 로 열어 `ico.info['sizes']`).
- `npm run app:exe` 빌드 후 실제 exe 의 작업표시줄 / 트레이 아이콘 육안 확인.
- 성공 기준: 트레이(16px)에서 형체 식별 가능 + 앱 UI 톤과 이질감 없음.

## 7. 오픈 이슈

- [x] 시안 확정 → A (`dial`)
- [ ] 트레이 전용 아이콘 분리 필요 여부. 현재는 `default_window_icon()` 재사용
      (`src-tauri/src/lib.rs:884`). 어두운 플레이트라 라이트 테마 트레이에서도
      대비는 확보되지만, Windows 11 트레이는 16px 로 강제 축소하므로 실사용 후 판단.
- [x] 인스톨러 아트 → v0.6.1 에서 `installerIcon`, v0.6.2 에서 NSIS header/sidebar +
      WiX banner/dialog 까지 적용 완료 (`--installer`). MSI 파일 자체의 아이콘은
      Windows Installer 가 강제하므로 변경 불가.

## 8. 진행 로그

- 2026-08-10: 현황 파악. 기본 Tauri 아이콘 그대로임을 확인. 파이프라인 안 B 선택, 시안 렌더 착수.
- 2026-08-10: 시안 4종 렌더 → `dial` 확정. 마스터 1024px 생성 후 `npx tauri icon` 으로
  전 플랫폼 세트 재생성. 이 프로젝트는 데스크톱 전용이라 CLI 가 만든
  `src-tauri/icons/{android,ios}/` 는 삭제했다.
- 2026-08-10: `tauri icon` 이 만든 `icon.ico` 는 16/32/128/256 4종뿐이라 48px 계열이
  32px 확대로 처리돼 뭉갰다. ICO 컨테이너를 직접 작성해 9종을 담고, 16~32px 는
  글로우를 줄인 compact 렌더를 쓰도록 했다.
  - Pillow 의 ICO 인코더는 32bpp 항목에 1bpp AND 마스크를 붙이지 않는다. Tauri 가
    원래 넣던 아이콘에는 있었고 일부 셸 경로가 이를 기대하므로, 마스크를 포함한
    DIB 를 직접 작성했다(16x16 항목이 1128바이트로 기존과 정확히 일치 = 레이아웃 검증).
  - 256px 만 PNG 압축, 나머지는 DIB. `tauri icon` 출력 구조와 동일.
- 2026-08-10: `index.html` favicon/title 정리, 미사용 `public/{vite,tauri}.svg` 삭제.
- 2026-08-10: **첫 exe 빌드에 새 아이콘이 안 들어갔다.** 원인은 위 §5 의
  `rerun-if-changed` 누락. `touch src-tauri/build.rs` 로 해결. 최종 확인은
  `PrivateExtractIcons` 로 exe 에서 16/20/24/32/40/48/64/128px 를 직접 뽑아
  육안 검증했다 (모두 신규 아이콘, 16px 형체 식별 가능).
- 2026-08-10: 삽질 기록 — 중간에 rustc 1.96.0 이 `glow_audio_lib` 를
  `staticlib + cdylib + rlib` 동시 산출할 때 `STATUS_HEAP_CORRUPTION` /
  `STATUS_ACCESS_VIOLATION` 으로 4회 연속 죽었다. 아이콘을 원본으로 되돌려도
  동일하게 재현돼 **아이콘과 무관한 툴체인 플레이크**로 판정했고, 이후 재시도에서
  그냥 통과했다. WER 이벤트(id 1000)는 남지 않는다. 재발하면
  `cargo rustc --lib --crate-type <one>` 으로 크레이트 타입을 하나씩 분리해
  재현 여부를 먼저 확인할 것 (개별 산출은 전부 정상이었다).
  이 프로젝트는 데스크톱 전용이라 `[lib] crate-type` 에서 `staticlib`/`cdylib` 를
  제거하는 선택지도 있으나, 이번 작업 범위 밖이라 손대지 않았다.
- 2026-08-10: v0.6.1 릴리즈 후 자산을 검사하다 **NSIS 설치 파일이 기본 NSIS 아이콘**을
  달고 있는 것을 발견 → `installerIcon` 지정. 이어서 마법사 화면 자체도 기본 이미지라
  §2 에서 제외했던 인스톨러 아트를 범위에 넣어 v0.6.2 로 처리했다(사용자 승인).
  - 크기/포맷은 NSIS/WiX 가 고정한다: 150x57, 164x314, 493x58, 493x312, **24bpp BMP**.
    알파 채널이나 PNG 는 거부된다.
  - WiX 다이얼로그(493x312)는 전면 아트로 만들면 안 된다. WixUI 가 제목/본문 텍스트를
    검은색으로 그 위에 그리므로, 좌측 164px 만 다크 패널로 두고 나머지는 흰색으로 남겼다.
  - 검증 방법: NSIS 는 `target/release/nsis/x64/installer.nsi` 의 `HEADERIMAGE` /
    `SIDEBARIMAGE` 정의 확인. MSI 는 `WindowsInstaller.Installer` COM 으로 Binary
    테이블을 읽어 `WixUI_Bmp_Banner` / `WixUI_Bmp_Dialog` 의 바이트 수가 생성물과
    일치하는지 확인(각각 85,894 / 461,814). 마법사를 실제로 실행해 눈으로 본 것은 아니다.
