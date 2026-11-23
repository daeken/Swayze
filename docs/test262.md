# Test262 corpus

The official [TC39 Test262](https://github.com/tc39/test262) corpus is a git
submodule at `vendor/test262`. The superproject commit records its exact gitlink;
cloning the superproject does not by itself populate the corpus. Initialize it with:

```sh
git submodule update --init vendor/test262
```

Verify the checked-out revision and a few representative files without executing
corpus code:

```sh
sh tools/check-test262.sh
```

The check compares the submodule HEAD with the committed superproject gitlink,
checks representative paths, and rejects tracked changes. It does not inventory
untracked files. The default merge gate uses only this non-executing check.
Normal Cargo builds do not initialize, download, or execute the corpus.

To see the full pinned file inventory and which paths are selected (not which
passed), run `python3 tools/test262-inventory.py`. See the
[inventory guide](test262-inventory.md) for count rules and audit output.

## Run the reviewed slice

In a disposable, credential-free checkout of the reviewed commit with the
pinned submodule and Rust toolchain already installed, run:

```sh
sh tools/run-test262.sh
```

The opt-in wrapper builds and runs the Swayze example in debug and release.
The example takes no arbitrary case paths, reads its committed allowlist and
selected blobs from Git objects, and checks the exact Test262 pin. It evaluates
these cases only through Swayze; no Test262 harness JS, includes, Node.js or
external guest engine runs. Run with network disabled and access limited to the
checkout and necessary tools. The wrapper itself is not a sandbox. Use
`sh tools/run-test262.sh --self-test` for synthetic adapter checks without
running corpus cases.

An **adapted** case is a reviewed single final `assert.sameValue` call in a
comment-only surrounding source. The adapter evaluates its parenthesized actual
expression in Swayze and compares it with a separately evaluated primitive
literal expectation. It compares exact value tags and UTF-16 code units,
distinguishes `+0` and `-0`, and treats any two NaNs as the same value. It is not
a general assertion rewriter. **Raw** mode is reserved for reviewed positive
`raw` cases run unchanged. The required hashbang comment case is the first
unmodified pass: Swayze parses the original bytes and executes the resulting
Script to normal completion, without a harness or injected value. The summary
counts raw and adapted passes separately. Strict variants are reported as not
run, never as passes. No negative, module or harness-heavy
cases are credited.

The required adapted sloppy-mode cases cover numeric separators in integer,
fraction and exponent positions; U+2028, U+2029 and U+180E in Strings; unary
positive and negative zero; `typeof x` for an unbound name; and two cooked
no-substitution template literals, including U+180E; and three cooked template
cases with nested substitutions. The template cases were first reproduced as
unsupported and then promoted to required after the fix. A required variant becoming
unsupported, a wrong result, or no required passes exits 1; infrastructure
errors exit 2. An unsupported frontier case alone does not fail the run.
Review any additions to [`tools/test262-cases.tsv`](../tools/test262-cases.tsv)
against the full pinned blob. Reproduce a frontier result before changing the
engine, rerun the same blob after the fix, then promote the case to required.
This is regression evidence for a small subset, not a Test262 conformance score.

## Update the pin

To update intentionally, fetch the official upstream in the submodule, check out a
chosen commit, review its changes and license, and stage the new gitlink in the
superproject. Commit the gitlink update only after review; rerun the smoke check
once the superproject commit records it. For example:

```sh
git -C vendor/test262 fetch origin
git -C vendor/test262 checkout --detach <reviewed-commit>
git -C vendor/test262 diff <old-commit> <reviewed-commit> --stat
git add vendor/test262
```

Test262 is separately licensed; see `vendor/test262/LICENSE` for its terms. Treat
the submodule as third-party, untrusted source: neither initialization nor the
smoke check runs its scripts or test files. Review any tooling and content before
choosing to execute it in an appropriately isolated environment.
