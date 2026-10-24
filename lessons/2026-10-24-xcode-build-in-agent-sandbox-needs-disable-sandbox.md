---
date: 2026-10-24
recorded_by: app-bot
area: mac/xcode
status: open
occurrences: ["2026-10-24 App iPad/Mac xcodebuild on the Mac mini (app-bot 보고)"]
---

# Xcode build in agent sandbox needs -disable-sandbox

## 무엇이

xcodebuild in the agent sandbox failed with "sandbox-exec: sandbox_apply: Operation not permitted" while compiling SwiftUI macros. Setting SWIFTC_DISABLE_SANDBOX, SWIFTPM_DISABLE_SANDBOX, or IDEPackageSupportDisableManifestSandbox did not help.

근거: App iPad/Mac xcodebuild on the Mac mini

## 이제

- Pass OTHER_SWIFT_FLAGS='$(inherited) -disable-sandbox' to xcodebuild so macro-plugin subprocesses are not sandboxed. For package resolution, copy an existing DerivedData SourcePackages to a writable -clonedSourcePackagesDirPath and add a matching Package.resolved, then use -skipPackageUpdates -disableAutomaticPackageResolution.
