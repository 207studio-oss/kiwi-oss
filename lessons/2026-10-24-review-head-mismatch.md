---
id: lesson-2026-10-24-review-head-mismatch
title: 리뷰 올리기 직전 head SHA 비교
date: 2026-10-24
source: 감마 (sample-app, sample-board #209)
status: active
summary: PR 리뷰를 올리기 직전에 현재 head SHA를 검증한 SHA와 비교하고, 다르면 새 head에서 다시 확인한다.
---
# 리뷰 올리기 직전 head SHA 비교

sample-board #209에서 d7991801 기준 검증 리뷰가 새 head 7e3543da에 붙었다. 리뷰 직전 `gh pr view --json headRefOid`로 head를 검증 SHA와 비교하고(가능하면 커밋 지정), 다르면 새 head에서 다시 검증한 뒤 올린다.
