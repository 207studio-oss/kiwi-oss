# kiwi-agent finish

일 끝에 공통 작업 절차 5단계의 세 가지를 Git 객체만 읽어 판정하고 receipt를 남긴다. 아무것도 고치지 않고, fetch·push도 하지 않는다. 명세는 agent-procedure-impl (a).

같은 이름의 바이너리가 둘이다. `check`·`run`은 `dev/tools/kiwi-graph`의 `kiwi-agent`(src/bin/kiwi-agent.rs), `finish`는 이 crate다. 하나로 합치는 일은 Kiwi 재설계에서 정한다(2026-10-23). 설치할 때 둘이 같은 경로에 덮어쓰지 않게 이 crate는 `cargo run --manifest-path`로 부른다.

```sh
cargo run --offline --manifest-path dev/tools/kiwi-agent/Cargo.toml -- finish \
  --kiwi-repo <Kiwi 체크아웃> --project-repo <worktree> \
  --agent-id <내 agent ID> --branch <브랜치> --receipt <Scratch>/<날짜>-<작업>/finish.json \
  [--path <요청 경로>]* [--log logs/<기록>.md] [--keep-claim]
```

| exit | state | 뜻 |
| --- | --- | --- |
| 0 | `finished` | 요청 경로가 커밋됐고 HEAD가 upstream과 같고 내 claim이 없다 |
| 3 | `release_pending` | 이 저장소·브랜치의 내 claim이 남았다. 넘김이면 `--keep-claim` |
| 4 | `incomplete` | `uncommitted_changes`, `no_upstream`, `head_not_on_upstream`, `log_missing_branch` |
| 2 | `unconfirmed` | 인수·receipt 오류, `kiwi_not_synced`, `source_not_committed`, `branch_mismatch`, `invalid_claim`, `remote_unresolved` |

- 여러 문제가 있으면 `findings`에 모두 적고, 종료 코드는 2 > 4 > 3 > 0 순서로 가장 나쁜 것을 따른다.
- upstream은 로컬 remote-tracking ref다(`upstream_source`). 로컬 브랜치 upstream은 없는 것으로 본다. 먼저 Kiwi를 pull하고 프로젝트 브랜치를 push한다.
- 저장소 주소는 브랜치 upstream의 원격에서, upstream이 없으면 하나뿐인 원격에서 읽는다. 원격 이름이 `origin`일 필요는 없다.
- receipt는 새 파일로만 0600으로 쓴다. 로그 본문과 claim의 `task`·`next`는 옮기지 않는다.
- 다른 저장소·브랜치의 내 만료 claim은 `own_expired`로만 알린다. 지울지는 그 일을 맡은 쪽이 정한다.
