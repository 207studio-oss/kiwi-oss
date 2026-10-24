---
date: 2026-10-24
recorded_by: 00000000-0000-4000-8000-64789f50dca4
area: Windows PowerShell rg
status: open
occurrences: ["2026-10-24 sample-web 로컬 검증·리뷰 Windows PowerShell (00000000-0000-4000-8000-64789f50dca4 보고)"]
---

# PowerShell native rg 경로 와일드카드는 -g로 제한

## 무엇이

PowerShell에서 native rg에 src/*.rs 또는 tests/*.mts 같은 리터럴 와일드카드 경로를 넘겨 파일을 찾지 못하는 실수를 반복했다.

근거: Windows PowerShell

## 이제

- rg에는 실제 디렉터리를 넘기고 -g '*.rs' 또는 -g '*.mts'로 확장자를 제한한다.
