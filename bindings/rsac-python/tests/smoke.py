"""Runtime smoke test for the rsac Python binding (rsac-e1f7).

Run after `maturin develop` (or any install of the binding):

    python bindings/rsac-python/tests/smoke.py

Asserts the extension module loads and the device-independent surface
works. Device enumeration is exercised but tolerated to fail on headless
machines (no audio stack), mirroring tests/ci_audio conventions.
"""

import sys

import rsac

failures = []

# ── module loads + capabilities shape ─────────────────────────────────
caps = rsac.platform_capabilities()
for field in (
    "supports_system_capture",
    "supports_application_capture",
    "supports_process_tree_capture",
    "supports_device_change_notifications",
    "requires_user_consent",
    "supported_sample_formats",
    "supported_sample_rates",
    "backend_name",
):
    if not hasattr(caps, field):
        failures.append(f"platform_capabilities() missing field: {field}")
print(f"capabilities: backend={caps.backend_name!r}")

# Desktop backends never require a config-time consent artifact
# (docs/MOBILE_BACKEND_DESIGN.md) — pin it so the projection can't drift.
if hasattr(caps, "requires_user_consent") and caps.requires_user_consent is not False:
    failures.append(
        f"requires_user_consent should be False on desktop, got {caps.requires_user_consent!r}"
    )

# ── CaptureTarget grammar round-trip ──────────────────────────────────
for spec in ("system", "app:1234", "name:Firefox", "tree:42"):
    t = rsac.CaptureTarget.parse(spec)
    if t is None:
        failures.append(f"CaptureTarget.parse({spec!r}) returned None")
print("CaptureTarget.parse round-trip: ok")

# Factory methods exist
rsac.CaptureTarget.system_default()
rsac.CaptureTarget.application_by_name("Firefox")
rsac.CaptureTarget.process_tree(1)

# Malformed spec must raise, not succeed silently
try:
    rsac.CaptureTarget.parse("not-a-scheme:whatever")
    failures.append("CaptureTarget.parse accepted a garbage scheme")
except Exception:
    print("garbage target spec rejected: ok")

# ── composition (device-free build + not-started contract) ────────────
# The composition classes are always present (the `compose` cargo feature is
# enabled unconditionally in the wheel). A "system"-source composition BUILDS
# without touching a device (build only validates); we do NOT start() it here
# because that needs a real device.
for cls in ("Group", "CompositionBuilder", "Composition", "CompositionStats", "SourceStats"):
    if not hasattr(rsac, cls):
        failures.append(f"rsac missing compose class: {cls}")

g = rsac.Group("main")
g.source("system")  # "system" always passes capture preflight on every platform
builder = rsac.CompositionBuilder(sample_rate=48000)
builder.add_group(g)
comp = builder.build()  # touches no devices → succeeds everywhere
if comp.channel_count != 0:
    failures.append(f"not-started comp.channel_count should be 0, got {comp.channel_count}")
if comp.stats() is not None:
    failures.append("not-started comp.stats() should be None")
if comp.source_stats(0) is not None:
    failures.append("not-started comp.source_stats(0) should be None")
if comp.is_running is not False:
    failures.append(f"not-started comp.is_running should be False, got {comp.is_running}")
# stop() before start() is an idempotent no-op (must not raise).
comp.stop()
comp.stop()
print("composition not-started contract: ok")

# A zero-quantum builder must fail preflight with ConfigurationError.
g2 = rsac.Group("main")
g2.source("system")
zero_q = rsac.CompositionBuilder(quantum_ms=0)
zero_q.add_group(g2)
try:
    zero_q.preflight()
    failures.append("zero quantum_ms preflight should raise ConfigurationError")
except rsac.ConfigurationError:
    print("zero-quantum preflight rejected: ok")

# Invalid gain is rejected eagerly by Group.source_with_gain.
g3 = rsac.Group("g")
try:
    g3.source_with_gain("system", -1.0)
    failures.append("negative gain should raise ConfigurationError")
except rsac.ConfigurationError:
    print("invalid gain rejected: ok")

# ── device enumeration (headless-tolerant) ────────────────────────────
try:
    devices = rsac.list_devices()
    print(f"devices: {len(devices)} enumerated")
except Exception as exc:  # headless CI: no audio stack is acceptable
    print(f"device enumeration skipped (headless?): {exc}")

if failures:
    for f in failures:
        print(f"FAIL: {f}", file=sys.stderr)
    sys.exit(1)
print("python binding smoke: OK")
