# 토큰 (canonical tokens)

`tokens.json`이 색·간격·모양·모션·claim/task 상태의 **유일한 원본**이다. 규칙과 앱 코드는 리터럴 값을 복사하지 않고 이 파일의 경로를 가리킨다.

- 색: `color.*`
- 간격: `spacing.*` (단위 `spacing.unit`)
- 모서리: `radius.*`
- 모션: `motion.duration_ms.*`, `motion.easing.*`
- 버튼 스프링: `motion.button_spring.*` (근거: [rules/app-ui-buttons.md](../rules/app-ui-buttons.md))
- claim 상태: `claim.states`, `claim.default_hours`, `claim.events`
- task 상태: `task.states`
- 웹훅 기본값·이벤트 이름: `webhook.*` (self-improve는 `rule.proposed`, 공개 사본은 `kiwi.improved`·`oss.released` 추가)
- self-improve: `self_improve.rule_proposals.*`, `self_improve.brief_feedback.*`, `self_improve.calibration.*`
- 공개 사본(`kiwi oss`): `oss.*` (포함·제외 경로, 치환, 스캔, 묶음 창 `oss.improved.window_minutes`, 마케팅 인계). 공개 사본에는 빠진다.

## 바꾸는 방법

값은 손으로 고치지 않고 kiwi-live `token_set` 도구로 바꾼다.

```json
{"path": "motion.button_spring.web.duration_ms", "value": 460, "reporter": "<agent id>"}
```

`token_set`은 `tokens/tokens.json`을 커밋하고 `token.changed` 이벤트를 구독자에게 보낸다. 기존 경로만 바꿀 수 있고 타입은 유지된다.

## 받는 방법

`subscribe` 도구 또는 `POST /webhooks`로 URL을 등록한다. 등록 응답의 `secret`은 그 구독자만의 HMAC 키이며 다시 조회되지 않는다.

```
POST <url>  Content-Type: application/json
X-Kiwi-Event: token.changed
X-Kiwi-Timestamp: <unix seconds>
X-Kiwi-Signature: v1=HMAC-SHA256(secret, timestamp + "." + raw_body)
```

본문은 `{"schema":1,"id":"evt-...","event":"token.changed","created":"...","source":"kiwi-live","data":{...}}` JSON이다. `data`에는 `file`, `path`, `old`, `new`, `commit`이 들어간다.

## 로컬 생산자

같은 bearer로 `POST /events`에 `{"event": "...", "data": {...}}`를 보내면 kiwi-live가 위와 같은 서명 웹훅으로 구독자에게 넘긴다. `webhook.external_events`에 있는 이름만 받고(아니면 403), `data`에 비밀 모양이 있으면 400이다. `kiwi oss`가 이 길로 `kiwi.improved`·`oss.released`를 낸다.
