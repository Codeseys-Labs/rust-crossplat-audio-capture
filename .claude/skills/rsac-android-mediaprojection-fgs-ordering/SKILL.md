---
name: rsac-android-mediaprojection-fgs-ordering
description: |
  Android 14 (API 34) MediaProjection ordering trap: a mediaProjection-typed
  FGS may be started only AFTER consent, yet getMediaProjection() internally
  requires that FGS to be ALREADY confirmed-foreground — and
  startForegroundService() promotes asynchronously, so acquiring the
  projection inline after starting the service throws SecurityException
  deterministically. Use when: (1) touching RsacProjection/RsacCaptureService
  consent flow, (2) a consent flow ends in SecurityException from
  getMediaProjection or startForeground, (3) reviewing any "start FGS then
  getMediaProjection" sequence, (4) designing similar consent-then-service
  handoffs.
author: Claude Code
version: 1.0.0
date: 2026-07-18
---

# Android 14 MediaProjection: acquire inside the FGS, after startForeground

## Problem

Two platform constraints collide (enforced since API Q, still on 14):

1. A `mediaProjection`-typed FGS may be started only **after** the user
   grants consent (`MediaProjectionFgsTypeCustomPermission` →
   `isAllowedMediaProjectionNoOpCheck`, keyed on a token map populated by
   the consent grant). Pre-consent `startForeground()` → SecurityException.
2. `MediaProjectionManager.getMediaProjection()` constructs a
   `MediaProjection` whose ctor binder-calls `IMediaProjection.start()`,
   which throws unless `hasRunningForegroundService(uid,
   FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION)` — i.e. the FGS must be
   **confirmed-foreground already** (the javadoc says this outright).

The trap: "start the FGS on the consent-success path, then call
getMediaProjection inline" LOOKS like it satisfies both — but
`startForegroundService()` promotion is async (the service's
`startForeground()` runs on a later main-looper message), so the inline
`getMediaProjection` always runs first → **deterministic SecurityException
on every request**. Three parties missed this in wave 7 (implementer
reasoning, the seed's own fix sketch); a cross-model adversarial review
caught it from the AOSP source before any device ever ran it.

## Solution (shipped in rsac PR #64)

Defer acquisition INTO the service:

1. `request()`'s consent callback stashes `PendingAcquisition(resultCode,
   data, callback)` (single slot; reject concurrent second requests) and
   calls `RsacCaptureService.start(context)`.
2. `onStartCommand` calls `startForeground(id, notif, MEDIA_PROJECTION)` —
   a **synchronous** AMS binder call (`setServiceForeground` is not oneway);
   on return the FGS type bits are committed in AMS.
3. Immediately after, the service calls back
   (`RsacProjection.onForegroundServiceReady(service)`) which runs
   `getMediaProjection` + `nativeRetainProjection` and delivers the token.
   Same-main-thread ordering guarantees the stash is set before
   `onStartCommand` runs.
4. Every failure arm (startForeground throws, SecurityException, null
   projection, 0 token) stops the FGS and fires `onDenied` exactly once.

## Verification

- AOSP android14-release chain: Service.java:858 → IActivityManager.aidl:324
  (two-way) → ActiveServices.setServiceForegroundInnerLocked:2351 (type set)
  vs MediaProjectionManagerService.java:926-943 (the check).
- Reasoning-verified + gate/lockstep green; on-device confirmation tracked
  in rsac-e6d3 (the CI-VERIFY note scopes the happy path).

## Notes

- The general lesson: **"start service then call API inline" patterns are
  broken whenever the API checks confirmed-foreground state** — promotion is
  async from the caller's frame even though `startForeground()` itself is
  synchronous from *inside* the service.
- Consensus wrongness again (cf. rsac-post-stop-read-contract): the wrong
  inline fix was plausible to every same-context reader; the catch came from
  an independent reviewer instructed to verify against platform sources.
- See also: rsac-cabf seed, ADR references in mobile/android/README.md
  § step 3.
