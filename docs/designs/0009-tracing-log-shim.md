# ADR 0009 — `tracing`/`log` dual-backend instrumentation shim with an RT-path prohibition

**Status:** Accepted
**Date:** 2026-05-30
**Scope:** `src/trace.rs` (`rsac_event!`, `rsac_span!`, `install_default_tracing`,
`__rsac_render_fields!`), `Cargo.toml` (`tracing` feature, `log`/`tracing` deps),
`src/lib.rs` (module declaration + feature-gated re-export)
**Verdict:** rsac instruments only **non-real-time** lifecycle points behind a
single pair of macros — `rsac_event!` / `rsac_span!` — that emit `tracing`
events/spans when the optional `tracing` feature is on and fall back to the
always-present `log::` facade when it is off (a span degrades to an event). The
optional dependency surface is exactly the `tracing` **facade** — no
`tracing-subscriber`. These macros are **prohibited on the OS audio callback /
sample-push path**.

## 1. Context

rsac's #1 documented rule is that the OS audio callback thread must never
allocate, lock, or block (see
[ADR-0001](0001-rt-allocation-guarantee.md)). At the same time, a capture
library has genuine observability needs on its **control plane** — build, start,
stop, drop, and backpressure transitions are exactly the moments a consumer wants
structured, filterable telemetry for.

Two pressures had to be reconciled:

1. **Zero-extra-dependency default.** The crate already depends on the `log`
   facade unconditionally (`Cargo.toml` — `log = "0.4.29"`), and earlier audit
   work replaced ad-hoc `eprintln!` diagnostics with `log::`. Adding a hard
   dependency on `tracing` (and worse, a subscriber) for every downstream would
   be a regression for embedders that only want `log`.
2. **Structured spans for those who opt in.** Modern consumers increasingly run a
   `tracing` subscriber and want rsac's lifecycle events to participate in their
   span tree with structured `key = value` fields, not flat strings.

A naïve solution — sprinkle `tracing::` calls behind `#[cfg(feature = "tracing")]`
at every call site, with a `log::` arm next to it — would double every
instrumentation site, drift between the two arms, and make it trivially easy to
emit a record on the RT path by accident. The decision below provides exactly
**one** instrumentation call site per event and makes the RT-path prohibition an
explicit, documented contract.

## 2. Decision drivers

- **One call site, two backends.** The choice of backend must be invisible at the
  call site so the two arms cannot drift and instrumenting is cheap to add.
- **`log` default stays free.** With the feature off the behavior must be
  identical to the pre-existing `log::` logging and pull in **no** extra
  dependency.
- **Opt-in `tracing`, facade only.** Turning the feature on must give real
  `tracing` events/spans without forcing a subscriber choice on the embedder —
  selecting and configuring a subscriber is the application's job, not the
  library's.
- **RT-safety is non-negotiable.** Emitting a `tracing`/`log` record can allocate,
  take a lock, or block in the installed subscriber/logger; that is categorically
  forbidden on the audio callback / sample-push path.
- **No `unsafe`, no panics, additive.** The shim is a pure macro/dependency
  addition; default behavior is unchanged.

## 3. Considered options

### Option A — Hard-depend on `tracing` everywhere; drop `log`

Make `tracing` a non-optional dependency and route all instrumentation through it.
- ➕ Single backend, no macro gymnastics, structured spans always available.
- ➖ Forces a new mandatory dependency (and the question of a subscriber) on every
  downstream, including those that only ever wanted `log`. Regresses the
  zero-extra-dependency default and the existing `log`-based diagnostics. Rejected.

### Option B — Two parallel call sites guarded by `#[cfg(feature = "tracing")]`

At each instrumentation point, write a `tracing::event!` arm and a `log::` arm.
- ➕ No macro machinery.
- ➖ Doubles every call site; the two arms drift; nothing structurally prevents an
  instrumentation call from landing on the RT path. High maintenance, low safety.
  Rejected.

### Option C — Dual-backend macro shim (`rsac_event!` / `rsac_span!`), facade-only `tracing`, RT-path prohibition (CHOSEN)

One pair of `#[macro_export]`ed macros. With `--features tracing` they expand to
`tracing::event!` / `tracing::span!`; without it they expand to the matching
`log::<level>!` (a span has no `log` analogue, so it **degrades to an event** and
yields `()`). The optional dependency is exactly the `tracing` facade — **no
`tracing-subscriber`**. The macros are documented as control-plane-only and are
forbidden on `audio/*/thread.rs` and the `bridge::ring_buffer` producer.
- ➕ Exactly one call site per event under both configs; the backends cannot
  drift; `log` default is byte-for-byte the prior behavior with no new dep;
  structured spans for opt-in users; the prohibition is a stated contract.
- ➖ Non-trivial macro: the `log` fallback is a tt-muncher that peels
  `field = value` pairs (after an `expr`, `,` is in `FOLLOW(expr)`, so a separated
  repetition is locally ambiguous and must be munched one pair at a time). The
  level token is resolved in the public arms because rust-analyzer/rustc do not
  eagerly expand a normalizer macro in argument position.

## 4. Decision

**Option C.** Implement the shim in `src/trace.rs`:

- **`rsac_event!(level, [field = value,]* [fmt, args..])`** — one public arm per
  level token (`error`/`warn`/`info`/`debug`/`trace`). Each arm resolves the level
  to a concrete `tracing::Level` ident (`ERROR`/…) **and** the matching `log::`
  macro ident, then dispatches:
  - with `tracing` on → `tracing::event!(Level::$lvl, …)`;
  - with `tracing` off → the `log::<level>!` tt-muncher, which renders structured
    fields as `key=value` (Debug-formatted via `__rsac_render_fields!`) prefixed
    to the message so **no field information is dropped** in the fallback.
- **`rsac_span!(level, name, [field = value,]*)`** — with `tracing` on expands to
  `tracing::span!`, yielding a `tracing::Span` the caller can `.enter()` /
  `.in_scope(..)`; with `tracing` off it **degrades to an `rsac_event!`** (spans
  have no `log` analogue) and yields the unit value `()`, so a create-and-hold
  call site (`let _span = rsac_span!(...)`) compiles and runs identically under
  both configs.
- **Portable field syntax only.** Call sites must use the `field = value` form;
  `tracing`'s `%`/`?` field sigils are **forbidden** because they have no `log`
  fallback and would only compile with the feature on.
- **Facade-only dependency.** The `tracing` feature pulls in `dep:tracing`
  (`Cargo.toml` — `tracing = { version = "0.1.41", optional = true }`) and
  nothing else. `__rsac_tracing` (the `event!`/`span!`/`Level` re-export) is
  `#[cfg(feature = "tracing")]`; `__rsac_log` (the `log::` re-export) is
  unconditional so the fallback always resolves.
- **`install_default_tracing()`** (feature-gated, re-exported at the crate root)
  is a best-effort, idempotent convenience for binaries/examples. It installs
  `tracing`'s built-in **`NoSubscriber`** as the global default — enough to make
  the global dispatcher non-default (so events are dispatched rather than
  discarded by the `NoSubscriber` fast path) while leaving formatting/filtering to
  whatever subscriber the embedding application installs. **Library code never
  calls it**; choosing/configuring a subscriber is the application's
  responsibility, which is precisely why rsac does **not** depend on
  `tracing-subscriber`.

**RT-path prohibition (contract).** `rsac_event!` / `rsac_span!` are for the
**control plane only** — build, start, stop, drop, backpressure transitions. They
**must never** appear on the OS audio callback / sample-push path
(`audio/*/thread.rs`, the `bridge::ring_buffer` producer / `push_samples_or_drop`
family). Emitting a record can allocate, lock, or block inside the installed
subscriber or logger, violating the ADR-0001 real-time guarantee. The only
allocation the shim itself performs is the single small `String` built by
`__rsac_render_fields!` in the `log` fallback — explicitly a non-RT control-plane
cost, never on the audio path.

## 5. Consequences

- A new **opt-in** `tracing` feature, **off by default**. With it off, behavior is
  identical to the prior `log::`-based instrumentation and **no** new dependency is
  added; with it on, the only added dependency is the `tracing` facade.
- Instrumentation has exactly one call site per event; the two backends cannot
  drift because the call site never names a backend directly.
- Spans are real under `tracing` and silently degrade to events under `log`, so
  call sites are uniform across configs.
- The RT-path prohibition is now a **documented contract** rather than an
  implicit assumption; a reviewer can reject any `rsac_event!`/`rsac_span!` that
  lands in `audio/*/thread.rs` or the ring-buffer producer on sight. At the time
  of writing the shim is wired and unit-tested in `src/trace.rs` but not yet
  attached to the control-plane lifecycle call sites; that wiring is additive and
  bounded by the same prohibition.
- Because the shim depends only on the facade and defaults to `NoSubscriber`,
  rsac imposes no subscriber/logger policy on downstreams — they keep full control
  of formatting, filtering, and routing.
- The portable-`field = value`-only rule is a small ergonomic constraint on call
  sites in exchange for backend-agnostic, drift-free instrumentation.

## 6. References

- Audit critique gap **"tracing/log dual-backend fallback + RT-path prohibition"**
  and **`### ADR review`** GAP list (2026-05-30 deep-dive,
  `docs/reviews/rsac-architecture-critique-2026-05-30.md`).
- [ADR-0001 — RT-allocation guarantee](0001-rt-allocation-guarantee.md) (the
  guarantee these macros must never violate).
- Related decisions recorded in the same 2026-05-30 wave of ADRs (ADR-0004
  through ADR-0009 in `docs/designs/`): the device-change notifications &
  threading model ([0004](0004-device-change-notifications.md)), the
  DeviceWatcher RAII teardown contract ([0005](0005-device-watcher-raii-teardown.md)),
  the [bridge-zerocopy `SampleRing` data plane](0006-bridge-zerocopy-samplering.md),
  period-derived ring sizing & `buffer_size` semantics
  ([0007](0007-capacity-period-sizing.md)), and the CachePadded false-sharing
  mitigation ([0008](0008-cache-padded-atomics.md)). (See each file in
  `docs/designs/` for its own grounding.)
- `src/trace.rs` — `rsac_event!`, `rsac_span!`, `__rsac_render_fields!`,
  `install_default_tracing`, `tracing_subscriber_fallback` (`NoSubscriber`).
- `Cargo.toml` — `tracing` feature (`["dep:tracing"]`), `log = "0.4.29"`
  (unconditional), `tracing = { version = "0.1.41", optional = true }`.
- `src/lib.rs` — `pub mod trace;` declared before its users; feature-gated
  `pub use crate::trace::install_default_tracing;`.
