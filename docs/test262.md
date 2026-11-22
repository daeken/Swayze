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
untracked files. Use a fresh checkout for CI; a future runner should discover
cases from the pinned Git tree rather than a filesystem glob. This is a corpus
smoke check, not a Test262 runner or a conformance/pass claim. Normal Cargo builds
do not initialize, download, or execute the corpus.

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
