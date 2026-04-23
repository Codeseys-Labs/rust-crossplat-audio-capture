# abi3 vs per-version Python wheels — decision

**Status:** Recommendation
**Date:** 2026-04-17
**Issue:** [#18](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/issues/18)
**Scope:** `bindings/rsac-python` PyPI release strategy
**Verdict:** **Adopt `abi3-py39`** after v0.2.0 ships on the current per-version matrix.

## 1. Context

rsac-python currently ships one wheel per (OS × CPython minor) combination.
`release-pypi.yml` builds 3 OS × 5 Python = **15 wheel jobs** per tag, plus 1 sdist
and 1 publish job. Loop 22 A3 introduced the PyO3/maturin-action pipeline; loop 23
A3 expanded the matrix to py3.9–3.13. Issue #18 (opened at end of loop 23) called
for a research spike to decide whether to collapse the matrix via the Python
Stable ABI ("abi3") before CPython 3.14 ships and pushes the matrix to 18 jobs.

This doc is that spike. No code changes yet — it only recommends a direction and
enumerates the migration steps so a follow-up PR can execute cleanly.

## 2. pyo3 abi3 overview

PEP 384 (accepted 2009, implemented in CPython 3.2) defines a subset of the
CPython C API that is **forward-compatible across all Python 3.x minor
versions**. An extension compiled against `Py_LIMITED_API` links only symbols in
this subset and can be loaded by any Python ≥ the floor chosen at compile time.

PyO3 exposes this via Cargo features:

- `abi3` — enables `Py_LIMITED_API` with no version floor (unusual; prefer a floor).
- `abi3-py39` (and `-py310`, `-py311`, etc.) — sets the minimum Python version.

When set, the generated `.so` / `.pyd` works on that version **and all future
Python 3 releases**, including 3.13, 3.14, and versions not yet released. The
wheel filename uses the `abi3` ABI tag: e.g. `rsac-0.2.0-cp39-abi3-manylinux_2_17_x86_64.whl`.

maturin automatically emits the `abi3` wheel tag when it sees the `abi3-py*`
feature enabled on pyo3 — no extra CLI flag needed. `--interpreter` still
picks a Python to build against, but the output targets the minimum floor.

## 3. Trade-offs table

| Dimension | per-version (today) | abi3-py39 |
|---|---|---|
| **CI matrix jobs** | 3 OS × 5 Py = **15** | 3 OS × 1 Py = **3** |
| **Jobs on CPython 3.14 launch** | 3 OS × 6 Py = 18 (matrix edit needed) | **3** (zero matrix churn) |
| **Wheels published per tag** | 15 wheels + 1 sdist | 3 wheels + 1 sdist |
| **Install UX** | pip picks exact cp3xx-cp3xx wheel | pip picks single cp39-abi3 wheel for any 3.9+ |
| **CPython 3.9–3.13 coverage** | Yes | Yes |
| **Forward compat (3.14, 3.15…)** | Need new matrix + new release | Automatic, no re-release needed |
| **PyPy / GraalPy** | Out of scope today (matrix lists CPython only) | Still out of scope (abi3 is CPython-only; PyPy needs cpyext, a separate build) |
| **Per-version CPython optimizations** | Full (pyo3 compiles against exact version) | Disabled (pyo3 can't specialize) |
| **`text_signature` on `#[pyclass]`** | Always available | Py 3.10+ only (not used by rsac-python) |
| **`dict` / `weakref` on `#[pyclass]`** | Always available | Py 3.9+ (matches our floor) |
| **Buffer protocol (`#[pyo3(buffer)]`)** | Always available | **Py 3.11+ only** (not used by rsac-python) |
| **Subclass native types via `#[pyclass(extends=PyException)]`** | Always available | **Py 3.12+ only** (not used by rsac-python) |
| **CI wall-clock per release** | ~15-way parallel, slowest job wins | ~3-way parallel, same slowest-job floor |
| **Total CI minutes per release** | 15 × (build+cache) ≈ 5× today | ~3× (80% reduction) |
| **Maintainer cognitive load on new Python release** | Edit matrix, re-test 3 OS | **Zero** (abi3 auto-works) |
| **Risk of per-version wheel rot** | Real (loop 22 abandoned sccache-based matrix for this reason) | Not applicable |

## 4. rsac-python compatibility audit

I reviewed `bindings/rsac-python/src/lib.rs` (848 lines) against the four
known abi3 restrictions documented in pyo3's [building-and-distribution
guide](https://pyo3.rs/v0.24.0/building-and-distribution.html#pyo3s-limited-python-api):

### 4.1 `text_signature` on `#[pyclass]` — **not used**

Grepped for `text_signature` across `bindings/rsac-python/src/lib.rs`: zero hits.
All API discoverability relies on `///` doc comments (abi3-safe) and
`#[pyo3(signature = ...)]` on methods (also abi3-safe — the Py 3.10+ restriction
applies only to the class-level `text_signature` attribute).

### 4.2 `dict` / `weakref` options on `#[pyclass]` — **not used, and floor matches**

All five `#[pyclass]` declarations (`PyCaptureTarget` lib.rs:179,
`PyAudioBuffer` lib.rs:264, `PyAudioDevice` lib.rs:385,
`PyPlatformCapabilities` lib.rs:436, `PyAudioCapture` lib.rs:514) use only
`name`, `module`, and `frozen` — none set `dict` or `weakref`. Even if a future
addition needs them, Py 3.9+ satisfies the abi3 constraint and we already require
`>=3.9` in `pyproject.toml:12`.

### 4.3 Buffer protocol — **not used**

Grepped for `PyBuffer`, `#[pyo3(buffer)]`, `__buffer__`, `PyBufferProtocol`: zero
hits. `PyAudioBuffer::to_bytes` (lib.rs:315-321) deliberately copies into
`PyBytes::new(py, byte_slice)` rather than exposing a zero-copy buffer view —
this is already abi3-compatible. If we later want zero-copy numpy interop, we
would need `abi3-py311` (or accept the copy cost).

### 4.4 Subclassing native types via `#[pyclass(extends=...)]` — **not used**

This is the restriction I was most concerned about because the module defines an
**exception hierarchy** that subclasses `PyOSError`, `PyValueError`, and each
other (lib.rs:70-129). However, these are built with the
`pyo3::create_exception!` macro, **not** `#[pyclass(extends=PyException)]`.

Per inspection of pyo3 0.24's `exceptions.rs`, `create_exception!` expands to
`PyErr::new_type`, which wraps CPython's `PyErr_NewExceptionWithDoc` — a
stable-ABI function available since Python 3.2. The abi3 restriction on
subclassing native types applies only to `#[pyclass]`-based type layouts that
need compile-time knowledge of the base type's struct, which `create_exception!`
does not require.

**Verdict for the exception hierarchy:** abi3-compatible. This was confirmed
against the pyo3 source ([exceptions.rs](https://docs.rs/pyo3/0.24.0/src/pyo3/exceptions.rs.html)).

### 4.5 Other APIs used — all stable-ABI

| API used | lib.rs reference | abi3 status |
|---|---|---|
| `#[pyclass(frozen)]`, `#[pymethods]`, `#[new]`, `#[getter]`, `#[staticmethod]` | throughout | Compile to stable-ABI calls |
| `#[pyo3(signature = (...))]` on methods | lib.rs:326, :530, :695 | Method-level only, abi3-safe |
| `py.allow_threads(|| ...)` | lib.rs:542, :576, :593, :624, :648, :679, :748, :784 | Stable ABI |
| `PyBytes::new(py, &[u8])` | lib.rs:315 | Stable ABI |
| `PyStopIteration::new_err(...)` | lib.rs:740, :744, :754 | Stable ABI |
| `PyOSError`, `PyValueError` as exception bases | lib.rs:70-105 | Stable-ABI globals (PEP 384 §Global Variables) |
| Iterator protocol (`__iter__`, `__next__`) | lib.rs:709-758 | Stable ABI (`tp_iter`, `tp_iternext`) |
| Context manager protocol (`__enter__`, `__exit__`) | lib.rs:690-705 | Python-level dunder, stable |
| `Py<T>::borrow`, `Py<T>::borrow_mut` | lib.rs:711, :691 | pyo3 reference machinery, stable |
| `std::sync::Mutex` around `rsac::AudioCapture` | lib.rs:517-520 | Pure Rust, no Python involvement |

### 4.6 Free-threaded CPython 3.13+ (PEP 703)

Free-threaded CPython (`python3.13t`) is a separate ABI from regular CPython
and is **not covered by PEP 384 / abi3**. Wheels that want to support
`python3.13t` must build with the `--python-version-info free-threading` tag
(maturin >= 1.7.3). This is independent of the abi3 decision — both the
per-version and abi3 approach currently ignore the free-threaded build, and
adding it later is the same amount of work either way (one extra matrix entry).

### 4.7 Audit verdict

**Zero abi3-incompatible APIs in the current rsac-python source.** Migration is
purely a build-system change; no Rust edits are required.

## 5. Recommendation

**Adopt `abi3-py39` for rsac-python v0.3.0.** The code is already abi3-compatible
(audit §4 found zero blockers), the CI cost drops 80% (15 jobs → 3), and the
wheel set auto-covers every future CPython 3.x without matrix edits — which is
exactly the kind of ongoing maintenance cost loop 23 A3 flagged as unsustainable.
The only thing abi3 forfeits is per-version pyo3 optimizations, which are
negligible for a binding whose hot path releases the GIL and spends all its time
in pure Rust (audio I/O), not in the FFI boundary.

**Keep per-version for v0.2.0.** The issue-18 brief already says "post v0.2.0,"
and we want one more release on the current matrix to validate which platforms
users actually install from before collapsing the wheel set.

## 6. Migration steps (for the follow-up PR)

Concrete file-level diffs for when we adopt this.

### 6.1 `bindings/rsac-python/Cargo.toml` (currently line 16)

```diff
-pyo3 = { version = "0.24", features = ["extension-module"] }
+pyo3 = { version = "0.24", features = ["extension-module", "abi3-py39"] }
```

### 6.2 `bindings/rsac-python/pyproject.toml`

No changes required. `requires-python = ">=3.9"` (line 12) already matches the
abi3 floor. The classifiers list for 3.9–3.13 can stay (they advertise tested
versions; abi3 doesn't reduce the set of versions that work).

### 6.3 `.github/workflows/release-pypi.yml`

Replace the existing `build-wheels` matrix (currently lines 39-76):

```diff
 jobs:
   build-wheels:
-    name: Wheel (${{ matrix.os }} py${{ matrix.python }})
+    name: Wheel (${{ matrix.os }} abi3)
     strategy:
       fail-fast: false
       matrix:
         os:
           - blacksmith-4vcpu-ubuntu-2404
           - blacksmith-6vcpu-macos-15
           - blacksmith-4vcpu-windows-2025
-        python: ['3.9', '3.10', '3.11', '3.12', '3.13']
     runs-on: ${{ matrix.os }}
     steps:
       - uses: actions/checkout@v5

       - uses: actions/setup-python@v5
         with:
-          python-version: ${{ matrix.python }}
+          python-version: '3.9'

       - uses: dtolnay/rust-toolchain@1.95.0

       - uses: Swatinem/rust-cache@v2
         with:
           workspaces: bindings/rsac-python -> target

       - name: Build wheel (maturin-action)
         uses: PyO3/maturin-action@v1
         with:
           command: build
           target: auto
           manylinux: auto
-          args: --release --out dist --interpreter ${{ matrix.python }} --manifest-path bindings/rsac-python/Cargo.toml
+          args: --release --out dist --manifest-path bindings/rsac-python/Cargo.toml

       - name: Upload wheel artifact
         uses: actions/upload-artifact@v4
         with:
-          name: wheels-${{ matrix.os }}-py${{ matrix.python }}
+          name: wheels-${{ matrix.os }}-abi3
           path: dist/*.whl
           if-no-files-found: error
```

Notes:

1. Python 3.9 on the runner is the floor that abi3 will target. maturin reads
   the `abi3-py39` feature from Cargo.toml and auto-emits the `abi3` wheel tag —
   no `--abi3` CLI flag is required in modern maturin / PyO3/maturin-action.
2. `--interpreter` is dropped because abi3 decouples the build interpreter from
   the wheel's runtime interpreter set.
3. The `sdist` and `publish-pypi` jobs are unchanged.

### 6.4 Verification checklist for the migration PR

- [ ] `maturin build --release --manifest-path bindings/rsac-python/Cargo.toml`
  locally produces a wheel named `rsac-0.3.0-cp39-abi3-<platform>.whl` (note
  `cp39-abi3`, not `cp39-cp39`).
- [ ] Install the produced wheel on Python 3.9, 3.10, 3.11, 3.12, 3.13 and run
  `python -c "import rsac; print(rsac.platform_capabilities())"`.
- [ ] `python -c "import rsac; rsac.RsacError"` — verify the exception hierarchy
  is intact (this is the riskiest thing in the audit).
- [ ] Push a pre-release tag (e.g. `v0.3.0rc1`) to `TestPyPI` before promoting.
- [ ] Confirm `pip install rsac` on a fresh Python 3.14 alpha works (proves
  forward compat). Optional but high-value.

### 6.5 Rollback

If abi3 breaks at runtime on any supported platform, revert the Cargo.toml
feature change and restore the per-version matrix in release-pypi.yml. The
sdist keeps source installs working throughout any regression window. No
consumer-visible API changes — downstream users don't need to pin or unpin.

## 7. References

- Issue #18 — [bindings: decide abi3 vs per-version Python wheels](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/issues/18)
- PEP 384 — [Defining a Stable ABI](https://peps.python.org/pep-0384/)
- pyo3 0.24 guide — [Building and Distribution §PyO3's limited Python API](https://pyo3.rs/v0.24.0/building-and-distribution.html#pyo3s-limited-python-api)
- pyo3 0.24 guide — [Features list (abi3-py*)](https://pyo3.rs/v0.24.0/features.html)
- pyo3 source — [exceptions.rs (create_exception! expansion)](https://docs.rs/pyo3/0.24.0/src/pyo3/exceptions.rs.html)
- maturin guide — [Bindings (abi3 + pyo3)](https://www.maturin.rs/bindings)
- PyO3/maturin-action — [README examples of abi3 wheels](https://github.com/PyO3/maturin-action)
- Current release workflow — `.github/workflows/release-pypi.yml`
- Source under audit — `bindings/rsac-python/src/lib.rs`, `bindings/rsac-python/Cargo.toml`, `bindings/rsac-python/pyproject.toml`
