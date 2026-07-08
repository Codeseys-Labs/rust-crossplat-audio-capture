# docker/ — Current Container Surfaces

The legacy Docker test matrix has been retired. Docker is now used only for the
Linux/PipeWire devcontainer and the optional native VM lab under `dockur/`.

## Layout

| Subdir | Purpose |
|---|---|
| `linux/Dockerfile.test` | Ubuntu/PipeWire image used by `.devcontainer/devcontainer.json`. |
| `dockur/` | Optional full Windows + macOS virtual machines running inside Docker via [dockur/windows](https://github.com/dockur/windows) + [dockur/macos](https://github.com/dockur/macos). Manual lab only, not a release gate. |

## Devcontainer

```bash
docker build -f docker/linux/Dockerfile.test -t rsac-linux-devcontainer .
```

VS Code uses this image automatically when reopening the repository in the
devcontainer. Inside it, use the normal project commands (`mise run gate`,
`mise run test`, or targeted `cargo` commands).

## Optional Native VM Lab

`docker-compose.native-testing.yml` is the only root compose file that remains.
It is for manual Windows/macOS VM experiments and requires a host that supports
the underlying virtualization stack.

See [docs/DOCKER_TESTING.md](../docs/DOCKER_TESTING.md) for the retirement note
and maintained verification paths.

## Why Not `.docker/`?

The previous `.docker/` hidden directory was an early-development orphan
with no references from scripts, docs, or CI. It was removed in the
2026-04-24 repo reorg — all active containerization lives here under
`docker/`.
