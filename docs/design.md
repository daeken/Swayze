# Swayze engine design — bootstrap draft

**Status:** requirements for implementation, not a statement of guarantees the scaffold already provides. The first deliverable is a standalone interpreter library; its first deployment model of interest is *inside an untrusted browser renderer*. There is no executable guest language yet.

Decision order: **security, then performance, then breadth of standards coverage**. A feature we cannot bound or isolate stays off the untrusted-input path. A fast but observably wrong implementation does not count as supporting the feature.

### Working is not the same as proven

Build the smallest working slice first. A parser does not need a completed GC design; a working interpreter does not need a proof of the JIT. Keep the seam that lets us replace a prototype (e.g. opaque handles instead of addresses, a specified reference semantics instead of “whatever this backend does”). The invariants below are targets for their respective features, not prerequisites for writing the first line of code.

For each implemented slice, keep a short, versioned note next to the feature or in its feature matrix: **what works and where it may run; what evidence we have; what remains unproven or known wrong**. Update it when behavior or evidence changes. For example, today `prepare_js` and `prepare_wasm` reject all inputs; debug/release tests cover representative inputs; no parsing, isolation or memory-safety theorem has been established. A test pass, an audit and a proof are different claims. Exercise prototypes in standalone tests or an isolated fuzz harness, not on live pages. A prototype with open safety obligations can land without being enabled in Keanu.

## System boundary

Assume the page controls every JS code unit, Wasm byte, module specifier, import request, workload size and sequence of calls. Lookups, coercions, imports and callbacks may run more guest code. Assume malformed IPC, interrupted jobs, stale handles and hostile test fixtures. Protect host memory and capabilities, other principals' data, and the host's ability to reclaim a runaway guest. Do not log source, heap contents or cross-principal identifiers by default.

| Layer | Owns | Must not assume |
| --- | --- | --- |
| Engine | Language semantics, validation, isolate-owned data, metering, typed host interface | Trusted guest input, ambient files/sockets/environment, a process per realm |
| Standalone embedder | Explicit inputs/imports, budgets, lifecycle and cancellation | Passing a Rust slice to the engine creates an OS sandbox |
| Browser renderer/worker | Maps documents and workers to isolates, schedules jobs, implements bindings | A JS exception undoes a host side effect; a realm is a security boundary |
| Browser broker/OS | Principal assignment, process sandbox and site isolation; checks authorizations on IPC and mediates privileged operations | Engine checks still work after a renderer compromise |

No DOM, network loader, filesystem resolver, WASI imports, wall clock or entropy source linked in as an implicit engine service. Time, randomness, dynamic code generation, imports and scheduling have explicit host policies; a pure embedder may deny services. The browser owns CSP, CORS, origin and cross-origin-isolation decisions. The engine must route **every** applicable entry (including `eval`, function constructors, dynamic import and Wasm compilation) through the corresponding policy, not just top-level scripts. Crossing a process boundary takes a bounded, versioned data message, never a Rust pointer or an implicitly trusted origin string. No IPC transport in the initial workspace.

An **isolate** owns a heap, handles, queues, budgets and host capabilities. A **realm** owns globals and intrinsics within an isolate; realms may share objects and are not principal containers. Default to separate isolates for mutually distrusting principals; the browser must also choose process separation where required. Cross-isolate transfer is explicit (initially none); no implicit shared heap, module cache or object handles. Start single-threaded per isolate. `SharedArrayBuffer`, atomics and Wasm shared memory require a separate memory-model, timing and browser-policy review. Language isolation does not address all timing/speculation channels or defend the machine after native-process compromise.

## Data path and invariants

```text
embedder → engine facade (policy, limits, lifecycle)
             ├─ JS source  → bounded parse + early errors → checked bytecode
             └─ Wasm bytes → bounded decode + validation → validated module
                            ↓
                  instantiate/link with explicit imports
                            ↓
                  budgeted reference interpreters
                            ↕
                  scoped values and host callbacks
```

This is a target boundary diagram, not a requirement for a single IR or a shared value representation. The two language crates have no dependency on each other or on a browser. The facade mediates interop. Introduce a shared runtime crate only when there is a concrete invariant to share.

* **No unchecked ingress.** Distinguish a JS Script from a Module; retain source positions and account for JS UTF-16 code units (Rust `str` alone cannot represent all JS strings). Wasm consumes bytes, not strings. Bound nesting, section lengths, counts and temporary memory before recursing, reserving or expanding. Use checked offset/length arithmetic and checked conversion to host sizes. No loading serialized ASTs, bytecode, native code or caches as trusted objects.
* **Validation precedes execution.** Only the validator can construct executable artifacts; callers cannot substitute raw instruction vectors. Check operands, branch targets, joins, stack effects and index spaces. Wasm validation is parameterized by a declared feature set. Successful validation is not authorization to instantiate; resolve only imports explicitly granted, check types, then run initialization/start under execution limits. Keep syntax/early errors, validation/link errors, runtime exceptions/traps, host termination and unsupported features distinct.
* **Wasm validation does not prove runtime bounds.** Each instance binds only its declared memories, tables, globals and functions. Check each memory access against the current byte length, including full access width without integer wrap; likewise check table bounds and indirect-call signatures. Trap on violation, never read a neighbor's memory. Zero new pages before exposure, enforce the smaller of declared and host growth limits, and leave state unchanged on failed growth.
* **Memory ownership survives guest reentry.** Start in safe Rust with isolate-owned storage and checked handles (owner + lifetime/generation for reusable slots); no guest-controlled pointer, host reference or raw heap address as a JS/Wasm value. The [Swayze–Keanu ownership contract](gc-ownership.md) describes roots, browser edges, cross-heap cycles and teardown, including a bounded arena as the first implementation. Do not hold a borrow, lock or cached bounds across a call that may allocate, collect, detach, resize, grow memory or run guest code. Reacquire and recheck afterward. Test stale handles, detached views, Wasm memory growth and reentrant host calls as those features land.
* **All operations have a cost.** The embedder sets finite ceilings for source/module bytes, nesting, heap and linear-memory bytes, call/operand stacks, handles and queued jobs. Bound work during parsing, validation, compilation, linking, builtins, GC and execution, not only at loop backedges. Charge before work/allocation; use fallible reservation and explicit guest stacks where host recursion would be unbounded. Check cancellation at bounded intervals, including while draining microtasks. Termination for a host quota/interrupt is not a catchable JS exception that lets the guest resume. Host callbacks need their own deadlines. Backstop cooperative checks with OS memory/CPU limits and renderer teardown: safe Rust alone cannot promise recovery from allocator failure, stack overflow or a blocked host.
* **No ambient authority or confused handles.** The host issues scoped, revocable capabilities, not guest-selected paths, numeric addresses or caller-asserted identities. Verify isolate ownership at every entry, including async completions; drop/revoke on teardown or navigation. Root callback arguments explicitly, account callback work to its owner, define reentrancy rules and never unwind across an FFI/IPC boundary. No C ABI or cross-thread handles in this phase.
* **Small trusted base.** Start without third-party Rust dependencies; review a dependency's transitive code, unsafe blocks, build scripts, proc macros, licenses and advisories before adding it. Build and test locked. First-party lints do not audit the standard library, allocator, toolchain or dependencies.

### When unsafe Rust earns its place

Safe Rust is the default, not a promise that no `unsafe` will ever ship. Keep the current crates' `forbid(unsafe_code)`. When a real operation needs it, opt a narrow implementation crate into its own lint policy rather than loosening the workspace. No such crate is needed today.

Document **why**: name the operation, the safe alternative considered and why it fails (measure the difference for a performance claim). Expose a safe interface and spell out what it guarantees for *all* safe callers. At each unsafe operation, give the local `SAFETY` argument: who establishes each precondition, who preserves it, and for how long. Account for ownership, layout, aliasing, lifetimes, unwinding, thread and GC behavior as applicable.

Before merging unsafe code, an independent reviewer audits the implementation **and the paths that establish its assumptions**, including failure and reentry. Exercise the invariants with focused tests and Miri or sanitizers where applicable. Keep the reviewed revision, reviewer, evidence and unresolved assumptions with the component; re-audit affected code when its callers, layout, ABI or assumptions change. A green tool run is not a soundness argument, and an audit is not a formal proof.

## Execution tiers and performance

The reference interpreters stay available even if there is never a JIT. Collect reproducible optimized-build baselines before changing tiers: cold parse/validate time, startup/instantiation, steady-state throughput, peak resident and per-isolate memory, and worst observed interrupt/GC latency. Record hardware, OS, toolchain, workloads and revisions. Compare like policy/limits and check results against the reference; do not benchmark by removing checks or counting skipped work.

### JIT: prototype first; end-to-end correctness is the destination

An experimental backend can compile a small subset and run trusted fixtures in a standalone test process before it is proven. Keep it opt-in, keep the interpreter as an oracle, and publish the supported subset, mismatches and missing proof steps. Do not feed it untrusted programs or enable it in Keanu on the strength of differential tests.

The eventual requirement is a **machine-checked end-to-end refinement proof** for each shipped JIT target/subset. From a supported JS program or Wasm module, through decoding/validation and lowering, every optimization and register assignment, to the **final relocated bytes executed** under the target ISA/ABI: each observable native behavior must be allowed by the specified language and host policy. Include results and side effects, exceptions/traps, host calls, termination/interrupt behavior, and transitions between interpreter and compiled frames (deoptimization, stack maps and GC safepoints). A theorem starting at typed IR leaves the frontend as an open obligation until the two arguments compose. Neither “the interpreter agrees” nor “the native code is memory-safe” proves language correctness.

A proved compiler, a sound translation validator that checks each emitted artifact, or a composition may discharge these obligations. Record the theorem, covered features, replayable proof artifacts and trusted assumptions (e.g. ISA model, runtime primitives, OS and hardware). An unmodeled pass or patch after verification breaks the chain. For untrusted deployment require that chain and a reviewed publication/lifetime path: enforce W^X, no writable aliases to published code, safe invalidation, and no executable allocation when embedder policy forbids it. Unsupported, stale or rejected output is never entered; interpret the validated input or refuse. This is a correctness claim under stated assumptions, not a proof against every side channel.

## What “standards-compliant” will mean

* Use [ECMA-262, 16th edition (2025)](https://262.ecma-international.org/16.0/) as the first fixed semantic reference. Start with a named Script subset, not a claim of edition compliance. Modules, jobs/promises, Annex B web compatibility, ECMA-402 and host hooks need explicit coverage. Version or proposal changes are reviewed, not inherited from the tip of a test repository.
* Start the Wasm interpreter against the [WebAssembly Core 1.0 Recommendation](https://www.w3.org/TR/2019/REC-wasm-core-1-20191205/); it is a bounded first target, not the endpoint for a browser. Track Core features and [JS API](https://webassembly.github.io/spec/js-api/) separately. Later pin the matching upstream Wasm spec tests (including malformed/invalid, unlinkable and trap cases) and browser integration tests. Test262 is not that suite.
* Keep a feature matrix as features land: spec section/version, status, positive/negative tests and known deviations with rationale. An embedder's intentional policy denial is not a spec pass. Unknown/unimplemented features fail closed; do not substitute a default value or silently ignore required behavior.
* [Test262](test262.md) is pinned input, not executable build logic. Today we check corpus identity/layout only. A future runner will use isolated state per case/variant, parse frontmatter as data, constrain harness `includes` to the pinned harness tree, apply flags (strict/sloppy, module, raw, async), and check expected negative phase **and** error type. Restrict host hooks to test-only capabilities. With bounded time/memory, report pass, fail, unsupported/skip (with reason), crash and timeout separately at a named pin and feature selection. Never count a skip, a crash or an arbitrary rejection as a passing negative test.

## Delivery gates

| Stage | Evidence needed to leave the stage |
| --- | --- |
| 0 · foundation (this repo) | Locked workspace builds/tests without the corpus; upstream Test262 checkout matches the gitlink; no guest execution or native backend. |
| 1 · bounded front doors | Define limit/termination and host-policy contracts. Deliver a small JS Script grammar with source spans and an empty Wasm module decoder/validator. Test valid, malformed, unsupported, at-limit and over-limit inputs; fuzz both bounded entry points for panics and hangs. No execution on parse or validation. |
| 2 · standalone interpreters | First checked JS bytecode and Wasm instruction slices, explicit instantiation, safe roots/handles as needed, deterministic metering tests. Add real suite runners, feature matrix and reference/performance baselines for the slices actually supported. |
| 3 · browser embedder | Test denial of imports/dynamic codegen, revocation, interrupt and navigation/teardown during callbacks, job ordering, realm/isolate separation, bounded broker messages and external process termination. Add JS↔Wasm API and web-platform tests incrementally. No production security claim without sandbox integration and review. |
| 4 · optional acceleration | Demonstrate a working experimental tier separately from the proof milestone. Enable it for untrusted workloads only for the subset/target covered by the end-to-end argument above; otherwise remain interpreter-only. |

Resolve during stage 1: initial limit defaults, the smallest useful JS expression grammar, and the host cancellation API. Tune limits on the first renderer target rather than blocking a standalone prototype on browser measurements. GC algorithm, shared IR, JIT backend and browser IPC protocol are deliberately not selected in the scaffold.
