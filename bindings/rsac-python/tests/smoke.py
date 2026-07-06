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
