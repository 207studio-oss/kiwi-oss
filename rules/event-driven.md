---
id: event-driven
title: "주기 확인 금지 (오너, 2026-10-16, 반복 지시)"
date: 2026-10-16
source: "git 02d950d4 (본문 참조)"
status: active
summary: "주기 확인 금지 (오너, 2026-10-16, 반복 지시)"
---
# 주기 확인 금지 (오너, 2026-10-16, 반복 지시)

- 몇 분마다 확인하는 polling, 주기 타이머(launchd/cron 간격 동기화 등)를 쓰지 않는다.
- 대신 이벤트로 움직인다: webhook, git hook, 요청 시점 실행(on-demand).
- Kiwi claim·token 변경은 kiwi-live `subscribe`(또는 `POST /webhooks`)로 등록한 구독자에게 즉시 HMAC 서명 JSON으로 간다. 받는 쪽은 폴링하지 않는다. 이벤트 이름·서명 형식은 [tokens/tokens.json](../tokens/tokens.json)의 `webhook`.
- 예: Kiwi(`207studio-founder/kiwi`)는 커밋 시 post-commit hook으로 push, 검색 직전에 pull.
- 정해진 시각의 루틴(예: 매일 아침 점검)은 오너이 요청한 경우만 예외.
- 루틴에서 출처 읽기가 실패하면 "새 것 없음"이 아니라 실패다. 조용히 넘기지 말고 실패한 출처를 결과에 적는다.
