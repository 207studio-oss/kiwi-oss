# kiwi-sprouts

Kiwi의 **코드·운영·문서 그래프를 하나의 stdio MCP/CLI**로 묶는 도구다. Python 표준 라이브러리만 쓰며, 대시보드·포트·소켓·브라우저가 없다.

## 도구

| 도구 | 하는 일 |
| --- | --- |
| `query` | 코드 심볼 + 문서 검색 + (선택) 운영 그래프를 한 번에 조회 |
| `code_overview` | 파일의 심볼 목록 |
| `code_find` | 이름으로 심볼 찾기 |
| `code_definition` | 1-based 줄/칸 위치의 정의로 이동 |
| `code_references` | 1-based 줄/칸 위치의 참조 찾기 |
| `code_replace_body` | 심볼 하나의 소스 범위를 새 본문으로 교체(파일 쓰기) |
| `ops_graph` | 기존 `kiwi-graph`의 읽기 전용 질의(`rules`, `trace`, `owners`, `preflight`, `validate`) |
| `context_for` | Kiwi 작업 시작 묶음(`kiwi brief`, 넘칠 때 Decisions 점수). Decisions 키는 각 기기 설정(`~/.local/bin/kiwi-scorer` + `OPENAI_API_KEY`, apply 스크립트가 연결)이 맡는다. 대화에서 키를 요청·요구하지 않는다. 점수가 빠지면 stderr 경고, `KIWI_DECISIONS_REQUIRED=1`이면 실패(exit 3). |
| `kiwi_rule` / `kiwi_status` / `task_guard` | 생략된 규칙 열기, 상태, 최신 지시 epoch 확인 |
| `docs_search` | 기존 brain-search(EmbeddingGemma) 문서 검색, 박스에서는 `rg` 폴백 |

## 실시간 claim·웹훅·토큰 (Python 전용)

`kiwi_live.py`(127.0.0.1:8765, 공개 `https://kiwi-host.example.net/mcp`)는 아래 도구를 추가로 제공한다. Rust `kiwi-sprouts` 실행 파일은 아직 이 도구들을 제공하지 않는다.

| 도구 | 하는 일 |
| --- | --- |
| `claim_register` | `bots/active/<owner>--<repo>--<branch>.toml` 작성·커밋, `claim.registered` 이벤트 |
| `claim_renew` | 내 claim의 `expires` 연장·커밋, `claim.renewed` 이벤트 |
| `claim_release` | 내 claim 삭제·커밋, `claim.released` 이벤트 |
| `claim_list` | 보드의 claim 목록(저장소·agent ID 필터) |
| `subscribe` / `unsubscribe` / `subscriptions` | 웹훅 구독 관리. `subscribe` 응답의 `secret`은 1회만 나온다 |
| `tokens_get` / `token_set` | `tokens/tokens.json` 읽기·한 토큰 변경. 변경은 `token.changed` 이벤트 |
| `rule_proposals` | `inbox/rule-proposals/`의 append-only 규칙 제안 목록. `record_lesson`이 유사 교훈 그룹이 임계값에 닿으면 생성·갱신하고 `rule.proposed`를 낸다. `rules/`는 읽기만 한다 |
| `brief_feedback` / `brief_quality` | brief 유용·노이즈 피드백을 `logs/brief-feedback/*.jsonl`에 추가하고 항목별 비율을 보고한다. `context_for`는 누적 점수만큼 `selected_context` 순서를 제한적으로 바꾸고 반복 `missing`을 표시한다 |
| `decide_calibration` | ask/decide 예측의 confidence 구간별 정확도. 표본이 충분하면 `self_improve.calibration` 범위 안에서 `self_improve.calibration.threshold`를 `token_set`으로 옮긴다(`token.changed`) |

구독은 도구 대신 HTTP로도 한다(모두 bearer 인증): `POST /webhooks` `{"url":"...","events":["*"]}`, `GET /webhooks`, `DELETE /webhooks/<id>`. 이벤트는 즉시 POST되며 본문은 `{"schema":1,"id":"evt-...","event":"claim.registered","created":"...","source":"kiwi-live","data":{...}}`다. 헤더 `X-Kiwi-Timestamp`와 `X-Kiwi-Signature: v1=<hex>`가 있고, 서명 대상은 `timestamp + "." + raw_body`, 키는 구독별 `secret`이다. 검증 헬퍼는 `kiwi_events.EventBus.verify`.

## 언어

Python(`pyright`), TypeScript/JavaScript(`typescript-language-server`), Rust(`rust-analyzer`), Swift(`sourcekit-lsp`)를 테스트한다. 서버가 없으면 해당 테스트는 skip된다. Rust는 `rust-src`가 없어도 매크로 안 이름을 찾도록 심볼/텍스트 폴백을 둔다.

## MCP로 등록

stdio 명령은 **`python3 /path/kiwi_sprouts.py mcp`** 이고, 루트는 환경변수 `KIWI_SPROUTS_ROOT`로 넘긴다(생략하면 현재 디렉터리).

**Codex** `~/.codex/config.toml`:

```toml
[mcp_servers.kiwi-sprouts]
command = "python3"
args = ["/path/kiwi_sprouts.py", "mcp"]
env = { KIWI_SPROUTS_ROOT = "/path/to/kiwi" }
```

**Claude Code** CLI:

```sh
claude mcp add kiwi-sprouts \
  --env KIWI_SPROUTS_ROOT=/path/to/kiwi \
  -- python3 /path/kiwi_sprouts.py mcp
```

또는 프로젝트 `.mcp.json`:

```json
{
  "mcpServers": {
    "kiwi-sprouts": {
      "type": "stdio",
      "command": "python3",
      "args": ["/path/kiwi_sprouts.py", "mcp"],
      "env": { "KIWI_SPROUTS_ROOT": "/path/to/kiwi" }
    }
  }
}
```

**Cursor** `~/.cursor/mcp.json` 또는 프로젝트 `.cursor/mcp.json`:

```json
{
  "mcpServers": {
    "kiwi-sprouts": {
      "command": "python3",
      "args": ["/path/kiwi_sprouts.py", "mcp"],
      "env": { "KIWI_SPROUTS_ROOT": "/path/to/kiwi" }
    }
  }
}
```

등록 후 `python3 /path/kiwi_sprouts.py --health`로 unittest를 돌려 확인한다.

## 문서 검색과 인덱스

`docs_search`는 Mac mini의 기존 brain-search/EmbeddingGemma 인덱스를 재사용한다. 박스에는 인덱스가 없으므로 `rg`로 폴백하며, **bootstrap은 임베딩 모델을 내려받지 않는다.** 새 인덱스도 만들지 않는다.

## env-sync

`dev/env/main.toml`의 `[kiwi_sprouts]`가 Codex·Claude Code·Cursor에 이 서버를 등록한다. `script.darwin`/`script.windows`와 `root.darwin`/`root.windows`를 OS에 맞게 골라 `args = [script, "mcp"]`, `env = { KIWI_SPROUTS_ROOT = root }`로 배포한다.

## 박스 업데이트 후

```sh
cd /path/to/kiwi/dev/tools/kiwi-sprouts
./bootstrap.sh
```

멱등(idempotent) 스크립트다. python3 >= 3.10 확인, `~/.local`에 pyright/typescript/typescript-language-server 설치(없을 때만), rust-analyzer 컴포넌트 확인, sourcekit-lsp 경고, `kiwi-graph` 릴리스 빌드(바이너리 없을 때만), `~/.local/bin/kiwi-sprouts` 심볼릭 링크, `--health` unittest까지 수행한다.
TypeScript 7 네이티브 프리뷰처럼 `tsserver.js`가 없는 경우에는 호환되는 TypeScript 5.x를 `~/.local/lib/kiwi-sprouts/typescript`에 따로 설치한다.

## Rust 구현 검증과 설치

Pass 3의 Python 비교 기준은 승인된 Kiwi main `5a22e898`이다. 당시 Python 기준에 이번 SPEC의 ask/ask_resolve와 Windows 호환 수정을 함께 적용하며, 테스트와 단발 CLI가 같은 소스를 사용한다. `accounts/`, `secrets`, `.env*`, `*.key`, `*.pem`은 읽기·검색을 허용하고 `.git`만 민감 경로 검사에서 제외한다. rg 인수도 해당 커밋과 같으며 `--hidden --no-ignore`를 사용한다. 코드 탐색의 기존 `SKIP` 목록과 core의 `.git`·`target`·`node_modules` 제외는 그 커밋대로 유지한다. 템플릿은 Python을 계속 사용한다.

`kiwi_sprouts.py`는 비교 기준과 현재 운영 진입점으로 유지한다. Rust 실행 파일은 같은 15개 도구·스키마·stdio MCP·리소스와 `--root`, `--health`, `mcp`, `TOOL JSON_ARGS`를 제공한다. env-sync 설정과 `dev/env/templates/kiwi-sprouts-*.toml`은 Python을 계속 가리킨다.

Kiwi 묶음·규칙·상태·task guard는 기존 `kiwi-core`의 `kiwi` 실행 파일을, 운영 질의는 `kiwi-graph` 실행 파일을 그대로 호출한다. `KIWI_BIN`, `KIWI_GRAPH_BIN`, 명령행 인수·종료 코드 처리를 유지하려고 이 경계를 보존했다. SQLite 리소스 목록은 `kiwi-core::rule_resources`를 재사용한다. `kiwi-agent`는 check/finish 전용 실행 파일이며 서버가 호출하던 API가 없어 새 의존성을 만들지 않았다.

15개 MCP 도구의 디스패치, LSP, 심볼/참조 폴백, 파일 수정, 검색 필터는 Rust다. Decisions는 기존 `tools/decisions/{adapter,decide,ask}.py`를 작은 상주 Python 연결 코드에서 호출한다. 해당 모듈·인증·환경변수·fallback을 중복 구현하지 않는다. `PYTHONIOENCODING`의 명시적 코덱·오류 처리와 JSON의 비표준 상수/오류 문구도 Python 표준 라이브러리 호환 계층을 재사용한다. 기본 UTF8·Windows ANSI 디코딩, 임의 정밀도 정수와 Python식 실수 출력은 Rust에서 처리한다.

Python을 완전히 제거한 실행 파일은 아니다. 단발 `TOOL JSON_ARGS` CLI와 잘못된 MCP 문법의 traceback 표시는 Python 기준 구현인 `kiwi_sprouts.py`를 사용해 파일명·스택·오류 출력까지 유지한다. 문법 오류 경로에는 실패한 입력 줄만 전달하며, 앞서 성공한 MCP 요청을 다시 실행하지 않는다. `--health`는 기존 unittest를 Rust 대상으로 실행한다. 따라서 Python과 원본 소스는 설치 후에도 필요하다. 기존 키 로더를 재사용하며, ask의 의미 검색 캐시는 `ask.py index`로 명시적으로 만든다.

macOS에서는 기존 Python 설치 바로 옆에 선택적으로 Rust도 설치한다:

```sh
cd ~/kiwi/dev/tools/kiwi-sprouts
KIWI_SPROUTS_BUILD_RUST=1 ./bootstrap.sh
```

Python 링크는 `~/.local/bin/kiwi-sprouts`, 새 실행 파일은 `~/.local/bin/kiwi-sprouts-rs`다. Mac mini에서는 Rust 빌드를 Windows에서 검증한 뒤, 플랫폼별 최종 빌드가 필요한 경우에만 부하 규칙에 따라 실행한다. `bootstrap.sh`의 기본 실행은 기존대로 Python만 설치한다.

Windows의 현재 Python MCP 경로는 `dev/env/main.toml` 기준으로 `python %USERPROFILE%\.kiwi\src\kiwi\dev\tools\kiwi-sprouts\kiwi_sprouts.py mcp`다. Rust는 별도 경로에 설치한다:

```powershell
Set-Location "$env:USERPROFILE\.kiwi\src\kiwi\dev\tools\kiwi-sprouts"
.\install-rust.ps1
& "$env:USERPROFILE\.local\bin\kiwi-sprouts-rs.exe" mcp
```

두 설치 방법은 테스트용 `kiwi-sprouts-test-driver`와 `kiwi-sprouts-fixture`도 같은 bin 디렉터리에 둔다. 설치한 실행 파일의 `--health`와 테스트 runner가 사용하며 MCP 도구는 추가하지 않는다. 소스와 tests 디렉터리를 유지해야 `--health`가 동작한다.

빌드·검증은 다음처럼 실행한다. Windows에서는 `python3` 대신 `python`, 실행 파일에는 `.exe`를 쓴다. `CARGO_TARGET_DIR`은 작업별 Scratch 경로로 지정한다.

```sh
cargo build --locked --bins --manifest-path Cargo.toml
cargo test --locked --manifest-path Cargo.toml -- --nocapture
cargo clippy --locked --manifest-path Cargo.toml --all-targets -- -D warnings
KIWI_SPROUTS_BIN="$CARGO_TARGET_DIR/debug/kiwi-sprouts" python3 -m unittest discover -s tests -p test_parity.py -v
```

`tests/sprouts_support.py`는 `KIWI_SPROUTS_BIN`을 지정하면 기존 테스트의 호출을 Rust로 보내고, 생략하면 원래 Python을 실행한다. `KIWI_TEST_CORE_BIN`으로 별도 빌드한 실제 `kiwi-core` 실행 파일을 지정할 수 있다. `cargo test`는 모든 도구의 parity와 기존 suite의 Python/Rust 실행을 포함한다. core 경로를 지정하지 않으면 테스트용 core를 Scratch에 빌드한다. 기존 테스트 전체를 결정적으로 실행하는 명령은 다음과 같다:

```sh
python3 tests/run_existing.py --binary "$CARGO_TARGET_DIR/debug/kiwi-sprouts" --synthetic-lsp
```

동등성 검사는 네트워크·실제 키 없이 합성 LSP/검색/Kiwi/Decisions를 사용하고, 응답 JSON을 재정렬하거나 내용 문자열·줄바꿈을 정규화하지 않고 stdout·stderr의 원시 바이트·종료 코드와 수정된 파일 바이트를 비교한다. 기본 유니코드 테스트에는 `PYTHONUTF8=1`, `PYTHONIOENCODING=utf-8`을 설정하며 별도 사례에서 cp949, 기본 ANSI, replace/backslashreplace, UTF16, CR/LF/CRLF와 EOF 전 응답을 확인한다. 큰 정수는 ±2^128 및 i64 경계, 실수는 경계 예와 고정 seed의 무작위 비트 패턴 3,000개 중 유한 f64 2,999개로 검사한다.

Windows 파일 URI 오류를 Python/Rust 양쪽에서 수정했다. Rust/Swift의 결정적 전체 테스트는 합성 LSP로 수행하고, 설치된 실제 rust-analyzer도 Python 테스트로 확인한다. 실제 Python·TypeScript LSP도 별도로 검증한다. macOS 설치와 실제 sourcekit-lsp는 해당 플랫폼에서 추가 검증해야 한다.

## 나중에 전환할 때 바뀌는 것

이번 변경은 전환하지 않는다. 전환 전에는 두 OS에서 같은 설치 경로·언어 서버·환경변수로 검증을 완료해야 한다. 전환 시에는 다음 항목을 함께 바꿔야 한다:

1. `dev/env/main.toml`의 `[kiwi_sprouts]`에 OS별 Rust 실행 파일 경로를 지정한다. macOS command는 `~/.local/bin/kiwi-sprouts-rs`, Windows command는 `%USERPROFILE%\.local\bin\kiwi-sprouts-rs.exe`의 확장된 절대 경로다.
2. `dev/env/bin/env-sync`의 `render_kiwi_sprouts`와 검증을 확장해 Rust의 args를 `["mcp"]`로 만든다. 현재 generator는 Python용 `[script, "mcp"]`를 무조건 만들므로 command만 바꾸면 안 된다. `script.darwin`/`script.windows`는 Python rollback용으로 남기거나 별도 backend 필드로 구분한다.
3. Codex `mcp_servers.kiwi-sprouts`와 Claude·Cursor `mcpServers.kiwi-sprouts`에 새 command·args를 배포한다. 기존 7개 `kiwi-sprouts-*.toml`과 Codex 템플릿은 동일한 토큰을 참조하며, env의 `KIWI_SPROUTS_ROOT`와 도구명은 유지한다. 직접 등록한 16개 봇 설정도 같은 범위에서 확인하고 MCP 프로세스를 재시작한다.
4. Python rollback은 command를 기존 OS별 `python3`/`python`, args를 `[기존 script, "mcp"]`로 되돌리는 것이다. Python 소스·링크는 검증과 전환이 끝날 때까지 유지한다.

## MCP ask / ask_resolve

Python과 Rust에 같은 두 도구를 제공한다.

- `ask`: 필수 `question`, 선택 `options`(문자열 배열), `task`, `principal`. Kiwi의 근거·확신도와 답을 반환하며 위험하거나 확신도가 낮으면 `오너 확인 필요`를 반환한다.
- `ask_resolve`: 필수 `ask_id`, `actual`. 실제 답을 기록하고 기존 근거 가중치를 조정한다.

Python은 `KIWI_ROOT/tools/decisions/ask.py`를 import한다. Rust는 기존 Decisions Python 연결 프로세스의 `ask`/`ask_resolve` 메서드를 통해 같은 함수를 호출한다. 공개 인자 외의 `threshold`, `kiwi`, `opener` 등은 Python/Rust와 내부 bridge에서 모두 거부한다. `SystemExit`는 `decide`와 같은 `Decisions not configured...` 오류로 바꾸고 다음 요청을 계속 처리한다. 실제 ask는 키가 없을 때 어휘 근거와 `needs_user`를 반환한다. 응답·기본값·오류는 fake 모듈과 실제 ask를 사용한 wire parity로 비교한다. 임베딩 캐시·기본 활성화·전송 제외 경로·`KIWI_ASK_SEMANTIC=off` 설정은 Decisions README를 따른다.

Windows의 기존 검증 오류도 수정했다. 파일 URI는 Python `Path.as_uri()`와 같은 `file:///C:/...` 형태를 사용해 실제 rust-analyzer가 받아들이고, 파일 경로로 되돌릴 수 있다. UTF-16 출력의 첫 BOM은 Windows의 Python stdout과 동일하게 처리한다. 원시 바이트 parity와 URI 왕복 검사가 회귀를 확인한다.
