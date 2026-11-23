# Swayze

Swayze is a Rust project for a standalone JavaScript and WebAssembly engine, designed to be embedded in Keanu, a security-first browser engine. **Status: early implementation.** JavaScript supports Number, Boolean, `null`, `undefined`, and bounded String literal values, decimal and explicit-radix Number literals with numeric separators, arithmetic, comparisons, bitwise operators, short-circuit logic and nullish coalescing, conditional expressions, braced `if` chains, `while`, `do…while`, and classic `for` loops, standalone blocks, optional labels on each of these forms, labeled and unlabeled jumps, explicit-semicolon expression statements, empty statements, and script- and block-scoped `const` and `let` bindings with simple, arithmetic, bitwise, and logical reassignment plus prefix and postfix increments and decrements in statements and supported expressions. JavaScript evaluations are limited to 16,384 instruction dispatches. WebAssembly can validate empty and custom-only modules, accepts empty type, function, code, and export vectors, and can run up to eight import-free functions. Each function selects one of up to eight signatures with up to four numeric parameters and an optional numeric result (`i32`, `i64`, `f32`, or `f64`). Hosts can call mixed numeric exports with `invoke_with_values`, using exact `WasmValue` tags and an explicit `WasmResultKind`. `WasmValue::F32(u32)` and `WasmValue::F64(u64)` carry raw IEEE 754 bits, including all NaN encodings; callers with exhaustive matches on either enum must handle the new float variants. The existing `i32` and void invocation methods remain available. Function bodies support mutable `i32`, `i64`, `f32`, and `f64` locals, per-instance globals of those same four types, integer arithmetic and bit operations, `nop`/`drop`/`select`, bounded `block`, `if`/`else`, and `loop` with an optional `i32`, `i64`, `f32`, or `f64` fallthrough result (branches back to a loop carry no values), direct calls, `return`, `br`, `br_if`, and `br_table` to enclosing block, loop, `if`, or function labels (up to 16 explicit entries per table, plus its default), and the `unreachable` trap. Internal `i64` operands support constants, `drop`, same-type `select`, comparisons, wrapping add/subtract/multiply, signed and unsigned division and remainder, bit operations, and conversions to and from `i32`. Internal `f32` and `f64` operands support constants, exact bit reinterpretation, `abs`, `neg`, `copysign`, comparisons, `ceil`, `floor`, `trunc`, `nearest` (ties to even), `sqrt`, `add`, `sub`, `mul`, `div`, `min`, `max`, `drop`, and same-type `select`. Rounding preserves signed zero; `min` chooses −0 and `max` chooses +0 when the zero signs differ. Rounding, square root, and min/max map NaNs to a canonical quiet NaN. Square root also maps negative nonzero inputs to that NaN, preserves signed zero, and uses at most 53 integer steps for a correctly rounded result. Numeric conversion between these widths preserves signed zeros and infinities, widens finite `f32` values exactly, and rounds narrowing to nearest, ties to even. These conversions canonicalize NaNs rather than preserving their original encoding. Signed and unsigned `i32` and `i64` operands convert to either float width with a single ties-to-even rounding step. Float-to-integer conversions truncate toward zero before checking range. NaN traps with `WasmTrap::InvalidConversionToInteger` (a new variant for exhaustive matches); infinity and out-of-range results trap with `WasmTrap::IntegerOverflow`. Addition, subtraction, multiplication, and division use integer arithmetic to round once to nearest, ties to even, including subnormal results. NaN operands and invalid combinations such as infinity minus itself, zero times infinity, zero divided by zero, and infinity divided by infinity return a canonical quiet NaN. Dividing a finite nonzero value by zero returns a signed infinity, not a trap. Modules may also declare one per-instance linear memory of at most one 64 KiB page, with full-width loads and stores for all four numeric types, narrow integer loads and stores, `memory.size`, and `memory.grow` within the declared maximum and one-page engine limit. Modules may also define a private `funcref` table of up to 16 entries with bounded active element segments. `call_indirect` checks the selected slot and signature before entering the callee. Instantiation checks element ranges before data ranges, then initializes each kind in source order before running the optional `() -> ()` start function. Start functions and export invocations each have their own limit of 16 live frames and 1,024 instruction dispatches. There is no JIT or browser sandbox here.

Start with the **[engine design](docs/design.md)** for the threat model, language targets, host/browser boundary and end-to-end JIT proof goal. The [GC ownership contract](docs/gc-ownership.md) explains who keeps values alive across Keanu callbacks and navigation. Working code may land before it is proven; document what works, what evidence exists and what is still unproven. The current implementation makes no security-proof claim.

`Script::evaluate_primitive()` returns a `JsPrimitiveValue`: either an existing scalar `JsValue` or a borrowed UTF-16 slice, including any lone surrogates. `evaluate_value()` remains scalar-only and `evaluate()` remains Number-only. Strings support assignment, truthiness, equality and UTF-16 code-unit ordering within the documented subset. Unary `+`, `-`, and `~`, binary `*`, `/`, `%`, and `**`, and binary bitwise and shift operators convert Strings to Numbers within a separate work budget. That includes the corresponding compound assignments, mixed String/scalar ordering, and loose equality between a String and a Number or Boolean. String concatenation, binary subtraction on Strings, and String updates remain unsupported. Exhaustive `JsLimit` matches need two new arms for literal bytes and aggregate UTF-16 units; `JsLimits` is unchanged.

A module can export its defined memory by name. `WasmInstance::read_memory()` copies a checked range into a caller buffer; it does not expose a live view or permit host writes. Unexported memory is not accessible through this method. `WasmInstance::read_global()` takes an owned snapshot of a named global's exact numeric value, without exposing a live handle or write access. Function, memory, and global exports share the eight-name limit. Exhaustive `WasmInvokeError` matches need to handle `ExportNotFunction`.

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

With Node.js on `PATH`, `sh tools/check-js-reference.sh` and `sh tools/check-wasm-reference.sh` compare generated fixtures against an independent engine. The Wasm check uses Node.js v22, compares `i64` values as BigInts, and compares copies from memory exports byte-for-byte. Neither check is required by the local merge gate.

## Push and merge policy

Push commits to their working branch by default. For a PR, run the internal gate on a clean checkout of the proposed merge with current `main`. Initialize Test262 as above, then run:

```sh
sh tools/check.sh
```

This checks the corpus pin, formatting, Clippy and both debug and release tests. Merge and push the tested result when the gate passes; if the PR or base changes, retest the new candidate. We run this on our side, not on GitHub Actions. For untrusted PRs use a disposable environment without credentials, and review changes to the gate itself before relying on it.

Swayze is [MIT-licensed](LICENSE); third-party corpus files retain their upstream terms.
