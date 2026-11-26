---
status: accepted
date: 2026-11-26
---

# ADR-0021: `hotplug-recovery` in 0.5.0: a failed watcher registration fails `start()`, and macOS is withheld

## Context and Problem Statement

[ADR-0019](0019-hotplug-auto-recovery.md) introduced the opt-in `hotplug-recovery` feature for desktop `SystemDefault` capture. It is unchanged. Three things it left open had to be settled before the feature's first release:

1. When the device-change watcher cannot be registered, the supervised capture started anyway and logged at info. On Windows the watcher is the only thing that notices a default-device switch while the old endpoint still works, so the capture looked healthy and silently stopped following the default (issue #114).
2. On macOS the watcher listens to the default-output device while `SystemDefault` builds its aggregate from the system-output (alerts) device (issue #107). When the two differ, a default switch reports `Recovered` on an aggregate that still hangs off the alerts device. The macOS recovery path has never been observed on hardware: hosted runners cannot grant the audio-capture permission, and no attended Mac was available.
3. The wait for an absent device probed every `max_backoff` and gave up at `device_wait_timeout`. With `max_backoff` not smaller than the wait, it could give up without a single further lookup.

## Decision Drivers

- Correctness before breadth: an opt-in that reports success must do what it says.
- No public surface that cannot be removed later.
- The feature has never been released, so refusing it on a platform breaks nobody.
- macOS code cannot be exercised by the people making this decision; Windows and Linux can.

## Considered Options

For the watcher failure:

- Keep the capture running, retry the registration in the background, and expose a degraded state through new snapshot fields and events.
- Fail `start()` where the watcher is the only trigger for a default switch.
- Keep the old behaviour and log at warn.

For macOS in 0.5.0:

- Ship the opt-in with an "unverified on macOS" caveat and a recorded waiver.
- Refuse the opt-in on macOS until an attended run exists.
- Hold the release for an attended run.

## Decision Outcome

**Watcher registration.** `start()` on a capture built with `with_auto_recovery`:

- registers the watcher before it opens a stream;
- on a platform that reopens on a default change (Windows, and macOS once enabled), returns the registration error. No stream is opened, no session begins, and the snapshot is unchanged. The error is transient (`Timeout` after five seconds, or `BackendError`): the caller retries `start()`, or builds without auto-recovery;
- on Linux, where PipeWire moves the stream to the new default itself, logs one warning and continues;
- on a backend whose `watch()` returns `PlatformNotSupported`, continues as before: recovery from stream invalidation still works.

The retry-and-degrade option was chosen by four of six panelists and rejected on a fact about the supervisor: a late registration cannot repair the time it missed. The OS replays no missed default switch, and the only repair available, forcing a default-change trigger when the registration finally succeeds, closes and reopens a healthy stream, emits a false `DefaultChanged` and counts a recovery. Comparing the bound endpoint's id per generation would avoid that at the price of permanent public types (`RecoverySnapshot` fields, two event variants) and a retry thread. Failing the start can be relaxed into that design later without a break; the reverse is not possible.

**macOS.** In 0.5.0 a macOS build that sets `with_auto_recovery` fails `preflight()` and `build()` with `PlatformNotSupported`. Plain `SystemDefault` capture and the device watcher are unchanged. The opt-in is enabled again in one change that also makes the system tap's aggregate follow the default-output device (the application and process-tree taps keep the system-output device), and that change merges only together with the record of an attended run.

Shipping with a caveat was chosen by four of six panelists. It was rejected because it is coherent only if the selector is changed now, and that change alters plain `SystemDefault` capture for every macOS user with nothing but permission-skipped CI behind it.

**Absent-device wait.** When `device_wait_timeout` passes, the supervisor runs exactly one final probe, and the wait ends as exhausted only if that probe finds no device.

### Consequences

- Good: on Windows a successful `start()` means default switches are being watched.
- Good: no new public type, field or variant.
- Good: nothing about macOS recovery is claimed before it has been seen.
- Bad: on Windows a transient COM stall during registration refuses a capture that would otherwise have run without following the default. The caller has to retry or drop the opt-in.
- Bad: macOS users cannot opt in until someone runs the attended procedure below.
- Neutral: `docs/features.md` lists Windows and Linux for the feature.

### The attended macOS run

Enabling the opt-in on macOS needs a record of one run on a Mac with the audio-capture permission granted, on the commit that contains the selector change. The record states:

- the Mac model, `sw_vers`, the rsac commit, the signing identity of the test binary, and a non-silent permission probe before the test;
- with the sound-effects output pinned to a different device than the normal output: both selectors' device ids and UIDs before and after every switch, read back after each change (`SwitchAudioSource` 1.2.2 exits zero on a failed set);
- one `SystemDefault` session with auto-recovery held open across A→B→A: the `DefaultChanged` and `Recovered` events, the generations, the delivered format per generation, at least ten seconds of non-silent audio per generation, and the live aggregate's main sub-device UID after each reopen, read through the API. Tones cannot prove endpoint identity under a global tap; the sub-device UID is the identity;
- the same switch with recovery off, including whether audio simply continues (the tap is global, so macOS may need no reopen at all; if so, the right change is to stop reopening on macOS);
- a change of the sound-effects output alone causing no reopen;
- a Multi-Output device as the default;
- a `stop()` during a reopen, and no hang in any step.

A skip, silence or a timeout is a failed run. Report only the configurations tested.

## More Information

- Decision record: `docs/DESKTOP_REFINEMENT.md`, "panel 2" (P1, P2 and P6), with the vote splits and the reasons for overruling two majorities.
- Evidence on Windows and Linux for the release: `docs/evidence/` and the audio workflow's hotplug tiers (issue #94).
- Issues: #114 (watcher failure), #107 (macOS selectors), #102 (Linux snapshot completion and the final probe), #94 (evidence).
