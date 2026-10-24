---
id: app-ui-buttons
title: "전 앱 버튼: 귀엽고 게임 같은 애니메이션"
date: 2026-10-24
source: "오너 2026-10-24 10:19 KST(베타 경유): 모든 앱에 이 수준 버튼 반영. https://x.com/yui540/status/2113452247472550340"
status: active
summary: "모든 앱 버튼은 yui540 영상 수준의 귀엽고 게임 같은 눌림·튀어오름 애니메이션. 웹은 CSS, iOS는 SwiftUI"
---

# 전 앱 버튼 애니메이션

- 모든 앱 버튼은 귀엽고 게임 같은 눌림·튀어오름 애니메이션을 쓴다. 기준: [yui540 영상](https://x.com/yui540/status/2113452247472550340). 웹은 CSS, iOS는 SwiftUI로 같은 느낌.

## 쫀득한 탄성 기준 (오너 10/24 10:48, 베타 경유)
- 누르면 눌렸다가(squash) 튀어 오르는(overshoot) 스프링.
- hover 때 살짝 흔들림.
- 누를 때 그림자가 줄어 눌리는 깊이감.
- `prefers-reduced-motion`(iOS: Reduce Motion)에서는 움직임을 끄고 색·그림자 변화만 남긴다. 필수.

## 토큰
- 색·간격·모션 값은 리터럴로 복사하지 않고 [tokens/tokens.json](../tokens/tokens.json)을 쓴다.
- 버튼 스프링은 `motion.button_spring` 하나가 원본이다: 웹 `motion.button_spring.web`, iOS `motion.button_spring.ios`, 접근성 `motion.button_spring.reduced_motion`.
- 웹 기준값은 sample-web에 출시된 모션(PR #40): 눌림 `press_*`·`press_scale_x/y`, 복귀 `release_*`, 등장 `pop_*`, hover `hover_lift_px`·`hover_scale`·`wobble_deg_1..4`, 깊이 `depth_*_px`, 깊이 색 `color.button_depth.*`. 옛 `press_scale`·`overshoot_scale`·`hover_wobble_deg`·`shadow_press_ratio`는 `motion.button_spring.web.deprecated`에 대체 키가 적힌 폐기 값이라 새 코드에서 쓰지 않는다.
- 값을 바꿀 때는 kiwi-live `token_set`을 쓰고 `token.changed` 이벤트로 각 앱에 알린다. 규칙이나 앱 코드에 숫자를 다시 하드코딩하지 않는다.
