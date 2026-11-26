# Windows: `hotplug-recovery` follows a live default-device switch

Tested-SHA: e86e8bae59441988178c28cfa116888acf0492ba

This note is workstation evidence that the hosted Windows runner cannot produce: the runner has one render endpoint, and a switch needs two. The release gate (`scripts/tag-release.sh`) reads the line above from the commit being tagged and refuses to tag if any of these paths changed since that commit: `src/api/recovery.rs`, `src/core/recovery.rs`, `src/audio/windows`, `tests/ci_audio/hotplug_common.rs`, `tests/ci_audio/hotplug_windows.rs`. When they change, run the tests again on the new commit and update this note.

## What was run

Windows 11 (10.0.22631.6199), Rust 1.95.0, 2026-11-26. The tests are `tests/ci_audio/hotplug_windows.rs`; the watched paths in the working tree were identical to the tested commit.

```
RSAC_HOTPLUG_SWITCH=1 \
RSAC_HOTPLUG_ENDPOINT_A='<endpoint id A>' RSAC_HOTPLUG_ENDPOINT_B='<endpoint id B>' \
cargo test --test ci_audio --no-default-features \
  --features feat_windows,compose,sink-wav,hotplug-recovery \
  hotplug_windows -- --test-threads=1 --nocapture
```

Endpoint ids are the ones `rsac list` prints. A was `CABLE Input (VB-Audio Virtual Cable)` and B was `Speakers (Bigscreen Audio Stream 1.2)`, two virtual endpoints with a 48 kHz stereo mix format. The tests set the `eConsole` and `eMultimedia` defaults through `IPolicyConfig::SetDefaultEndpoint` (the undocumented interface the Sound settings use), read each change back through `GetDefaultAudioEndpoint`, and restore the original defaults for every role they touched. A 440 Hz tone plays on A and a 997 Hz tone on B for the whole test, each from a player bound to its endpoint by id, so the tone in the captured audio says which endpoint the capture is on.

## Results

| Test | Result |
|---|---|
| `hotplug_switch_follows_default_with_recovery` | One `SystemDefault` session with auto-recovery, held open across A→B→A. Stream generations 0, 1, 2; `DefaultChanged` then `Recovered` after each switch; B audio 520 ms after the first switch, A audio 537 ms after the second. Worst tone dominance in a counted window: 48.8 dB (the threshold is 20 dB over five consecutive 200 ms windows). |
| `hotplug_switch_without_recovery_stays_on_old_endpoint` | The same switch without auto-recovery: 15 of 15 windows after the switch on A, none on B, generation 0. Recovery is what moves the capture. |
| `hotplug_communications_role_switch_is_ignored` | Changing only the communications default: no recovery event, no new generation, 15 of 15 windows on A. |

The machine's defaults were as before after the run.

Earlier runs of the same tests on the pull-request branch also passed between a 48 kHz stereo endpoint and a 96 kHz eight-channel one (recovery 620 ms and 450 ms).

## The check can fail

With the library changed so that it never reopens on a default change, `hotplug_switch_follows_default_with_recovery` fails with "DefaultChanged then Recovered (generation 1) after switching to B: not reached", and the defaults are still restored. (That experiment also taught a process lesson recorded in `AGENTS.md`: it was built in a second worktree that shared the main target directory, and four later runs in the main worktree linked the broken library and failed the same way until the artifacts were cleaned.)

## What this does not show

- Physical devices (USB, Bluetooth, HDMI), a switch made in the Settings app, or a Windows version other than the one above.
- Endpoint loss. That runs on the hosted runner in the audio workflow's "Hotplug recovery tier": with the only endpoint hidden, the capture ends with `StreamEnded` once the absent-device wait expires (about 3.2 s with a 3 s wait), and it recovers into generation 1 about 0.2 s after the endpoint returns (audio runs 45896704998, 45900211449, 45902679795).
- Anything on macOS: the opt-in is refused there in 0.5.0 (ADR-0021).
