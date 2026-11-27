# Swayze

Swayze is an early-stage JavaScript and WebAssembly engine written in Rust. It
runs standalone today and is being designed for [Keanu](docs/design.md), a
security-first browser engine. Our priorities are memory safety and security,
then performance, then standards coverage.

## What works today

| Surface | Implemented subset |
| --- | --- |
| **JavaScript evaluator** | Primitive expressions, bounded UTF-16 strings and cooked templates, lexical bindings, conditionals, loops, and labeled control flow. `parse_script` provides the standalone entry point; primitive result APIs preserve lone surrogates. |
| **Opt-in JavaScript Realm** | Persistent globals and lexical bindings, strict code, ordinary calls and returns, and ASCII dot reads, simple writes, and member calls. It now supports `throw Expression;` and `try { … } catch (name) { … }`, including unwinding across function calls. Automatic `ReferenceError` and `TypeError` become real Error objects when caught. Uncaught throws report a kind and source span, not a guest value; engine faults and declaration-instantiation failures are not catchable. |
| **WebAssembly** | A bounded Core 1.0 subset with typed numeric calls, locals and globals, structured control flow, direct and indirect calls, tables, and up to two 64 KiB memory pages. Hosts can copy bytes from exported memory, snapshot exported globals, and query exported table sizes. Imports are rejected by default; a separate opt-in API accepts immutable numeric global imports by value. |

The targets are **ECMA-262 (2025, 16th edition)** and **WebAssembly Core 1.0**,
not claims of conformance. JavaScript still lacks general object construction,
computed property access, closures, `finally`, and the host integration needed by
the standard Test262 harness. There is no JIT, tracing GC, or browser sandbox.
The implementation uses explicit resource limits and forbids first-party unsafe
Rust; neither that nor the tests constitute a security proof. Any future shipped
JIT needs end-to-end machine-checked correctness.

## Try it

Install [rustup](https://rustup.rs/), then run from the repository root:

```sh
cargo run --frozen -p swayze-engine --example first_slice
```

The example prints `JavaScript result: 11` and `WebAssembly result: 42`.
The repository pins the Rust toolchain in `rust-toolchain.toml`. Cargo needs no
registry packages, and the example does not require the Test262 submodule.

## Testing and conformance

```sh
# Workspace tests without the corpus
cargo test --workspace --frozen

# Full local merge gate: corpus metadata, fmt, Clippy, debug and release tests
git submodule update --init --depth 1 -- vendor/test262
sh tools/check.sh
```

The full gate checks the Test262 pin and metadata; it **does not execute the
corpus**. Test and publish the exact merge candidate locally. Review changes to
the gate before relying on it, and use a credential-free disposable environment
for untrusted PRs.

The separate [reviewed Test262 slice](docs/test262.md) last recorded **24 passes
per profile**: 16 adapted cases, 4 unchanged raw positives, and 4 unchanged raw
parse negatives. Another 14 strict variants were not run. There are **zero
standard harness passes** so far. The goal is to grow genuine coverage without
counting adapted cases or unrun variants as standard passes. Optional Node.js
reference checks use synthetic fixtures only, never Test262 guests.

## Where to read more

- [Engine design](docs/design.md) and [Keanu GC ownership contract](docs/gc-ownership.md)
- [JavaScript semantics, limits, and evidence](docs/js-arithmetic.md)
- [WebAssembly API and limits](crates/swayze-wasm/src/lib.rs)
- [Embedder-facing facade](crates/swayze-engine/src/lib.rs)

Swayze is [MIT-licensed](LICENSE); third-party corpus files retain their upstream
terms.
