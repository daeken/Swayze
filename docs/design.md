# Swayze engine design — bootstrap draft

**Status:** requirements for implementation, not a statement of guarantees the scaffold already provides. The first deliverable is a standalone interpreter library; its first deployment model of interest is *inside an untrusted browser renderer*. There is no executable guest language yet.

Decision order: **security, then performance, then breadth of standards coverage**. A feature we cannot bound or isolate stays unsupported. A fast but observably wrong implementation does not count as supporting the feature. Proposals that expand authority, add native code, or weaken an invariant below require a written design change before implementation.

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
* **Memory ownership survives guest reentry.** Start in safe Rust with isolate-owned storage and checked handles (owner + lifetime/generation for reusable slots); no guest-controlled pointer, host reference or raw heap address as a JS/Wasm value. Design roots for frames, globals, jobs and scoped host handles before a collecting JS heap. Specify root/barrier invariants before introducing moving or generational GC. Do not hold a borrow, lock or cached bounds across a call that may allocate, collect, detach, resize, grow memory or run guest code. Reacquire and recheck afterward. Test stale handles, detached views, Wasm memory growth and reentrant host calls as those features land.
* **All operations have a cost.** The embedder sets finite ceilings for source/module bytes, nesting, heap and linear-memory bytes, call/operand stacks, handles and queued jobs. Bound work during parsing, validation, compilation, linking, builtins, GC and execution, not only at loop backedges. Charge before work/allocation; use fallible reservation and explicit guest stacks where host recursion would be unbounded. Check cancellation at bounded intervals, including while draining microtasks. Termination for a host quota/interrupt is not a catchable JS exception that lets the guest resume. Host callbacks need their own deadlines. Backstop cooperative checks with OS memory/CPU limits and renderer teardown: safe Rust alone cannot promise recovery from allocator failure, stack overflow or a blocked host.
* **No ambient authority or confused handles.** The host issues scoped, revocable capabilities, not guest-selected paths, numeric addresses or caller-asserted identities. Verify isolate ownership at every entry, including async completions; drop/revoke on teardown or navigation. Root callback arguments explicitly, account callback work to its owner, define reentrancy rules and never unwind across an FFI/IPC boundary. No C ABI or cross-thread handles in this phase.
* **Small trusted base.** First-party workspace crates forbid `unsafe_code`. This does not audit the standard library, allocator, toolchain or dependencies. Start without third-party Rust dependencies; review a dependency's transitive code, unsafe blocks, build scripts, proc macros, licenses and advisories before adding it. Build and test locked. An exception for unsafe code would need a separately reviewed, narrow crate with a safe API, written safety contract and invariant tests; not an `allow(unsafe_code)` in an interpreter hot loop.

## Execution tiers and performance

The reference interpreters stay available even if there is never a JIT. Collect reproducible optimized-build baselines before changing tiers: cold parse/validate time, startup/instantiation, steady-state throughput, peak resident and per-isolate memory, and worst observed interrupt/GC latency. Record hardware, OS, toolchain, workloads and revisions. Compare like policy/limits and check results against the reference; do not benchmark by removing checks or counting skipped work.

**JIT admission gate (future, default off):** a design must state the semantics preserved, safety properties checked, target ISA/ABI, trusted checker/decoder and threat model. Specify a restricted typed IR and verifier, then check the *final relocated native bytes* against the validated input and allowed control-flow, memory-access, call, stack-map and safepoint rules **before publication**. An IR type check alone says nothing about a miscompiled native load. Include deoptimization, host transitions, code lifetime and invalidation in the argument. Require a reviewable proof or translation-validation story for lowering; differential tests are useful evidence, not a proof. Enforce W^X with no writable alias of published code, isolate executable memory from guest data, and support a browser policy that forbids executable allocation altogether. On verification failure discard the output and interpret the validated input or refuse execution. No executable pages until the checker, platform policy, negative tests and independent security review exist.

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
| 4 · optional acceleration | Show a measured need, meet the JIT admission gate on one target, compare observable results and policy behavior with the interpreter. Ship interpreter-only on any unsupported/unreviewed target. |

Open before stage 1: numeric defaults for limits (measure stack and memory on the first renderer target), the smallest useful JS expression grammar, and the host cancellation API. GC algorithm, shared IR, JIT backend and browser IPC protocol are deliberately not selected in the scaffold.
