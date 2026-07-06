# rsac — iOS SwiftPM package (`RsacAudio` + `RsacBroadcastKit`)

> ## ⚠️ Status: source-complete; **not yet built in CI**
>
> These sources have never been compiled by this repository's CI — there is
> no Xcode on the machines that authored them. **Seed rsac-48e7 adds the
> SwiftPM CI job** (`swift build` / `xcodebuild` on a macOS runner) that
> turns "source-complete" into "builds". Until that job is green, treat
> every API-shape detail marked `// CI-VERIFY:` in the sources as
> unconfirmed. Design provenance: ADR-0012 (packaging), ADR-0013 (transport
> & CaptureTarget semantics), `docs/MOBILE_BACKEND_DESIGN.md` (iOS sections).

## What this package is

The Swift half of rsac's iOS backend (ADR-0012's "batteries included"
decision — rsac ships the consent-flow and extension glue so every consumer
doesn't reinvent it):

| Product | Purpose |
|---|---|
| `RsacAudio` | `AVAudioSession` helpers for the **mic path** — configure/activate a record-capable session, request the record permission (async + callback). rsac's Rust backend never touches the session; the host app owns it, with these helpers. |
| `RsacBroadcastKit` | The **system-audio path** glue: the canonical cross-process mmap SPSC ring (`RingLayout.swift`), the ring producer, and an open `RPBroadcastSampleHandler` template for the consumer app's Broadcast Upload Extension. |

Internal target `CRsacRingAtomics` (not a product) supplies C11 `stdatomic`
acquire/release u64 operations — Swift on iOS 14+ has no standard-library
atomics usable on raw cross-process mmap'd memory.

> ### ⚠️ `RingLayout.swift` is CANONICAL — version-gate any change
>
> The Rust consumer (`src/audio/ios/broadcast.rs`, seed **rsac-b3aa**)
> mirrors the ring layout in Rust, byte for byte: header offsets, cursor
> acquire/release ordering, the offset-0 magic+version publish point, the
> CLOCK_MONOTONIC heartbeat domain, the ring file name, and the Darwin
> notification strings. **Any change to `RingLayout.swift` is a
> cross-process ABI break:** bump `layoutVersion`, update the Rust mirror in
> lockstep, and make both sides reject a version mismatch.

## Embedding the broadcast extension (system audio, `CaptureTarget::SystemDefault`)

The mic path needs none of this. System capture needs all of it:

1. **Add a Broadcast Upload Extension target** to your app in Xcode
   (File → New → Target → Broadcast Upload Extension; uncheck "include UI
   extension" unless you want a picker UI).
2. **Depend on `RsacBroadcastKit`** from the extension target and replace
   the generated `SampleHandler.swift` with:

   ```swift
   import RsacBroadcastKit

   class SampleHandler: RsacBroadcastSampleHandler {
       override var appGroupIdentifier: String { "group.com.example.myapp.rsac" }
   }
   ```

   Keep `NSExtensionPrincipalClass` in the extension's `Info.plist` pointing
   at your subclass.
3. **App Group entitlement — on BOTH targets.** Add the *same* App Group
   (e.g. `group.com.example.myapp.rsac`) to the
   `com.apple.security.application-groups` entitlement of the **host app
   AND the extension** (Signing & Capabilities → + App Groups). The ring
   file lives in that shared container; if either side lacks the
   entitlement, `containerURL(forSecurityApplicationGroupIdentifier:)`
   returns nil and setup fails with an actionable error.
4. **Host side (Rust):** build with `CaptureTarget::SystemDefault` and pass
   the same App Group identifier to the rsac iOS backend (consumption
   surface lands with rsac-b3aa). The host drains the ring into a normal
   `BridgeProducer` — overrun/terminal semantics identical to desktop.
5. **Starting capture:** offer the user `RPSystemBroadcastPickerView` (you
   can pre-select your extension via `preferredExtension`), or let them use
   the Control Center screen-recording long-press. There is no third
   option — see UX constraints below.

## UX constraints (ADR-0013 — present these honestly to your users)

- **User-initiated only.** iOS provides **no API to start a broadcast
  programmatically**. The user must tap the picker or Control Center. Your
  UI must be a *prompt to start*, never a "capturing…" state you set
  yourself.
- **The user can stop it at any time** (Control Center, red status bar/pill).
  The host-side stream then ends with a **fatal terminal** (`finished`
  Darwin notification, or missed heartbeats if the extension was killed) —
  design your consumer for streams that end without your code asking.
- **Captures everything.** ReplayKit app-audio is the mixed output of the
  whole device — there is **no per-app filter on iOS, permanently**
  (`Application`/`ApplicationByName`/`ProcessTree` report
  `PlatformNotSupported`; Apple provides no API).
- **Extension memory cap (~50 MB).** The ring defaults to ~2 s of audio
  (768 KiB at 48 kHz stereo). Ring-full ⇒ buffers are **dropped and
  counted** (`producerDropCount`), never buffered unboundedly.
- **Mic:** the extension ignores `.audioMic`; microphone capture is the host
  app's `AVAudioEngine` path (`RsacAudio` helpers +
  `NSMicrophoneUsageDescription`).

## App Store review notes

- Broadcast Upload Extensions get **elevated review scrutiny**: state
  clearly in your review notes *why* your app captures system audio and
  what happens to the data (on-device processing vs upload).
- The system broadcast consent UI is the only capture trigger — do not
  attempt to auto-start, hide the system indicators, or imply capture is
  passive; these are rejection (and policy) territory.
- `NSMicrophoneUsageDescription` is required if you use the mic path — the
  string is user-facing; write a real one.
- Ship the extension **only if you use system capture** — an unused
  broadcast extension invites review questions.

## Layout

```
mobile/ios/
├── Package.swift                     # swift-tools 5.9, iOS 14+, 2 products
├── README.md                         # this file
└── Sources/
    ├── RsacAudio/
    │   └── AudioSessionHelper.swift  # session config/activation + permission
    ├── RsacBroadcastKit/
    │   ├── RingLayout.swift          # ⚠️ CANONICAL ring contract (v1)
    │   ├── RingProducer.swift        # mmap + publish + drop-not-block writes
    │   └── SampleHandlerTemplate.swift # open RPBroadcastSampleHandler
    └── CRsacRingAtomics/             # C11 stdatomic shim (internal target)
        ├── include/rsac_ring_atomics.h
        └── rsac_ring_atomics.c
```

## Related seeds

| Seed | What |
|---|---|
| rsac-6d5f | This package (SwiftPM glue sources) |
| rsac-b3aa | Rust consumer: `src/audio/ios/broadcast.rs` mirrors `RingLayout.swift` |
| rsac-48e7 | SwiftPM CI build job (macOS runner) — resolves every `// CI-VERIFY:` |
