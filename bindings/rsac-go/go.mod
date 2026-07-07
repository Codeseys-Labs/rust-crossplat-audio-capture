// Repo-path-prefixed module path: this module lives in the bindings/rsac-go
// subdirectory of the rust-crossplat-audio-capture repository, and Go's
// subdirectory-tag convention (tags shaped bindings/rsac-go/vX.Y.Z, pushed by
// .github/workflows/release-tag.yml) only resolves when the module path is
// <repo path>/<subdirectory>. A short vanity path like
// github.com/Codeseys-Labs/rsac-go would require a separate mirror repo.
module github.com/Codeseys-Labs/rust-crossplat-audio-capture/bindings/rsac-go

go 1.22
