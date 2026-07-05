#!/bin/bash
# ⚠️ DISABLED — this script is non-functional and fails fast on purpose.
#
# It orchestrated the docker-compose.yml PipeWire/PulseAudio test matrix,
# whose in-container driver (scripts/run_audio_tests.sh) depends on
# binaries removed in Phase 0 — see that file's header. Kept only as a
# referenced entry point (docs/TESTING.md, now quarantined, pointed here).
#
# Full history: `git log --follow scripts/run_linux_matrix_tests.sh`.
# Tracking: scripts/README.md.
echo "run_linux_matrix_tests.sh is DISABLED: the docker audio matrix it drives is non-functional (see scripts/run_audio_tests.sh header)." >&2
echo "Use 'cargo test --test ci_audio --features feat_linux' or scripts/test-audio-linux.sh instead. See scripts/README.md." >&2
exit 1
