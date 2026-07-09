# Docker Status

The old Docker test matrix is retired. It depended on removed examples,
removed helper binaries, stale toolchain pins, and root `docker-compose*.yml`
files that no longer represented the maintained CI path.

Use these maintained paths instead:

- Local lint/build gate: `mise run gate`
- Host test replica: `mise run test`
- Local audio testing on real hardware: `mise run test:audio`
- CI audio details: [`CI_AUDIO_TESTING.md`](CI_AUDIO_TESTING.md)
- Physical-machine testing: [`LOCAL_TESTING_GUIDE.md`](LOCAL_TESTING_GUIDE.md)

The repository still keeps two Docker-adjacent surfaces:

- `docker/linux/Dockerfile.test` powers the VS Code devcontainer for a
  Linux/PipeWire development environment.
- `docker-compose.native-testing.yml` and `docker/dockur/` are an optional,
  manual VM lab for native Windows/macOS experiments. They are not part of the
  normal gate or release checklist.

Do not use the removed `make docker-*` or root `docker-compose*.yml` test matrix
commands as verification evidence. Rebuild any future container test lane around
the current `ci_audio` suite and the scripts documented in `scripts/README.md`.
