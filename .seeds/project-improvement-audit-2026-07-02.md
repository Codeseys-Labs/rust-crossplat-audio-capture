# Project Improvement Audit Seeds - 2026-07-02

Purpose: concrete follow-up findings from a read-heavy audit of `work/critique-ci-audio-integration`. These are ready for depth-2 subagents. Do not push. Preserve unrelated dirty integration-branch changes.

Global delegation constraints for every prompt:

```text
Current depth: 2. Maximum delegation depth: 5. Further nesting must not exceed depth 5. Prefer read-only investigation first. Do not push. Avoid touching unrelated files. If editing, keep changes small, validate, and commit only if the worktree state is safe; otherwise update this seed.
```

## P0 - Binding Capability Parity Gap

Finding: Rust's public `PlatformCapabilities` surface has outgrown the C/Go/Python/Node projections. Rust exposes `supports_device_change_notifications`, `supported_sample_formats`, `sample_rate_range`, `max_channels`, `backend_name`, and the canonical builder whitelist via `SUPPORTED_SAMPLE_RATES` / `supported_sample_rates()`. Most bindings expose only the older subset, and the C ABI prevents Go from adding the missing fields without new exported accessors.

Evidence:

- `src/core/capabilities.rs:81` exposes `supports_device_change_notifications`.
- `src/core/capabilities.rs:83` exposes `supported_sample_formats`.
- `src/core/capabilities.rs:106` exposes `SUPPORTED_SAMPLE_RATES`; `src/core/capabilities.rs:113` exposes `supported_sample_rates()`.
- `bindings/rsac-ffi/src/lib.rs:1426` through `bindings/rsac-ffi/src/lib.rs:1511` only export system/app/process-tree/device-selection/max-channels/backend-name accessors.
- `bindings/rsac-ffi/include/rsac.h:431` through `bindings/rsac-ffi/include/rsac.h:460` mirrors that limited C ABI.
- `bindings/rsac-go/rsac.go:1252` through `bindings/rsac-go/rsac.go:1266` has no device-change, supported-format, or sample-rate whitelist fields.
- `bindings/rsac-python/src/lib.rs:530` through `bindings/rsac-python/src/lib.rs:539` documents only the older capability fields.
- `bindings/rsac-napi/src/lib.rs:1069` through `bindings/rsac-napi/src/lib.rs:1101` exposes min/max sample-rate range but not device-change notifications, supported sample formats, or the builder whitelist.

Impact: downstream UIs and bindings cannot make the same readiness decisions Rust users can. In particular, a non-Rust consumer cannot tell whether `DeviceEnumerator::watch` is implemented, cannot populate an exact builder-compatible sample-rate picker, and cannot present supported sample formats consistently.

Recommended follow-up:

- Add additive C ABI accessors for device-change notifications, sample-rate whitelist count/value, and supported sample-format count/value.
- Project the new accessors into Go and update `bindings/rsac-go/rsac.h` in lockstep with `bindings/rsac-ffi/include/rsac.h` and generated `rsac_generated.h`.
- Add direct Python and Node fields from Rust where no C ABI is involved.
- Add binding-level tests that assert the projected fields match `rsac::PlatformCapabilities::query()` on the host.

Ready prompt:

```text
Current depth: 2. Maximum delegation depth: 5. Further nesting must not exceed depth 5. Do not push. Preserve unrelated dirty changes.

Close rsac binding capability parity. Start read-only: compare `src/core/capabilities.rs` against C FFI, Go, Python, Node, and docs. Then implement only additive fields/accessors for `supports_device_change_notifications`, supported sample formats, and the canonical supported sample-rate whitelist if safe. Keep C header changes in lockstep across `bindings/rsac-ffi/include/rsac.h`, generated `rsac_generated.h`, and `bindings/rsac-go/rsac.h`. Validate with `cargo check -p rsac-ffi`, `cargo check -p rsac-napi`, the narrow Python/Go checks available on the host, and the header symbol drift check if present. If implementation is blocked, update this seed with exact blockers.
```

## P1 - Node Binding Feature Selection Still Uses Root Defaults

Finding: `rsac-napi` still depends on the root crate with implicit default features, while `docs/features.md` defines the canonical binding convention as per-target `rsac` dependencies with `default-features = false` and exactly one platform backend enabled.

Evidence:

- `bindings/rsac-napi/Cargo.toml:13` has `rsac = { path = "../.." }`.
- `docs/features.md:91` through `docs/features.md:153` defines the canonical binding feature-resolution convention.
- `docs/features.md:161` correctly still marks `rsac-napi` as `migrating` and says it should adopt per-target blocks.
- `bindings/rsac-python/Cargo.toml:30` through `bindings/rsac-python/Cargo.toml:43` is the working reference pattern.

Impact: the Node binding does not follow the project's own packaging contract. Even where target-specific dependency cfgs prevent wrong-OS native crates from linking, the binding enables the root all-backends meta-feature and can drift from the tested per-binding feature policy.

Recommended follow-up:

- Convert `bindings/rsac-napi/Cargo.toml` to the same per-target dependency pattern used by Python.
- If needed, add binding-local passthrough features only for features Node actually exposes.
- Update `docs/features.md:155` through `docs/features.md:165` once the manifest conforms.

Ready prompt:

```text
Current depth: 2. Maximum delegation depth: 5. Further nesting must not exceed depth 5. Do not push. Preserve unrelated dirty changes.

Migrate `bindings/rsac-napi/Cargo.toml` to the canonical per-target backend dependency convention documented in `docs/features.md`, using `bindings/rsac-python/Cargo.toml` as the reference. Keep the change manifest-only unless docs need the status line updated. Validate at minimum with `cargo check -p rsac-napi`; if on Windows, also run `cargo check -p rsac-napi --no-default-features` only if the package defines such features after the change. Do not broaden feature surfaces unnecessarily.
```

## P1 - Release And Feature Docs Have Stale Version-Specific Claims

Finding: release/feature docs still carry stale 0.2-era statements even though manifests are at 0.4.0 and prior release-pipeline fixes have landed.

Evidence:

- `docs/features.md:190` says the matrix reflects the `0.2.0` release line.
- `docs/RELEASE_PROCESS.md:425` through `docs/RELEASE_PROCESS.md:429` says `rsac-ffi` trails at `0.1.0` while others are `0.2.0`; current manifests are `0.4.0`.
- `docs/RELEASE_PROCESS.md:431` through `docs/RELEASE_PROCESS.md:437` refers to the generated `rsac.h` header, while the actual generated twin is `bindings/rsac-ffi/include/rsac_generated.h` and curated consumer header is `bindings/rsac-ffi/include/rsac.h`.
- `scripts/bump-version.sh:3` through `scripts/bump-version.sh:9` and `.github/workflows/ci.yml:720` onward now reflect six-manifest lockstep, so the old warning is misleading.

Impact: release operators can distrust otherwise-correct automation, and C ABI guidance blurs the curated/generated header split that the CI header-drift check now relies on.

Recommended follow-up:

- Replace stale point-in-time statements with evergreen wording, or update them to 0.4.0 if they are intentionally historical.
- Clarify that `include/rsac.h` is curated and `include/rsac_generated.h` is the cbindgen output used for drift comparison.
- Keep 0.2.0 examples only where the section explicitly says they are examples.

Ready prompt:

```text
Current depth: 2. Maximum delegation depth: 5. Further nesting must not exceed depth 5. Do not push. Preserve unrelated dirty changes.

Refresh stale release/feature docs without changing code. Audit `docs/features.md`, `docs/RELEASE_PROCESS.md`, and nearby release docs for point-in-time 0.2.0 / rsac-ffi-0.1.0 claims that conflict with current 0.4.0 manifests and the curated-vs-generated C header policy. Make minimal wording updates only; keep worked examples as examples if clearly labeled. Validate with a grep for stale claims and, if available, markdown/link checks.
```

## P2 - Existing Seeds Already Cover Submodule, CI Matrix, And Platform Readiness Work

This audit did not duplicate already-filed seeds for these areas:

- `apps/audio-graph` is configured in `.gitmodules` but absent as a gitlink/path: see `.seeds/delegation-backlog-2026-07-02.md:104` through `.seeds/delegation-backlog-2026-07-02.md:121`.
- CI coverage, deterministic Linux PipeWire routing, Windows runner boundaries, and macOS TCC/self-hosted planning are already seeded in `.seeds/delegation-backlog-2026-07-02.md:29` through `.seeds/delegation-backlog-2026-07-02.md:102`.
- Deferred dirty worktree imports are already seeded in `.seeds/delegation-backlog-2026-07-02.md:123` through `.seeds/delegation-backlog-2026-07-02.md:140`.

Validation performed during this audit:

- `cargo check --no-default-features` passed on the current Windows host.
- `cargo check --no-default-features --features feat_windows` passed on the current Windows host.
- `cargo check -p rsac-napi` passed on the current Windows host.
- `git submodule status --recursive` did not list `apps/audio-graph`, confirming the stale `.gitmodules` entry is not represented by a committed gitlink in the current tree.

Worktree notes:

- No implementation files were intentionally edited by this audit.
- Existing dirty/untracked files were present or changed concurrently on the integration branch; preserve them unless a follow-up owns that scope.
