---
id: realtime-first
title: "실시간 동기화 우선, 하드코딩 금지"
date: 2026-10-24
source: "오너 2026-10-24: \"이런 하드 코딩은 쓰레기야 대부분\". claim을 손으로 적던 방식과 흩어진 리터럴 값을 도구·토큰·웹훅으로 바꾸면서 정리."
status: active
summary: "같은 값·상태를 여러 곳에 복사하지 말고 하나의 원본과 실시간 이벤트로 동기화한다; claim·token은 도구로만 바꾼다"
---
# 실시간 동기화 우선, 하드코딩 금지

- 오너: "이런 하드 코딩은 쓰레기야 대부분".
- 같은 값·상태를 두 곳 이상에 적지 않는다. 하나의 원본을 정하고(예: `tokens/tokens.json`, `bots/active/`) 나머지는 그것을 읽거나 이벤트로 받는다.
- 변경은 도구 경로로 한다: claim은 `claim_register`·`claim_renew`·`claim_release`, 토큰은 `token_set`. 도구가 쓰기·커밋·이벤트를 함께 처리한다.
- 기다리거나 맞추는 일은 폴링·수동 복사·주기 동기화가 아니라 webhook·git hook·요청 시점 실행으로 한다([event-driven](event-driven.md)).
- 외부 시스템이 이벤트를 주지 않아 어쩔 수 없이 복사하거나 주기 확인을 해야 하면, 그 이유와 범위를 코드나 로그에 한 줄로 남긴다.
