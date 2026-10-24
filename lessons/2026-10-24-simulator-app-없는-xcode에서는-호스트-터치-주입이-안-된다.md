---
date: 2026-10-24
recorded_by: codex
area: ios-simulator-capture
status: open
occurrences: ["2026-10-24, App 버튼 애니메이션 시뮬레이터 캡처  (codex 보고)"]
---

# Simulator.app 없는 Xcode에서는 호스트 터치 주입이 안 된다

## 무엇이

Simulator.app가 설치되지 않은 Xcode에서 CoreSimulator HID로 홈 버튼은 동작했지만 터치 다운/업은 앱에 전달되지 않았다. 9-arg mouse 메시지와 IOHID digitizer 경로 모두 실패했다.

## 이제

- DEBUG 전용 --button-capture 화면에서 실제 AppBounceButtonStyle 경로를 forcedPressed로 구동해 simctl recordVideo로 녹화하고, 그 사실을 캡처 노트와 PR에 명시한다.
