# Who keeps a value alive? Swayze ↔ Keanu

**Proposed embedding contract; not implemented or proven.** This describes what each side must be able to tell the other, not how Keanu currently allocates DOM objects. The first interpreter can use a bounded, non-moving arena reclaimed on isolate teardown. It need not collect to evaluate its first expression. It **does** need to keep addresses out of the embedding API so that a later collector can move objects.

## Storage, identity and roots

| Storage owner | Owns and releases | Does not own |
| --- | --- | --- |
| Swayze isolate | JS objects (including host wrappers), VM frames, handle/root tables and the JS references held by Wasm instances | The DOM node or browser resource named by a wrapper |
| Keanu | Native objects, listeners, pending browser operations and their lifetimes; revocation on navigation or shutdown | JS object storage or a stable pointer into the isolate heap |

Storage ownership answers **who can free memory**. Reachability answers **when it can be freed**. A strong edge keeps its target live *if its source is live*; it is not necessarily a root. Neither a live wrapper nor a GC root grants authority to use a revoked browser resource.

* **Inside Swayze:** trace from embedder-retained realms/globals, active frames and VM temporaries, queued jobs and registered host roots. Each GC-managed type enumerates its strong fields: for example, a reachable generator traces its suspended frame. A suspended frame is not automatically a permanent root. Wasm reference-valued locals, tables and globals participate; arbitrary linear-memory bytes are not scanned for pointers.
* **During a call:** a handle scope roots arguments and results until scope exit or transfer to the caller's scope. Root a new value before the next allocation or callback that could collect. Across reentry, keep handles, not borrows or interior pointers. Resolve the handle again on return. A moving collector can update one handle-table slot without changing identity for Keanu.
* **Across calls:** for a genuinely external owner (e.g. a queued browser task that must deliver one callback), Keanu explicitly promotes a scoped handle to a persistent root. Register it against the owning isolate and task/document lifetime; release on completion, cancellation or teardown. A native object's *field* pointing to JS is normally a **traced edge**, not a persistent root. A weak identity cache keeps neither side alive; upgrading a weak handle either obtains a scoped strong root at a safepoint or reports that the target is gone.
* **Resolution:** tokens identify owner/isolate, slot and generation; reject a dead owner, wrong owner or stale slot without dereferencing it. Never recycle an identity on counter wrap. Only the isolate's owner thread resolves JS handles. A worker/IPC message carries data or a revocable operation ID, not a dereferenceable handle.

## A cycle across the browser boundary

Consider `JS wrapper → native node → JS event listener → JS wrapper`. While the document retains the node, the listener must survive GC. Once the node is detached *and* the last external JS/native root is gone, the cycle alone must not keep either side alive. Making the listener a permanent root leaks the entire cycle; making it unconditionally weak loses live listeners.

The integration target is **one reachability decision, not necessarily one allocator**. At a coordinated safepoint, Keanu supplies its externally reachable roots and enumerates native-to-JS and native-to-native strong edges; Swayze enumerates its roots, JS fields and wrapper-to-native edges. Starting from both root sets, follow strong edges across the boundary until neither side finds a new object. Only then clear weak entries and let each owner reclaim its own unreachable storage. A root from another document, isolate or browser operation must be accounted for before freeing a shared native object. Tracing must not allocate guest objects, run script or mutate the graph. An incomplete trace is not permission to sweep. If objects later move, the owner must update every reference or indirection affected. If marking becomes incremental, account for mutations on either side (including listener installation during marking) before resuming collection.

Keanu's mechanism for reporting roots and edges is an integration decision. We don't need a joint collector to start: a prototype may conservatively retain a **bounded** set of bridge objects until explicit teardown. Record that retention and its limit as a known gap; do not offer general long-lived DOM bindings on that basis. Don't export raw pointers or turn all wrapper edges into public permanent roots to make the prototype work.

## Teardown is explicit, not a finalizer

On document/worker teardown, stop accepting work for that owner, cancel its queued operations, revoke native bindings and unregister its roots on the isolate thread. Late completions must fail their owner/epoch check even if a new document reused the same slot. On isolate destruction invalidate its handle table before freeing heap storage; later drops of invalid tokens must not touch the freed isolate. Keanu closes resources explicitly. Collector cleanup must not run arbitrary host or guest callbacks during tracing/sweep; JS finalization, if supported later, is separately scheduled and is never relied on for revocation.

**Evidence as features land:** force a collection at an allocation inside a reentrant callback and use its scoped argument afterward; keep a listener alive via a live native node, then detach it, drop the last roots and collect the cycle; deliver a late task after teardown and slot reuse. A teardown-only arena can test invalidation now, but must not claim to pass the cycle-reclamation case.
