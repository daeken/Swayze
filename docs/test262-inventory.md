# Pinned Test262 inventory

This development-only report measures the corpus in **standalone test paths**
and reconciles those paths with the reviewed runner manifest. It does not build
or run an engine, execute any upstream scripts, fetch objects, or participate in
the default gate.

```sh
python3 tools/test262-inventory.py
python3 tools/test262-inventory.py --prefix test/language/expressions
python3 tools/test262-inventory.py --audit
python3 tools/test262-inventory.py --self-test
```

Use Python 3.9+ (standard library only) and the same system Git used by the
runner. Reports require a local `vendor/test262` repository with the pinned
Git objects available. The tool resolves the superproject's `HEAD` once, reads
the `vendor/test262` gitlink and `tools/test262-cases.tsv` from that commit,
and lists `test/` from the recorded Test262 commit. Staged edits, untracked
files, and submodule working-tree content or HEAD do not affect the report.
The synthetic self-test does not require Git or the corpus.

## What the denominator counts

Only entries under the pinned `test/` tree are inventoried. Root `harness/`,
`tools/`, and all other top-level paths are out of scope; `test/harness/` is
in scope because it contains harness **self-tests**. No feature, proposal,
staging, manifest role, or engine support filter is applied.

- A regular `.js` blob with no `_FIXTURE` in its basename counts once as a
  standalone test path. This is a path inventory, not a frontmatter validator.
- A regular blob with the case-sensitive substring `_FIXTURE` anywhere in its
  basename is a fixture, independent of extension. This follows the pinned
  `INTERPRETING.md` rule: such files may be needed to run tests, but aren't
  standalone tests. It does not exclude a file based on a directory name.
- The pinned tree has one reviewed filename exception:
  `test/language/statements/using/initializer-disposed-at-end-of-each-iteration-of-forofstatementjs`.
  It lacks the `.js` suffix but has Test262 frontmatter (`explicit-resource-management`)
  and three `assert.sameValue` checks. It counts as a test path, not as a
  conventional `.js` discovery result. The exception is scoped to that exact pin;
  recheck it on a pin update.
- Any other entry (e.g. an unexplained non-`.js` blob, a symlink, a nested
  gitlink) is **unknown**, not assumed to be a fixture or a test. Investigate
  unknowns before claiming complete accounting. `--audit` lists every fixture,
  unknown, and reviewed exception in scope with its escaped path and category.

At Test262 `2a1ade303f72d3de80565c921d685222405c32ce`, `test/` contains
**54,115 entries** = **53,802 standalone test paths** (53,801 conventional
`.js` and one reviewed exception) + **313 fixtures** (294 `.js` and 19 other)
+ **0 unknown**.

| Directory under `test/` | Standalone test paths | Fixtures |
| --- | ---: | ---: |
| `annexB/` | 1,086 | 0 |
| `built-ins/` | 23,885 | 3 |
| `harness/` | 117 | 0 |
| `intl402/` | 3,394 | 0 |
| `language/` | 23,833 | 302 |
| `staging/` | 1,487 | 8 |

Use `--prefix` to compare subtrees when choosing the next work. The default
report shows up to 15 next-level groups (including a group for files directly
in the requested directory), ranked by test-path count with name ties broken
deterministically. Smaller groups are combined in a single remainder row;
counts still account for the full directory. The opt-in audit prints paths in
sorted order and is not subject to this group limit.

## How to read manifest coverage

**Selected** means the committed TSV names that standalone path, not that it
ran, passed, or is runnable. **Not selected** is the rest of the standalone
paths in the requested directory, not an unsupported or failed count. The
report breaks selections down by `adapted`/`raw` and `required`/`frontier`. It
fails on malformed path rows, duplicate paths, or membership in fixtures,
unknowns, or paths missing from the pin. It does not validate assertion spans
or predict runner eligibility. Even for a narrow prefix, it reconciles the
whole committed manifest before reporting that subtree's counts.

In the initial snapshot all 11 selections are required, adapted paths under
`test/language/`; 53,791 standalone paths are not selected. Separately, the
existing opt-in runner's baseline is 11 adapted sloppy-mode passes and 11
strict not run per profile. Adapted evidence is **not** an official Test262
pass. A `raw` selection would also not itself constitute any kind of pass.
This inventory emits no variant total: strict/sloppy, module, raw, and other
execution requirements depend on metadata and harness behavior rather than
path counts. A conformance tally needs a runner that records actual outcomes
under those conditions.
