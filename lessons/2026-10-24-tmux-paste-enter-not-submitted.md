# tmux로 붙여넣은 지시는 전송됐는지 capture-pane으로 확인한다

- 날짜·출처: 2026-10-24 감마 보고.
- 문제: 11:15 KST쯤 감마이 window-bot(win-host)에 여러 줄 지시(#16 머지, 작업 폴더를 sample-window-v2로 이동, CW2-002/011/003 수정)를 tmux paste-buffer로 붙여넣었다. 첫 Enter가 먹히지 않아 지시가 입력창에만 남았고, 세션이 12:21까지 약 1시간 놀았다.
- 근본 원인: 여러 줄 paste-buffer 뒤 Enter를 누르고 전송 여부를 확인하지 않았다.
- 다음엔: 붙여넣고 Enter를 누른 뒤 반드시 capture-pane으로 입력줄이 비었는지, 또는 'queued' 표시가 떴는지 확인한다. 안 됐으면 Enter를 다시 누르고 다시 확인한다. 입력창에 남은 글을 C-u로 지우기 전에 그게 보내지 않은 지시인지 먼저 확인한다.
- 승인 키도 같다: 12:47 KST쯤 감마이 win-kiwi-bot(sample-board #219 리뷰)에 permission_prompt 승인 키 '4'를 보냈는데 프롬프트가 이미 닫혀 일반 메시지 '4'로 들어갔다(kiwi-bot가 무시, 피해 없음). 승인 키를 보내기 직전 capture-pane으로 권한 프롬프트가 아직 떠 있는지 확인하고, 없으면 보내지 않는다.
- occurrences: 2026-10-24 감마 → window-bot; 2026-10-24 감마 → kiwi-bot(승인 키).
