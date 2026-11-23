# Swayze

Swayze is a Rust project for a standalone JavaScript and WebAssembly engine, designed to be embedded in Keanu, a security-first browser engine. **Status: early implementation.** JavaScript supports Number, Boolean, `null`, and `undefined` values, decimal and explicit-radix Number literals with numeric separators, arithmetic, comparisons, bitwise operators, short-circuit logic and nullish coalescing, conditional expressions, braced `if` chains, `while`, `do…while`, and classic `for` loops, standalone blocks, optional labels on each of these forms, labeled and unlabeled jumps, explicit-semicolon expression statements, empty statements, and script- and block-scoped `const` and `let` bindings with simple, arithmetic, bitwise, and logical reassignment plus standalone increments and decrements. JavaScript evaluations are limited to 16,384 instruction dispatches. WebAssembly can validate empty and custom-only modules and run up to eight import-free functions. Each function selects one of up to eight signatures with up to four numeric parameters and an optional numeric result (`i32`, `i64`, `f32`, or `f64`). Hosts can call mixed numeric exports with `invoke_with_values`, using exact `WasmValue` tags and an explicit `WasmResultKind`. `WasmValue::F32(u32)` and `WasmValue::F64(u64)` carry raw IEEE 754 bits, including all NaN encodings; callers with exhaustive matches on either enum must handle the new float variants. The existing `i32` and void invocation methods remain available. Function bodies support mutable `i32`, `i64`, `f32`, and `f64` locals, per-instance globals of those same four types, integer arithmetic and bit operations, `nop`/`drop`/`select`, bounded `block` and `if`/`else` with an optional `i32`, `i64`, `f32`, or `f64` result, direct calls, `return`, `br`, `br_if`, and `br_table` to enclosing block, loop, `if`, or function labels (up to 16 explicit entries per table, plus its default), and the `unreachable` trap. Internal `i64` operands support constants, `drop`, same-type `select`, comparisons, wrapping add/subtract/multiply, signed and unsigned division and remainder, bit operations, and conversions to and from `i32`. Internal `f32` and `f64` operands support constants, exact bit reinterpretation, `abs`, `neg`, `copysign`, comparisons, `ceil`, `floor`, `trunc`, `nearest` (ties to even), `sqrt`, `min`, `max`, `drop`, and same-type `select`. Rounding preserves signed zero; `min` chooses −0 and `max` chooses +0 when the zero signs differ. Rounding, square root, and min/max map NaNs to a canonical quiet NaN. Square root also maps negative nonzero inputs to that NaN, preserves signed zero, and uses at most 53 integer steps for a correctly rounded result. Other floating-point arithmetic is not supported. Modules may also declare a private linear memory of at most one 64 KiB page, with full-width loads and stores for all four numeric types, narrow integer loads and stores, `memory.size`, and `memory.grow` within the declared maximum and one-page engine limit. Modules may also define a private `funcref` table of up to 16 entries with bounded active element segments. `call_indirect` checks the selected slot and signature before entering the callee. Instantiation checks element ranges before data ranges, then initializes each kind in source order before running the optional `() -> ()` start function. Start functions and export invocations each have their own limit of 16 live frames and 1,024 instruction dispatches. There is no JIT or browser sandbox here.

Start with the **[engine design](docs/design.md)** for the threat model, language targets, host/browser boundary and end-to-end JIT proof goal. The [GC ownership contract](docs/gc-ownership.md) explains who keeps values alive across Keanu callbacks and navigation. Working code may land before it is proven; document what works, what evidence exists and what is still unproven. The current implementation makes no security-proof claim.

| Path | Responsibility today |
| --- | --- |
| `crates/swayze-engine` | Rust embedder-facing facade; `parse_script` returns a script that can be evaluated; `validate_module` returns a Wasm module that can be instantiated. |
| `crates/swayze-js`, `crates/swayze-wasm` | Separate frontend boundaries, with no external crates. Supported subsets, limits and gaps are documented in [`swayze-js`](crates/swayze-js/README.md) and [`swayze-wasm`](crates/swayze-wasm/src/lib.rs). |
| `vendor/test262` | Official TC39 corpus, pinned as a Git submodule. Test data only; not a build dependency. |
| `docs/`, `tools/` | Design, corpus maintenance notes and non-executing corpus check. |

## Run the first slice

```sh
cargo run --frozen -p swayze-engine --example first_slice
```

This prints `JavaScript result: 11` for `let x = 2; const step = 5; x = x * step; x + 1;` and `WebAssembly result: 42` for an exported Wasm function called with `45` and `3` that subtracts its arguments. See the [JS evaluator notes](docs/js-arithmetic.md) for semantics, limits and evidence. Neither language can reach host capabilities.

## Check a checkout

From the repo root, with Git and [rustup](https://rustup.rs/) installed:

```sh
rustup show active-toolchain     # install/select the repo's pinned Rust toolchain
cargo build --workspace --frozen
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --frozen -- -D warnings
cargo test --workspace --frozen
```

The first toolchain installation needs network access. Cargo itself needs no registry packages for this scaffold. First-party crates inherit `forbid(unsafe_code)`; the release profile keeps overflow checks enabled. Tests cover supported arithmetic syntax and Wasm framing, as well as malformed and unsupported inputs. These are not conformance runs or security proofs.

To populate and verify Test262 separately:

```sh
git submodule update --init --depth 1 -- vendor/test262
sh tools/check-test262.sh
```

This checks the revision and sample paths; it does **not** run JavaScript. See [corpus maintenance](docs/test262.md) for the trust boundary, license and update process.

With Node.js on `PATH`, `sh tools/check-js-reference.sh` and `sh tools/check-wasm-reference.sh` compare generated fixtures against an independent engine. The Wasm check uses Node.js v22 and compares `i64` values as BigInts, without converting them to Numbers. Neither check is required by the local merge gate.

## Push and merge policy

Push commits to their working branch by default. For a PR, run the internal gate on a clean checkout of the proposed merge with current `main`. Initialize Test262 as above, then run:

```sh
sh tools/check.sh
```

This checks the corpus pin, formatting, Clippy and both debug and release tests. Merge and push the tested result when the gate passes; if the PR or base changes, retest the new candidate. We run this on our side, not on GitHub Actions. For untrusted PRs use a disposable environment without credentials, and review changes to the gate itself before relying on it.

Swayze is [MIT-licensed](LICENSE); third-party corpus files retain their upstream terms.
