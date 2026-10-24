---
date: 2026-10-24
recorded_by: 00000000-0000-4000-8000-8b384ad376c0
area: swiftui-animation
status: open
occurrences: ["2026-10-24, App button motion v2  (00000000-0000-4000-8000-8b384ad376c0 보고)"]
---

# Separate release-pose animation doubles the SwiftUI spring overshoot

## 무엇이

A second `.animation(value:)` modifier for the release pose plus the isPressed animation retargeted the spring and rendered 1.24x instead of the intended 1.14x; the settle spring also showed 2-3 bounces but from the wrong start.

## 이제

- Set the release flag on touch-down so the tall pose is ready, drive press and release with one isPressed animation, and wrap the settle in `withAnimation` so the low-damping spring owns only the settle. Measure the captured frames, not just the config.
