# kiwi-core (Kiwi)

Rust 엔진. 바이너리 `kiwi`. 설계: alpha.md, 재설계안 Page.

| 단계 | 명령 | 하는 일 |
| --- | --- | --- |
| A 의도·승인 | `kiwi intent add/stop/revoke/list`, `kiwi approve add/check`, `kiwi pref add` | 오너 지시만 epoch를 올림. 참고 자료·다른 봇은 승인 불가. 추론 선호는 지시를 못 덮음 |
| B 작업 | `kiwi task start/guard/finish` | fencing token + epoch. guard 0 계속, 10 변경(brief 다시), 9 취소/펜스 |
| D 컴파일 | `kiwi compile --src . --out X.db` | rules/·bots/·bots/skills/·checks/registry.toml·kiwi.toml → kiwi.db(FTS5)+graph.json. 깨진 링크·중복 id·없는 mandatory·민감 경로면 실패 |
| D 문맥 | `kiwi brief …` / `kiwi rule ID` | Bundle. mandatory는 안 자름, 선택 문맥만 `--budget`(기본 4096B) 안에서 결정적 packer |
| 쓰기 | `kiwi write --repo CLONE --patch P --message M --principal P` | snapshot에서 compile+registry 검사 → gate → commit/push(lease·fencing) → 원격 SHA 확인 → kiwi.db 원자적 교체. 같은 proposal-id 재전송은 커밋 1개 |
| 관측 | `kiwi status`, `kiwi backup --to P`, `kiwi refresh --repo CLONE` | 활성 SHA·epoch·lease·outbox·brief 통계 |
| 공개 사본 | `kiwi oss tick/flush/export/scan/status --repo CLONE` | 토큰 `oss.*`만 보고 한 SHA의 git 트리를 내보냄 → 식별자 치환 → 비밀·식별자 스캔(하나라도 걸리면 발행 안 함) → 테스트 → 커밋·태그·push → `oss.released`. 설명 dev/tools/kiwi-oss |

- 상태: `$KIWI_HOME`(기본 `~/.kiwi`)의 `ops.db`(백업 필요)와 `kiwi.db`(재생성 가능). 저장소 밖.
- 박스가 단일 writer·compiler·index. 맥·윈도우는 `kiwi refresh --replica`로 읽기 전용 복제본(`freshness: replica`).
- gate: `kiwi.toml [writer] gated`(보안·결제 규칙)만 오너 승인(`--approval-ref`) 필요.
- Decisions 점수: `python3 tools/decisions/adapter.py brief -- <brief 인자>`.
- 테스트: `cargo test -q` (단계 1~4 수용 시험, 네트워크 없음).

## 박스 운영 (2026-10-23 추가)

- **자동 갱신:** `tools/box/kiwi-refresh`. 원격 main이 활성 SHA와 다를 때만 `kiwi-main`을 ff하고 `kiwi refresh`(같으면 아무것도 안 함, flock). `kiwi brief`가 5분에 한 번까지 백그라운드로 부르고(`KIWI_REFRESH_SECS`, 끄기 `KIWI_AUTOREFRESH=off`), `kiwi-sync`도 끝날 때 부른다. 박스에 cron·systemd가 없어 주기 실행 대신 이 두 경로를 쓴다.
- **의미 검색:** `tools/box/kiwi-embed`(fastembed `minishlab/potion-multilingual-128M`, `~/.kiwi/venv-embed`, 모델 `~/.kiwi/models`). `kiwi write`·`kiwi refresh`가 kiwi.db에 `vec` 표를 만들고, `kiwi brief`는 정확 검색 점수에 의미 점수를 섞는다(필수 규칙과 무관, 끄기 `KIWI_SEMANTIC=off`). 색인 대상은 컴파일된 노드뿐이라 `accounts/`·비밀은 들어가지 않는다. 복제본(맥·윈도우)은 정확 검색만.
- **공개 사본:** kiwi-refresh가 활성화 뒤 `kiwi oss tick`을 백그라운드로 부른다. 가장 오래된 변경이 `oss.improved.window_minutes`보다 오래되면 `kiwi.improved` → 내보내기·스캔·테스트·push → `oss.released`. dev/tools/kiwi-oss
- **MCP 자원:** kiwi-sprouts가 `kiwi://rule/{id}`를 `resources/list`·`resources/read`로 내주고, initialize `instructions`에 v2 시작 안내를 싣는다.
- **ops.db 백업:** `tools/box/ops-backup`가 하루 한 번(kiwi-refresh가 부름) 비공개 저장소 `207studio-founder/ops-backup`에 `ops.db.gz` 하나를 갱신하고 주마다 `ops-YYYY-Www` 태그를 단다. 캐시 표는 빼고 키·토큰·전화·주민번호 모양 문자열은 `[REDACTED]`로 바꾸며, 남아 있거나 저장소가 비공개가 아니면 push하지 않는다. 복원: `ops-backup restore OUT.db`(무결성 검사 포함).

## Decisions 문맥 압축 (2026-10-23 14:24 오너)

- `kiwi brief`: 정확·의미 점수 상위 40개 선택 후보만 `tools/decisions/kiwi-scorer`(Decisions score, gpt-6-luna, 한 번의 batch)로 0..1 관련도를 받아 순위를 정하고 4KB에 담는다. 필수 규칙은 후보에 들어가지 않는다. 결과는 ops.db `decision_cache`에 (작업 문장, 후보 id·본문 해시)로 저장해 같은 질문은 다시 부르지 않는다. 실패·미응답이면 기존 혼합 순위(`decisions: fallback_*`)이고 그 묶음은 캐시하지 않는다. 끄기 `KIWI_DECISIONS=off`.
- `kiwi compact --task T [--budget 4096 --keep-tail 2] < context.txt`: 긴 작업 문맥을 빈 줄 단위로 나눠, 첫 조각·마지막 조각·`[pin]` 조각은 항상 남기고 나머지는 Decisions 점수 순으로 예산 안에서 남긴다(원래 순서 유지). 실패하면 최근 조각 우선.
