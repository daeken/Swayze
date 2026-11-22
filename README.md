# Swayze

Swayze is a Rust project for a standalone JavaScript and WebAssembly engine, designed to be embedded in a security-first browser. **Status: design and build scaffold.** It does not parse, validate or execute guest programs yet. There is no JIT or browser sandbox here.

Start with the **[engine design](docs/design.md)** for the threat model, language targets, host/browser boundary, JIT admission criteria and delivery gates. Security takes precedence over speed or feature coverage.

| Path | Responsibility today |
| --- | --- |
| `crates/swayze-engine` | Rust embedder-facing facade; `prepare_js` and `prepare_wasm` return typed `Unsupported` errors for all inputs. |
| `crates/swayze-js`, `crates/swayze-wasm` | Separate frontend boundaries, with no external crates. Neither accepts a program as valid. |
| `vendor/test262` | Official TC39 corpus, pinned as a Git submodule. Test data only; not a build dependency. |
| `docs/`, `tools/` | Design, corpus maintenance notes and non-executing corpus check. |

## Check a checkout

From the repo root, with Git and [rustup](https://rustup.rs/) installed:

```sh
rustup show active-toolchain     # install/select the repo's pinned Rust toolchain
cargo build --workspace --frozen
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --frozen -- -D warnings
cargo test --workspace --frozen
```

The first toolchain installation needs network access. Cargo itself needs no registry packages for this scaffold. First-party crates inherit `forbid(unsafe_code)`; the release profile keeps overflow checks enabled. The tests assert that even a valid script or empty Wasm module is reported as **unsupported**, not as a successful parse or a syntax error.

To populate and verify Test262 separately:

```sh
git submodule update --init --depth 1 -- vendor/test262
sh tools/check-test262.sh
```

This checks the revision and sample paths; it does **not** run JavaScript. See [corpus maintenance](docs/test262.md) for the trust boundary, license and update process.

Swayze is [MIT-licensed](LICENSE); third-party corpus files retain their upstream terms.
