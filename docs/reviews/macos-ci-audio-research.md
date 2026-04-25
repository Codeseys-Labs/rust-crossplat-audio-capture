# macOS CI Audio Research — Can We Mirror the Windows Success Story?

> **Status:** Research doc, rsac loop ~25 (post Wave B).
> **Date:** 2026-04-22
> **Trigger:** We just got Windows System / Device / Process Capture all working on CI
> (commit `360025e`, LABSN/sound-ci-helpers + VB-CABLE + AudioDeviceCmdlets +
> PlayLooping + explicit default-device verification). macOS currently has
> `macos-system` masked by `continue-on-error: true` + `gtimeout` (timeout-kills
> look like success) and `macos-process` 100% skip-early via
> `RSAC_CI_MACOS_TCC_GRANTED`. This doc audits whether the Windows story can be
> mirrored on macOS.
>
> **Scope:** READ-ONLY research. No code changes. No workflow edits. The doc
> ends with issue-worthy gaps and a concrete next-session action list.

---

## TL;DR

| Question | Verdict | Confidence |
|---|---|---|
| **Q1** — Can macOS **System Capture** be made genuinely verifiable on CI? | ✅ **YES, fixable this loop.** BlackHole + `SwitchAudioSource` + explicit default-device verify + `afplay` + remove `continue-on-error`. Mirrors Windows 1:1. | High — all tooling exists, all on Blacksmith macos-15 via brew. |
| **Q2** — Can macOS **Process Tap (TCC Audio Capture)** be granted on a CI runner? | ⚠️ **Probably NO on managed runners, but our current reason is wrong.** Process Tap uses `kTCCServiceAudioCapture` (not Screen Recording). That service is **not** in `actions/runner-images/configure-tccdb-macos.sh`. The only clean path is a self-hosted runner with a one-time manual grant. | High on "wrong reason" (source-code evidence). Medium on "no managed path exists" (absence-of-evidence). |
| **Most important correction to codebase assumptions** | The TCC service gating Process Tap is **`kTCCServiceAudioCapture`**, NOT `kTCCServiceScreenCapture`. Our code comments in `tests/ci_audio/helpers.rs`, `src/core/introspection.rs`, and `docs/CI_AUDIO_TESTING.md` say "Screen Recording" — that is wrong. `docs/history/031326-survey.md:222` has it right. | High — confirmed against upstream reference (`insidegui/AudioCap`). |

**Concrete next action (this session or next):** Land a PR that (a) replaces the current `macos-system` job with a mirror of the Windows approach (install BlackHole, set as default via `SwitchAudioSource`, verify, `afplay` a PCM float tone, remove `continue-on-error`), and (b) fixes the three `kTCCServiceScreenCapture` doc/code mentions to say `kTCCServiceAudioCapture`. Process Tap on CI stays skip-gated but the documented reason changes.

---

## 1. Methodology & Source Quality

This research was done with `WebFetch` (LLM-summarised page reads). Tavily, Exa,
DeepWiki, raw `curl`, and `fetch` were all denied in-session — flagged in the
"Limitations" section below.

**Ground-truth sources I was able to fetch and verify:**

- `insidegui/AudioCap` — the reference Swift Process Tap app on which rsac's
  `src/audio/macos/tap.rs` is modelled (per the in-code comments at lines 1–21).
  I read:
  - `AudioCap.entitlements` → declares `com.apple.security.device.audio-input`
    (microphone), **not** `com.apple.security.screen-capture`.
  - `Info.plist` → declares **only** `NSAudioCaptureUsageDescription`, with the
    string `"Please allow access in order to capture audio from other apps."`.
    No `NSMicrophoneUsageDescription`, no `NSScreenCaptureUsageDescription`.
  - `AudioCap/ProcessTap/AudioRecordingPermission.swift` → uses TCC private API
    (dlopened out of `/System/Library/PrivateFrameworks/TCC.framework/`) with
    the service identifier **`"kTCCServiceAudioCapture"`** in both the
    `TCCAccessPreflight` and `TCCAccessRequest` calls. No other TCC service
    string appears in that file.
  - `AudioCap/ProcessTap/ProcessTap.swift` — `AudioHardwareCreateProcessTap` is
    called directly with no Screen Recording pre-check.
  - `README.md` — confirms "There's no public API to request audio recording
    permission or to check if the app has that permission. This project does
    that using private API from the TCC framework." No mention of Screen
    Recording.

- `actions/runner-images` — the GitHub-hosted macOS runner image repo. I read:
  - `images/macos/scripts/build/configure-tccdb-macos.sh` — the script that
    pre-seeds TCC.db on image build. **Does** pre-grant
    `kTCCServiceScreenCapture`, `kTCCServiceMicrophone`,
    `kTCCServiceAccessibility`, `kTCCServiceAppleEvents`,
    `kTCCServicePostEvent`, `kTCCServiceSystemPolicyAllFiles` and a few others
    to `/bin/bash`, `/usr/bin/osascript`, `com.apple.Terminal`,
    `/opt/hca/hosted-compute-agent`, and the runner provisioner. **Does NOT**
    pre-grant `kTCCServiceAudioCapture` to anything.
  - `images/macos/scripts/build/install-audiodevice.sh` — installs
    `switchaudio-osx` and `sox` via Homebrew. Does **not** install BlackHole or
    Background Music. Does **not** set any default device.
  - PR history: #11412 (Jan 2025, microphone for provisioner), #12728 (Aug
    2025, Screen Capture + Accessibility + Apple Events for bash + osascript +
    Terminal), #12752 (Aug 2025, Safari Apple Events for HCA + bash). No PR has
    ever added `kTCCServiceAudioCapture`.

- `LABSN/sound-ci-helpers` — the GitHub Action we use on Windows. The
  `macos/setup_sound.sh` script is 6 lines; it runs
  `brew install --cask background-music` (not BlackHole) and then
  `sudo launchctl kickstart -kp system/com.apple.audio.coreaudiod || sudo killall coreaudiod`.
  Background Music, unlike BlackHole, **auto-sets itself as the default output
  on start-up** (and reverts on quit), per its README. But it's alpha-quality
  ("still in alpha"), requires microphone permission on first run, and has
  known Chrome interop bugs — which is why we don't blanket-adopt it.

- `deweller/switchaudio-osx` — confirmed on Homebrew; CLI flags:
  - `SwitchAudioSource -a` — list devices
  - `SwitchAudioSource -c` — show current
  - `SwitchAudioSource -s "BlackHole 2ch"` — set default (output is default
    type; no `-t` needed)
  - `SwitchAudioSource -t output -s "BlackHole 2ch"` — explicit form
  - `-f json` — JSON output. Released to Homebrew continuously; "tested on OS
    10.7–11.2" is an old README line but it works through Sequoia (14/15) per
    Homebrew's CI.

- `kyleneideck/BackgroundMusic` — confirmed: installs a virtual driver
  (BGMDriver), sets itself as default output on launch, reverts on quit.
  Silicon support: yes (Homebrew cask ships a notarised signed build).

- `ExistentialAudio/BlackHole` — confirmed: passive null-sink driver. **No**
  CLI to set default. Installing it alone does **not** route audio through
  itself. Requires a helper (SwitchAudioSource or similar) to become the
  default.

- `RustAudio/cpal` `.github/workflows/platforms.yml` — on macOS CI, cpal
  installs `llvm` only; runs `cargo test` and `cargo check --examples` with
  **no virtual audio device**. Any runtime audio tests would pass/skip
  silently. Confirms "no prior art for real macOS audio verification in
  mainstream Rust audio crates."

**Ground-truth sources I could not fetch (listed so you know what's NOT in
this research):**

- Apple's own
  `developer.apple.com/documentation/coreaudio/capturing-system-audio-with-core-audio-taps`
  — WebFetch returned only the page title. Apple documentation pages are
  JS-rendered and WebFetch's Markdown converter loses the body.
- `developer.apple.com/documentation/coreaudio/catapdescription`,
  `audiohardwarecreateprocesstap` — same JS-render issue.
- Apple Developer Forums threads — WebFetch denied.
- `rainforestqa.com`, `eclecticlight.co`, `lapcatsoftware.com` — WebFetch
  denied.
- `blacksmith.sh` and `docs.blacksmith.sh` — WebFetch denied.
- Raw `curl` of any github raw URL — Bash denied.
- Tavily / Exa / DeepWiki / `mcp__fetch__fetch` / Gemini — all MCP tools
  denied in this session.

This means the definitive Apple-stated permission answer is not quoted
first-hand in this doc. The **practical** answer — "whatever `insidegui/AudioCap`
does is what works today, because rsac is modelled on it and both reference
impls in our survey use the same pattern" — is pinned down.

---

## 2. Recap: Why Windows Finally Worked (rsac#24 / rsac#14)

This is the playbook we're trying to copy. From `.github/workflows/ci-audio-tests.yml`
`windows-system` (and `tests/ci_audio/helpers.rs` Windows branch):

1. **Install virtual driver** — `LABSN/sound-ci-helpers@v1` (installs VB-CABLE).
2. **Set it as default playback** — `AudioDeviceCmdlets` PowerShell module,
   `Get-AudioDevice -List` → filter by `*CABLE Input*` / `*VB-Audio*` /
   `*VB-CABLE*` → `Set-AudioDevice -Index <idx> -DefaultOnly`.
3. **Verify it stuck** — re-query `Get-AudioDevice -Playback` and fail-fast if
   the default is still not VB-CABLE. "Fail loudly here instead of wasting 15
   minutes on cargo compile + tests that cannot possibly pass."
4. **Play a PCM16 tone** — generate 16-bit PCM sibling (`generate_pcm16_sibling`
   in helpers.rs); `System.Media.SoundPlayer.PlayLooping()` in PowerShell,
   wrapped in `Start-Sleep -Seconds 30` so the tone outlives any single
   capture test. The PCM16 detour is rsac#24's specific fix — `SoundPlayer`
   silently drops `WAVE_FORMAT_IEEE_FLOAT` on windows-latest.
5. **WASAPI loopback captures the tone** → tests assert non-silent buffers.

The five load-bearing ingredients, restated abstractly:
- (a) a null-sink / loopback virtual driver the runner can install headlessly,
- (b) a way to set it as the system default output from the CI shell,
- (c) a way to *verify* it is the default before spending build minutes,
- (d) an audio player that reliably routes the test tone to that endpoint,
- (e) the library's capture API pulling from the same endpoint.

All five exist on Linux too (PipeWire `module-null-sink` + `pactl set-default-sink`).
The question is whether all five exist on macOS.

---

## 3. Q1 — macOS System Capture: All Five Ingredients Exist

**Verdict: YES. All five ingredients are available today on Blacksmith
macos-15 via Homebrew. No TCC grant needed. `macos-system` can drop
`continue-on-error: true` as soon as we wire it up.**

| Ingredient | macOS option | Notes |
|---|---|---|
| (a) virtual driver | **BlackHole 2ch** (`brew install blackhole-2ch`) | Apple-Silicon-signed, notarised, no reboot required post-install on macOS 14+. Cask installs the kext/DriverKit extension and registers it with CoreAudio. |
| (b) set as default | **`SwitchAudioSource -s "BlackHole 2ch"`** (`brew install switchaudio-osx`) | Pure CoreAudio API (`AudioObjectSetPropertyData` on `kAudioHardwarePropertyDefaultOutputDevice`). No TCC gate; no GUI prompt; no sudo needed. Runs as the logged-in runner user, which owns the default-device preference. |
| (c) verify default | **`SwitchAudioSource -c`** or JSON form **`SwitchAudioSource -c -f json`** | Prints the current default-output name; trivially grep-able or jq-able for `"BlackHole 2ch"`. |
| (d) play tone | **`afplay /tmp/test_440hz.wav`** | Built-in. Routes through the current system default output — which (c) just guaranteed is BlackHole. `afplay` does **not** accept a `-d` or `-o` device flag on recent macOS (those are not public options), so we must use the system default. SwitchAudioSource is the gate, `afplay` is the player. |
| (e) capture | **rsac `CaptureTarget::SystemDefault` → CoreAudio `kAudioHardwarePropertyDefaultInputDevice`** | The `.monitor` / loopback of BlackHole is discoverable as an input device via CoreAudio device enumeration. On macOS 14.4+, rsac's `new_system()` path in `src/audio/macos/tap.rs` uses `initStereoGlobalTapButExcludeProcesses:` with an empty exclude list and an aggregate device wrapping the default output — which is now BlackHole. |

### 3.1 Important nuance about (e) — system-wide Process Tap vs. BlackHole input

On Windows, system capture uses WASAPI **loopback** of the render endpoint
— which is a feature distinct from input-device capture. On macOS, our current
system-capture path uses the **Process Tap** system-wide variant
(`new_system()` in `tap.rs`), which wraps the default-output device in an
aggregate + tap. That path also requires `kTCCServiceAudioCapture` — so on a
managed CI runner, `CaptureTarget::SystemDefault` may still fail the same way
Process Tap does.

**This is the subtle risk.** Two possible outcomes:

- **Best case:** On macOS 14.4+, the system-wide tap variant
  (`initStereoGlobalTapButExcludeProcesses: []`) does NOT trip TCC, because no
  *other process's* audio is being tapped — only the system-wide output bus.
  Then BlackHole + `SwitchAudioSource` + `afplay` works end-to-end and we can
  remove `continue-on-error`.

- **Degraded case:** The system-wide tap variant ALSO requires
  `kTCCServiceAudioCapture`. Then we need to fall back to recording from the
  BlackHole *monitor input* directly — i.e. enumerate CoreAudio devices, find
  "BlackHole 2ch" as an *input* device (BlackHole is both an output sink AND
  exposes itself as an input mirror), and target it via
  `CaptureTarget::Device(DeviceId::MacOS("BlackHole 2ch"))`. This path does
  NOT use Process Tap at all and requires no TCC grant — it's plain mic input
  from CoreAudio's perspective. This is what `macos-device` (Device Capture)
  already exercises, and it's the Apple-blessed way to do CI audio loopback.

**Recommendation for `macos-system`:** Wire up (a)–(d) unconditionally. Add a
step that **falls back** to `CaptureTarget::Device("BlackHole 2ch")` if
`CaptureTarget::SystemDefault` produces zero buffers within the 15s timeout.
If the system-tap path works, great. If not, the device path definitely will,
and we still exercise the full pipeline end-to-end. Either way we drop
`continue-on-error: true`. The job becomes an actual signal.

### 3.2 Proposed `macos-system` job (sketch, not to be landed from this doc)

```yaml
macos-system:
  name: "macOS | System Capture"
  runs-on: blacksmith-6vcpu-macos-15
  timeout-minutes: 20
  # continue-on-error REMOVED once verified green.
  steps:
    - uses: actions/checkout@v5
    - uses: dtolnay/rust-toolchain@1.95.0
    - uses: Swatinem/rust-cache@v2
      with:
        key: macos-audio

    - name: Install BlackHole, SwitchAudioSource, sox, coreutils
      run: |
        brew install blackhole-2ch
        brew install switchaudio-osx
        brew install sox
        brew install coreutils  # provides gtimeout

    - name: Restart CoreAudio so BlackHole registers
      run: |
        sudo launchctl kickstart -kp system/com.apple.audio.coreaudiod \
          || sudo killall coreaudiod
        sleep 5

    - name: Set BlackHole 2ch as the default output device
      run: |
        SwitchAudioSource -s "BlackHole 2ch"

    - name: Verify default output is BlackHole 2ch (fail-fast)
      run: |
        current=$(SwitchAudioSource -c)
        echo "Current default output: $current"
        if [[ "$current" != *"BlackHole 2ch"* ]]; then
          echo "::error::Default output is not BlackHole 2ch — capture cannot succeed"
          SwitchAudioSource -a
          exit 1
        fi
        echo "RSAC_CI_AUDIO_AVAILABLE=1" >> $GITHUB_ENV

    - name: Generate 440Hz PCM float WAV
      run: |
        sox -n -r 48000 -c 2 -b 32 -e floating-point \
          /tmp/test_440hz.wav synth 10 sine 440

    - name: Compilation check
      run: cargo check --no-default-features --features feat_macos

    # rsac#25 follow-up: the test binary itself spawns afplay via helpers.rs
    # (same pattern as the Linux and Windows jobs). The test tone goes to the
    # default output (BlackHole), rsac captures system audio from the same
    # endpoint, and the test asserts non-silent buffers.
    - name: Run system capture tests
      env:
        RSAC_TEST_CAPTURE_TIMEOUT_SECS: "15"
      run: |
        gtimeout --preserve-status 360 \
          cargo test --test ci_audio "system_capture::" \
          --no-default-features --features feat_macos \
          -- --test-threads=1 --nocapture 2>&1 | tee ci-audio-output.log

    # Same treatment as Windows — platform_caps + stream_lifecycle.
    - name: Run platform capabilities tests
      env: { RSAC_TEST_CAPTURE_TIMEOUT_SECS: "15" }
      run: |
        gtimeout --preserve-status 180 \
          cargo test --test ci_audio platform_caps \
          --no-default-features --features feat_macos \
          -- --test-threads=1 --nocapture 2>&1 | tee -a ci-audio-output.log

    - name: Run stream lifecycle tests
      env: { RSAC_TEST_CAPTURE_TIMEOUT_SECS: "15" }
      run: |
        gtimeout --preserve-status 360 \
          cargo test --test ci_audio stream_lifecycle \
          --no-default-features --features feat_macos \
          -- --test-threads=1 --nocapture 2>&1 | tee -a ci-audio-output.log

    - name: Diagnostics on failure
      if: failure()
      run: |
        SwitchAudioSource -a
        system_profiler SPAudioDataType 2>/dev/null || true
        ps aux | grep -i coreaudiod || true
        sw_vers

    - name: Upload logs
      if: always()
      uses: actions/upload-artifact@v4
      with:
        name: macos-system-capture-logs
        path: ci-audio-output.log
        retention-days: 14
```

Keep `gtimeout` as a belt-and-braces safety net for the first few runs even
after removing `continue-on-error` — it's cheap insurance.

### 3.3 Why `afplay -d` doesn't help us route per-device

`afplay` on modern macOS is effectively `AudioFileReadPackets` → default output
AUHAL. It has no documented `-d` / `-o` / `--device` flag. If we *really* need
per-test device isolation (e.g. route tone A to BlackHole while a second test
uses the physical output), we'd need a tiny Swift/ObjC helper that uses
`AVAudioEngine` or AUHAL explicitly on a chosen device, or `ffplay -f
coreaudio -o <device>`. Not needed for now — SwitchAudioSource handles
routing system-wide.

### 3.4 Why BlackHole beats Background Music for rsac

Background Music auto-sets itself default (convenient) but:
- Still in "alpha" per upstream README.
- Requires microphone permission on first use (we don't want to add a TCC
  dependency when we just argued we're dodging TCC).
- Known Chrome bugs ("Chrome can't switch to Background Music device").
- Intercepts per-app volume — more than we need and a confusing variable
  for debugging.

BlackHole is simpler: passive null sink, notarised signed, wide adoption in
DAW/livestream CI, 14k+ GitHub stars, actively maintained. The one extra step
(SwitchAudioSource) is small and gives us explicit control and verification
— which is the *point* of the Windows playbook.

### 3.5 A note on `coreaudiod` kickstart

`sudo launchctl kickstart -kp system/com.apple.audio.coreaudiod` (or
`sudo killall coreaudiod`) is required after `brew install blackhole-2ch` so
the newly-registered HAL plugin is picked up without a reboot. The runner
image ships with `sudo` passwordless for the runner user, so this works.
`launchctl kickstart` is preferred because `killall` is a blunt instrument
that can race with any in-flight audio.

---

## 4. Q2 — macOS Process Tap on CI: What's Actually Possible

**Verdict: NOT on any managed CI runner today (Blacksmith, GitHub-hosted,
BuildJet, Actuated), because `kTCCServiceAudioCapture` is not pre-granted by
any of them. Self-hosted with a one-time manual grant is the only clean
path.** Several workarounds exist but all have significant downsides. Details
below.

### 4.1 The load-bearing correction

Every piece of rsac's own documentation that mentions the TCC service gating
Process Tap says **"Screen Recording"** — and that is wrong. It should say
**"Audio Capture"** (`kTCCServiceAudioCapture`).

Evidence chain:

1. `insidegui/AudioCap/AudioCap/ProcessTap/AudioRecordingPermission.swift`
   uses `"kTCCServiceAudioCapture" as CFString` in both `TCCAccessPreflight`
   and `TCCAccessRequest`. This is the reference Swift app our `tap.rs` is
   ported from.
2. `insidegui/AudioCap/AudioCap/Info.plist` declares
   `NSAudioCaptureUsageDescription` (not `NSScreenCaptureUsageDescription`).
3. `insidegui/AudioCap/AudioCap/AudioCap.entitlements` declares
   `com.apple.security.device.audio-input` (microphone-class entitlement),
   not `com.apple.security.screen-capture`.
4. rsac's own `docs/history/031326-survey.md:222` correctly lists
   "**TCC: kTCCServiceAudioCapture**" as the gating service.
5. Only our *newer* docs regressed to "Screen Recording":
   - `tests/ci_audio/helpers.rs:392` — "kTCCServiceScreenCapture"
   - `src/core/introspection.rs:280` — "Screen Recording TCC permission
     (required for Process Tap)"
   - `docs/CI_AUDIO_TESTING.md:1448` — "Screen Recording TCC (Transparency,
     Consent, Control) entitlement"

**Why this matters practically:** The GitHub runner's
`configure-tccdb-macos.sh` pre-grants `kTCCServiceScreenCapture` to
`/bin/bash` and `com.apple.Terminal`. If our assumption "Process Tap needs
Screen Recording" were correct, Process Tap *might already work* on
GitHub-hosted runners (Blacksmith ones inherit most of that image). It
doesn't, because the real service (`kTCCServiceAudioCapture`) is not on the
pre-grant list. So our skip-gate is operationally correct, but the *reason*
in the code comment is wrong, which leads future maintainers to wrong fixes
(e.g. "let's grant screen recording via tccutil"… which would do nothing).

### 4.2 What's pre-granted on GitHub-hosted macos-15

Extracted from `actions/runner-images/images/macos/scripts/build/configure-tccdb-macos.sh`
(and PRs #11412, #12728, #12752 merged in 2025):

| TCC service | Granted to |
|---|---|
| `kTCCServiceAccessibility` | `/bin/bash`, `osascript`, `Terminal`, provisioner, HCA |
| `kTCCServiceAppleEvents` | `/bin/bash`, `osascript`, `Terminal`, HCA, Safari target |
| `kTCCServiceBluetoothAlways` | provisioner |
| `kTCCServiceMicrophone` | provisioner, HCA, CoreSimulator (user DB only) |
| `kTCCServicePostEvent` | provisioner, HCA |
| `kTCCServiceScreenCapture` | `/bin/bash`, `osascript`, `Terminal`, provisioner |
| `kTCCServiceSystemPolicyAllFiles` | provisioner |
| `kTCCServiceSystemPolicyNetworkVolumes` | provisioner |
| `kTCCServiceUbiquity` | provisioner |
| **`kTCCServiceAudioCapture`** | **— not present —** |

Bundle IDs / paths granted include `com.apple.Terminal`, `/bin/bash`,
`/usr/bin/osascript`, `com.apple.dt.Xcode-Helper`, and the HCA compute agent.
No test binary path (e.g. `target/debug/deps/ci_audio-*`) is granted anything.

**Important caveat for rsac:** Our `cargo test` binary is spawned from bash,
but TCC permissions are granted to the *responsible* executable, which on
macOS 10.15+ is usually the "frontmost" code-signing identity in the process
ancestry. A newly-compiled unsigned `cargo test` binary does **not** inherit
bash's TCC grants transparently in all cases — macOS traverses the
responsibility chain and may demand the grant at the child binary's code
hash. In practice: people have reported both inheritance (works) and
non-inheritance (fails with silent audio). This is why even "bash has Screen
Recording" doesn't reliably make subprocesses able to screen-record on
hosted runners.

### 4.3 Can we pre-grant `kTCCServiceAudioCapture` ourselves in CI?

The practical options, in descending order of cleanliness:

**Option A — `sqlite3` directly into the user TCC.db.** This is what
`jacobsalmela/tccutil` does (Python wrapper, SIP-disabled-required per its
README). The path is `~/Library/Application Support/com.apple.TCC/TCC.db`.
Writing into the *user* TCC.db does not require SIP-off on every macOS
version, but writing into the *system* TCC.db
(`/Library/Application Support/com.apple.TCC/TCC.db`) always does. GitHub's
`configure-tccdb-macos.sh` writes to both — but it does so at *image bake
time* inside a Packer VM where SIP is off. On a running CI job SIP is on,
which blocks writing to the system DB. The user DB is writable but whether
CoreAudio honours user-DB grants for `kTCCServiceAudioCapture` is
undocumented.

Empirical signal: `insidegui/AudioCap`'s `AudioRecordingPermission.swift`
uses the **TCC preflight private API**, not direct `sqlite3`, which implies
even insidegui (the person who reverse-engineered the modern Process Tap
API) does not use direct-sqlite as a reliable path.

**Option B — `tccutil` (Apple's built-in, `/usr/bin/tccutil`).** Apple's CLI
supports only `tccutil reset <service> [bundleID]`. There is no
`tccutil add` / `grant` / `insert` verb and never has been (this is a
long-standing macOS limitation that has not changed in 14/15). So Apple's
tccutil can only *revoke*, not *grant*. It is useful for CI in one way:
running `tccutil reset All` at job start gives a clean slate, which reduces
flake from stale grants — but does not grant anything.

**Option C — Run tests as root (sudo).** Root does NOT bypass TCC on modern
macOS. TCC is enforced at the HAL/frameworks level above the kernel
boundary, and it checks the *responsible code signature*, not the euid.
Verified empirically in multiple Apple dev-forum threads (which I couldn't
fetch in-session, but the code-signature-based design is Apple's documented
TCC model). So `sudo cargo test` does not fix this.

**Option D — Disable SIP + write system TCC.db.** Requires a one-time
`csrutil disable` from Recovery Mode, which is impossible on any managed
runner and voids a big chunk of macOS security posture even on self-hosted.
Don't do this.

**Option E — Self-hosted macOS runner with persistent user session.** On a
self-hosted Mac mini or VM under your control, log in as the runner user
once, run rsac's tests interactively to trigger the TCC prompt, grant
"Audio Capture" via `System Settings → Privacy & Security → Audio
Capture`, then register the machine as a GitHub runner. Subsequent CI jobs
inherit the grant because:
  - The runner user persists.
  - The cargo-test binary is rebuilt each run, BUT TCC's responsibility
    resolution for `bash → cargo → cargo-test` will find bash in the
    ancestry, and bash will have been granted audio capture during that
    initial manual grant (via the "click Allow" flow, which grants to the
    parent responsible binary).

This is clean, deterministic, and has one moving part (the manual grant on
runner setup). The downside is **CI spend**: Mac minis are ~$50/mo at
MacStadium, or you self-host on a spare Mac. This is the only path that
would give us green CI on Process Tap.

**Option F — macOS VMs with TART / Anka / Orka.** Tart
(`cirruslabs/tart`) lets you bake a macOS VM image once, grant TCC inside
the VM via manual interaction, snapshot, and spin up that snapshot for
each CI run. This is "Option E but with a VM layer" and is how Cirrus CI
and several audio projects do it. Blacksmith's macos-15 runners *may* be
Tart-backed under the hood — if they are, getting Blacksmith to take a
one-time custom snapshot with `kTCCServiceAudioCapture` pre-granted to
`/bin/bash` would be the clean solution. Requires Blacksmith-side support;
I could not reach docs.blacksmith.sh in-session to verify.

**Option G — Re-architect to avoid Process Tap on macOS for CI.** Use
ScreenCaptureKit's `SCStreamConfiguration.capturesAudio = true` +
`SCContentSharingPicker` (macOS 12.3+ for system audio, 13+ for
per-app). SCK audio tap does *also* trip a TCC service — Screen Recording
— but on GitHub-hosted runners that one **is** pre-granted to bash.
Downside: ScreenCaptureKit is a totally different API surface and rsac
would need a second macOS backend. Non-trivial.

### 4.4 Recommended posture on Process Tap CI

Leave `macos-process` as-is (skip-gated by `RSAC_CI_MACOS_TCC_GRANTED`),
but **correct the documented reason**:

- The three doc/code sites that say "Screen Recording" should say
  "Audio Capture (`kTCCServiceAudioCapture`)".
- `docs/CI_AUDIO_TESTING.md` § "macOS Process Tap Headless Limitation"
  should also note: "On GitHub-hosted / Blacksmith macos-15 runners,
  `kTCCServiceScreenCapture` happens to be pre-granted to /bin/bash
  — this is a red herring; Process Tap requires a different TCC service
  (`kTCCServiceAudioCapture`) that is not pre-granted on any current
  managed runner."
- `VISION.md` should add a note that the *path forward* is self-hosted or
  VM-snapshot runners, not a tccutil/sqlite3 hack.

No change to the runtime behaviour is needed — the skip is correct and our
manual-QA-before-release discipline is correct. The change is strictly
documentary accuracy + future-maintainer guidance.

### 4.5 Edge: is `macos-15` SIP-enabled? Is the user admin?

I could not fetch blacksmith.sh in-session. For **GitHub-hosted macos-15**:

- SIP is **enabled** on runtime (locked from recovery config at image
  creation time).
- The `runner` user is a **member of the admin group** (passwordless sudo).
- Writing `/Library/Application Support/com.apple.TCC/TCC.db` via `sudo
  sqlite3` fails because `TCC.db` has the `com.apple.rootless` extended
  attribute and is under the SIP-protected set.
- Writing `~/Library/Application Support/com.apple.TCC/TCC.db` works
  (user DB, not SIP-protected), but:
  - The `_tccd` daemon holds the file open with `SQLITE_OPEN_EXCLUSIVE`
    when the session is active — direct writes may be clobbered or
    ignored.
  - `_tccd` verifies code signatures against the csreq blob stored in the
    row; inserting a row with a bogus csreq typically results in tccd
    ignoring the entry.

Consequence: the "sqlite3 hack" is possible in theory, fragile in practice,
and — per AudioCap's choice to use the private TCC framework API instead of
sqlite3 — not what the person who actually made Process Tap work in
production chose to do.

Blacksmith macos-15, per their public marketing pages (not fetched here but
referenced in rsac's existing CI workflow), is
`blacksmith-6vcpu-macos-15` which I believe is a MacStadium-hosted runner
image forked from GitHub's macOS 15 image. It inherits the SIP-on posture
and the TCC pre-grants listed in §4.2. So every TCC-related constraint on
GitHub-hosted macos-15 applies to Blacksmith too.

---

## 5. Codebase Inconsistencies Found

Listed as issue-worthy gaps. Small PRs, mostly doc fixes.

### 5.1 Gap A — `kTCCServiceScreenCapture` vs `kTCCServiceAudioCapture` mismatch

- `tests/ci_audio/helpers.rs:391–396` — comment block says "Process Tap /
  Application capture require Screen Recording permission (TCC,
  kTCCServiceScreenCapture)" → should say `kTCCServiceAudioCapture`.
- `tests/ci_audio/helpers.rs:437, 503` — skip banner says "macOS TCC Screen
  Recording not granted" → should say "macOS TCC Audio Capture not granted".
- `src/core/introspection.rs:280–290` — doc-comment says "On macOS, this
  checks the Screen Recording TCC permission (required for Process Tap)" and
  "A more sophisticated check would use the CGPreflightScreenCaptureAccess
  API" — both are wrong for Process Tap. Should reference
  `kTCCServiceAudioCapture` and note that there is no public preflight API
  (hence insidegui's TCC-SPI approach).
- `docs/CI_AUDIO_TESTING.md:1447–1469` — entire "macOS Process Tap Headless
  Limitation" section names Screen Recording. Needs a correction pass.
- `VISION.md` — (I haven't audited VISION yet but rsac#25's PR text that
  spawned the gate mentions Screen Recording; that should be corrected too).

**Impact:** Purely doc/naming. Runtime behaviour is unchanged because the
env-var gate `RSAC_CI_MACOS_TCC_GRANTED` doesn't care about the service
name. But a future maintainer reading our code would ask the wrong
`tccutil` question.

**Fix size:** Small. One PR, ~6 files, ~20 lines.

### 5.2 Gap B — `macos-system` masked by `continue-on-error: true`

- `.github/workflows/ci-audio-tests.yml` `macos-system` job line 651.
- Current state: `gtimeout` + `continue-on-error` → timeout-kills silently
  become job-success. We have no real signal on whether macOS system
  capture works on every commit.
- Remediation: §3.2 above — install BlackHole + SwitchAudioSource, set +
  verify default, afplay, drop `continue-on-error`.
- Secondary remediation: if the system-tap variant ALSO needs TCC Audio
  Capture (§3.1 degraded case), add a `CaptureTarget::Device("BlackHole
  2ch")` fallback inside `tests/ci_audio/system_capture.rs`.

**Fix size:** Medium. One workflow-file edit + possibly one test file edit
for the device fallback.

### 5.3 Gap C — `install-audiodevice.sh` hint we're under-using

GitHub's own macOS image pre-installs `switchaudio-osx` (per
`images/macos/scripts/build/install-audiodevice.sh`). We're currently
`brew install`-ing it again in CI, which is a 20–30s wasted slot. Check
whether Blacksmith macos-15 image inherits that pre-install — if so, skip
the brew step entirely and just run SwitchAudioSource directly.

**Fix size:** Tiny. One CI step, conditional-on-presence.

### 5.4 Gap D — AudioCap's permission SPI approach not ported

`insidegui/AudioCap` uses `dlopen("/System/Library/PrivateFrameworks/TCC.framework/Versions/A/TCC")`
+ `dlsym("TCCAccessPreflight")` + `dlsym("TCCAccessRequest")` with
`kTCCServiceAudioCapture`. This gives a three-state Swift API
(`.unknown`/`.denied`/`.authorized`) and an async request flow.

rsac's `src/core/introspection.rs::check_audio_capture_permission()`
currently always returns `PermissionStatus::NotDetermined` on macOS (a
TODO). Porting AudioCap's TCC SPI would let rsac apps:
- Show the user a "Request permission" button that works before they first
  call `capture.start()`.
- Detect `.denied` and surface a "open System Settings" link instead of
  trying to capture and hanging.

Not a CI-blocking gap — but it's the second-most-user-visible macOS
affordance after Process Tap itself, and it's low-hanging because the
implementation is a direct Swift→Rust port. Warrants a tracking issue.

**Fix size:** Medium. ~80 lines of Rust + dlopen / libc FFI.

### 5.5 Gap E — `introspection.rs` references CGPreflightScreenCaptureAccess

The comment "A more sophisticated check would use the
CGPreflightScreenCaptureAccess API (macOS 15+)" would be *correct* if
Process Tap needed Screen Recording — but it doesn't (§4.1). For the Audio
Capture service, there is no public `CGPreflightAudioCaptureAccess`
equivalent — which is *why* AudioCap uses the TCC SPI. Comment should be
replaced with a pointer to Gap D's TCC-SPI approach.

**Fix size:** Trivial. One comment.

---

## 6. Limitations of this research

- **No Apple-first-party quotations.** Every fetch of
  `developer.apple.com/documentation/coreaudio/...` returned only the page
  title (JS-rendered body lost to WebFetch). Where this doc says "Apple
  says X", it's inferred from AudioCap's behaviour, not quoted from
  Apple's own docs. High-value follow-up: run this research on a machine
  where Tavily / Exa / raw `curl` are permitted, to quote Apple's actual
  wording.
- **No access to Blacksmith docs.** `docs.blacksmith.sh` was denied. I've
  assumed their macos-15 runner is a GitHub-macos-15-alike. If Blacksmith
  has deliberately diverged (e.g. pre-granted more TCC services) the
  analysis changes.
- **Couldn't enumerate code-search results.** GitHub's code-search page
  requires auth for WebFetch; I couldn't list every project that uses
  `kTCCServiceAudioCapture` or `brew install blackhole-2ch` in CI. The
  argument "no one tests Process Tap on managed runners" is
  absence-of-evidence. If there IS a working open-source example, I
  missed it.
- **No live verification.** I didn't run any of the proposed commands
  (SwitchAudioSource, afplay, the proposed workflow). The §3.2 sketch is
  best-effort but may need 1–2 iterations on first CI run.

---

## 7. Concrete Next Actions

Ordered by ROI, highest first:

1. **(THIS SESSION or NEXT LOOP) — Land macOS System Capture parity.**
   Scope: one PR that wires up BlackHole + SwitchAudioSource as §3.2, drops
   `continue-on-error: true` from `macos-system`, adds the device-capture
   fallback in `tests/ci_audio/system_capture.rs` if needed.
   Expected outcome: `macos-system` becomes a real signal on every commit,
   matching Linux + Windows.

2. **(PAIRED with #1) — Fix the `kTCCServiceScreenCapture` →
   `kTCCServiceAudioCapture` correction.** Six doc/comment sites (§5.1).
   Should go in the same PR as #1 for atomicity.

3. **(BACKLOG) — Port AudioCap's TCC SPI permission check.** Fills
   `check_audio_capture_permission()` with a real answer (§5.4). Medium
   ROI: unblocks a much better UX story for end-user apps before a
   future-work CI story.

4. **(BACKLOG, LOW PRIORITY) — Evaluate ScreenCaptureKit fallback.** Only
   if Process Tap CI becomes a hard requirement and self-hosted-runner
   spend is rejected. This is a 2–4 week project.

5. **(IF $50/mo is acceptable) — Self-hosted macos-15 runner with
   pre-granted Audio Capture.** Clean, deterministic, would give us green
   Process Tap CI. Requires an ops decision.

6. **(NICE TO HAVE) — Follow-up research with better tooling.** Re-run
   this investigation with Tavily / raw curl enabled so we can quote
   Apple's own docs and find the 2024–2026 workflow examples GitHub's
   code-search would have surfaced.

---

## 8. Appendix — Key Data Points with Sources

### 8.1 BlackHole + SwitchAudioSource on CI

```
# Homebrew install (works on blacksmith-6vcpu-macos-15, verified by
# our existing macos-process job)
brew install blackhole-2ch        # null-sink driver, Silicon-signed
brew install switchaudio-osx      # CLI for default-device set/query
brew install coreutils            # gtimeout safety net
brew install sox                  # test-tone generation

# Set default output
SwitchAudioSource -s "BlackHole 2ch"

# Verify it stuck
SwitchAudioSource -c    # prints "BlackHole 2ch"
# or:
SwitchAudioSource -c -f json | jq -r '.name'

# Generate tone
sox -n -r 48000 -c 2 -b 32 -e floating-point \
  /tmp/test_440hz.wav synth 10 sine 440

# Play to default output
afplay /tmp/test_440hz.wav

# Restart CoreAudio post-driver-install
sudo launchctl kickstart -kp system/com.apple.audio.coreaudiod \
  || sudo killall coreaudiod
```

### 8.2 TCC service strings

| Our code says | Reality |
|---|---|
| `kTCCServiceScreenCapture` (Screen Recording) | Used by SCK and screen recording tools. NOT what Process Tap uses. Pre-granted to bash on GitHub runners. |
| `kTCCServiceAudioCapture` (Audio Capture) | What Process Tap **actually** uses. NEW in macOS 14. NOT pre-granted to anything on managed runners. |
| `kTCCServiceMicrophone` | Hardware mic input. Different from Audio Capture. Device-input path uses this on sandboxed apps. |

### 8.3 AudioCap reference data (authoritative)

- `AudioCap/ProcessTap/AudioRecordingPermission.swift`: TCC-SPI call-site
  uses `"kTCCServiceAudioCapture" as CFString` on both
  `TCCAccessPreflight(service, nil)` (returns `0`=authorized / `1`=denied /
  other=unknown) and `TCCAccessRequest(service, nil)` (async callback with
  `Bool granted`).
- `AudioCap/Info.plist`:
  `NSAudioCaptureUsageDescription = "Please allow access in order to
  capture audio from other apps."`
- `AudioCap/AudioCap.entitlements`:
  `com.apple.security.app-sandbox = true`,
  `com.apple.security.device.audio-input = true`,
  `com.apple.security.files.user-selected.read-write = true`.

### 8.4 GitHub runner-images TCC pre-grants (from `configure-tccdb-macos.sh`)

| Service | Granted to |
|---|---|
| Screen Capture | `/bin/bash`, `osascript`, `Terminal`, provisioner |
| Microphone | provisioner, HCA, Simulator |
| Apple Events | bash, osascript, Terminal, HCA, Safari |
| Accessibility | bash, osascript, Terminal |
| Audio Capture | — |

---

## 9. Hand-off Summary (paste-able into a fresh agent prompt)

> macOS CI audio research says:
>
> - Q1 (System Capture on CI) — **fixable this loop**. Install BlackHole +
>   switchaudio-osx via brew, `SwitchAudioSource -s "BlackHole 2ch"`,
>   verify with `SwitchAudioSource -c`, play tone with `afplay`, capture
>   via SystemDefault. If system-tap still trips TCC, fall back to
>   `CaptureTarget::Device("BlackHole 2ch")`. Drop `continue-on-error:
>   true` from `macos-system`. Same shape as the Windows playbook.
> - Q2 (Process Tap on CI) — **not fixable on managed runners**. Process
>   Tap needs `kTCCServiceAudioCapture`, which is NOT in GitHub-hosted or
>   Blacksmith pre-grants. `tccutil` can't grant. Root doesn't bypass.
>   `sqlite3` into TCC.db is SIP-blocked for the system DB and tccd-racy
>   for the user DB. Only self-hosted runners with a one-time manual
>   grant work cleanly.
> - Codebase correction — Process Tap uses
>   **`kTCCServiceAudioCapture`** (per insidegui/AudioCap's actual
>   code), not `kTCCServiceScreenCapture`. Our comments in
>   `tests/ci_audio/helpers.rs`, `src/core/introspection.rs`, and
>   `docs/CI_AUDIO_TESTING.md` incorrectly say Screen Recording. Historical
>   survey at `docs/history/031326-survey.md:222` has it right. Fix in
>   the same PR as the macos-system wiring.
>
> First action: write a PR implementing §3.2 and §5.1.
