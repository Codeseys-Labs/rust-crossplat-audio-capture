#!/bin/bash
# ⚠️ DISABLED — this script is non-functional and fails fast on purpose.
#
# It drove the in-container audio test matrix by building and running the
# `run_tests` and `test-report-generator` binaries, both of which were
# removed from Cargo.toml (Phase 0 legacy-API removal; `autobins = false`
# makes them unbuildable regardless). It is kept only because live callers
# still reference it (docker-compose.yml services, docker-compose.unified.yml:70,
# docker/linux/Dockerfile.unified:24) — those docker matrix legs are
# themselves non-functional (Dockerfile.unified also COPYs pulse-*.conf
# files that don't exist).
#
# Recovering the functionality means rebuilding the driver on the current
# test surface (`cargo test --test ci_audio`), not resurrecting the old
# bins. Full history: `git log --follow scripts/run_audio_tests.sh`.
# Tracking: scripts/README.md.
echo "run_audio_tests.sh is DISABLED: it depended on binaries removed in Phase 0 (run_tests, test-report-generator)." >&2
echo "Use 'cargo test --test ci_audio --features feat_<os>' instead. See scripts/README.md." >&2
exit 1
