# Test Coverage Gate

## Context

`lt` has a test suite (`make test`, run with and without the `sim` feature) but
at the time of this decision had no measurement of what it covers and no gate
against regressions. The baseline then was 45.85% lines; the seams it lacked
(network fakes, a driveable event loop) have since landed as `FakeTransport` and
the loop tests that drive a real `Runtime` without starting `run()`. Coverage as
of 2026-07-06, measured with `cargo-llvm-cov` over both test configurations
merged:

```text
TOTAL   lines 83.44%   regions 81.89%   functions 84.13%
```

The remaining uncovered mass is CLI command orchestration and the auth/sync IO
periphery:

```text
  0% covered                                    partially covered
  ├─ lt-cli: main, logging, output, auth,       ├─ lt-config/src/lib.rs      44%
  │  sync, sim, search, issues/{mod,list},      ├─ lt-tui/src/ui/text_span   31%
  │  inbox/mod                                  ├─ lt-runtime/src/sync/probe 38%
  ├─ lt-upstream/src/auth/{refresh,logout,      ├─ lt-tui/src/new_issue      52%
  │  status}                                    ├─ lt-tui/src/popup          57%
  ├─ lt-runtime/src/{issues.rs, sync/full.rs}   └─ lt-tui/src/detail         60%
  └─ lt-tui/src/present/comment.rs
```

The gate exists to (a) stop coverage regressing and (b) ratchet the floor up as
the remaining gaps close.

## Decision

### Tool — `cargo-llvm-cov`

LLVM source-based instrumentation (`-C instrument-coverage`), the de-facto
standard for Rust. Chosen over `cargo-tarpaulin` because:

- The toolchain is already nightly; source-based coverage needs only the
  `llvm-tools-preview` component, which the rust-overlay toolchain provides.
- It merges multiple test invocations into one report — required here, since the
  `sim`-gated code only compiles under `--features sim`.
- nixpkgs ships `cargo-llvm-cov` 0.8.5, and its bundled
  `llvm-cov`/`llvm-profdata` match rustc's LLVM (22.1.7) exactly, so there is no
  version-skew risk.

### Nix wiring

```text
toolchain.extensions += "llvm-tools-preview"   # provides llvm-cov, llvm-profdata
lt.nativeBuildInputs += cargo-llvm-cov          # available in the devshell
```

`cargo-llvm-cov` auto-discovers `llvm-cov`/`llvm-profdata` from the toolchain
sysroot once the extension is present; no `LLVM_COV`/`LLVM_PROFDATA` overrides
are needed.

### Makefile

`make cov` is the gate; `make cov-html` writes a browsable report for finding
gaps. Both share `cov-collect`, which runs each test configuration under
instrumentation and accumulates profile data:

```text
cov-collect ─▶ llvm-cov clean --workspace
            ─▶ llvm-cov --no-report                 # default features
            ─▶ llvm-cov --no-report --features sim  # sim-gated code
cov         ─▶ llvm-cov report --summary-only --fail-under-lines $(COVERAGE_FLOOR)
cov-html    ─▶ llvm-cov report --html
```

### The ratchet — a single monotonic floor

`COVERAGE_FLOOR` in the `Makefile` is the one source of truth. `make cov` fails
when measured line coverage drops below it. The floor only ever moves up, in the
same change that adds the covering tests:

```text
  add tests ─▶ measured climbs ─▶ raise COVERAGE_FLOOR ─▶ commit together
                                          ▲
                          floor is a one-way latch; CI blocks any drop below it
```

It starts at `45` (just under the 45.85% baseline). Line coverage is the gated
metric: it is the most stable under refactors and the most legible. Regions and
functions are still reported by `make cov`/`cov-html` for diagnosis but are not
gated.

### Exclusions — defaults only

`cargo-llvm-cov` already excludes `build.rs`, dependencies, and the generated
parser in `OUT_DIR` (verified: reports with and without an explicit ignore-regex
for them are byte-identical). Nothing else is excluded. The 0%-covered IO
modules stay in the denominator deliberately — hiding them would inflate the
number and erase the signal about which seams still need building.

### CI

The coverage run executes the full suite, so it subsumes the plain test run. CI
replaces the `make test` step with `make cov`:

```text
nix flake check ─▶ nix build .#lt ─▶ make check ─▶ make cov
```

`make test` stays in the `Makefile` for fast local iteration without an
instrumented rebuild.

## Consequences

- New gate `make cov`, wired into CI; `nix develop` gains `cargo-llvm-cov` and
  the `llvm-tools-preview` component (see [[nix.md]]).
- CI does one extra instrumented build (replacing the uninstrumented test run).
- Raising coverage now has teeth: the floor blocks regressions, and the
  per-module map above is the worklist for driving it toward 100%.
- Inline `#[cfg(test)]` modules are counted in the denominator (llvm-cov has no
  clean way to exclude same-file test bodies). This is a small, constant
  inflation that does not affect the ratchet's monotonicity.

## Rejected alternatives

- **`cargo-tarpaulin`**: Linux-only, historically less accurate, and its
  multi-run merge story is weaker. No advantage here over llvm-cov on a nightly
  toolchain.
- **Auto-tightening guard** (fail when measured exceeds the floor by more than a
  slack margin, forcing a bump every time coverage rises): makes the ratchet
  self-enforcing but turns every refactor that shifts the line denominator into
  gate churn. The monotonic floor plus the bump-in-the-same-PR convention gets
  the ratchet without the friction.
- **Excluding the IO modules** (`linear/*`, `sync/*`, `tui/mod.rs`) from the
  metric: would lift the headline number immediately but bury exactly the code
  that most needs test seams. Kept in the denominator on purpose.
- **Per-file floors**: a manifest of file→min% would let already-covered files
  lock at 100% and block new untested files. Stronger, but it needs JSON parsing
  and a manifest to maintain. Deferred until the single floor proves too coarse.
