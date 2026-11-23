#!/usr/bin/env python3
"""Offline Test262 path inventory; does not execute or validate tests."""

import argparse
from io import BytesIO
from collections import Counter, defaultdict
from pathlib import Path
import re
import subprocess
import sys


SUBMODULE = 'vendor/test262'
MANIFEST = 'tools/test262-cases.tsv'
GROUP_LIMIT = 15
TREE_OUTPUT_LIMIT = 16 * 1024 * 1024
SMALL_OUTPUT_LIMIT = 32 * 1024
REGULAR_MODES = {'100644', '100755'}
# The pinned blob has Test262 frontmatter and assertions but no `.js` suffix.
REVIEWED_TESTS = {
    '2a1ade303f72d3de80565c921d685222405c32ce': {
        'test/language/statements/using/'
        'initializer-disposed-at-end-of-each-iteration-of-forofstatementjs',
    },
}
TEST_KINDS = {'js-test', 'reviewed-test'}
AUDIT_KINDS = {'js-fixture', 'other-fixture', 'unknown', 'reviewed-test'}
ROLES = ('required', 'frontier')
MODE_FIELDS = {'adapted': (6, 7), 'raw': (3,), 'raw-negative': (3,)}


def bounded_read(stream, max_bytes):
    data = stream.read(max_bytes + 1)
    if len(data) > max_bytes:
        raise ValueError(f'Git output exceeded {max_bytes} bytes')
    return data


def git(repo, *args, max_bytes=SMALL_OUTPUT_LIMIT):
    # One pipe avoids a blocking stderr pipe and keeps even failure output bounded.
    with subprocess.Popen(
        ['git', '--no-replace-objects', '--no-lazy-fetch', '--literal-pathspecs',
         '-C', str(repo), *args],
        env={'PATH': '/usr/bin:/bin', 'LC_ALL': 'C', 'GIT_CONFIG_NOSYSTEM': '1',
             'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_ALLOW_PROTOCOL': ''},
        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
    ) as process:
        try:
            data = bounded_read(process.stdout, max_bytes)
        except BaseException:
            process.kill()
            raise
        if process.wait() != 0:
            detail = ascii(data[:1024].decode('utf-8', errors='replace').strip())
            raise ValueError(f'git {args[0]} in {repo}: {detail}')
    return data


def tree_entries(data):
    # -z avoids Git quoting and preserves tabs/newlines/non-UTF-8 in paths.
    if data and not data.endswith(b'\0'):
        raise ValueError('unterminated Git tree listing')
    for row in data.split(b'\0')[:-1]:
        header, path = row.split(b'\t', 1)
        mode, kind, oid = header.decode('ascii').split(' ')
        yield mode, kind, oid, path.decode('utf-8', errors='surrogateescape')


def inventory(data, reviewed):
    paths = {}
    for mode, kind, _, path in tree_entries(data):
        if not path.startswith('test/') or path in paths:
            raise ValueError(f'unexpected or duplicate inventory path: {ascii(path)}')
        name = path.rsplit('/', 1)[-1]
        if mode not in REGULAR_MODES or kind != 'blob':
            category = 'unknown'
        elif '_FIXTURE' in name:
            category = 'js-fixture' if name.endswith('.js') else 'other-fixture'
        elif name.endswith('.js'):
            category = 'js-test'
        elif path in reviewed:
            category = 'reviewed-test'
        else:
            category = 'unknown'
        paths[path] = category
    if not paths or any(paths.get(p) != 'reviewed-test' for p in reviewed):
        raise ValueError('empty inventory or stale reviewed filename exception')
    return paths


def manifest_paths(text, paths):
    """Reconcile path and selection labels, not assertion spans or eligibility."""
    selected = {}
    for line_no, line in enumerate(text.split('\n'), 1):
        line = line.removesuffix('\r')
        if not line or line.startswith('#'):
            continue
        fields = line.split('\t')
        if len(fields) < 3:
            raise ValueError(f'manifest line {line_no}: expected role, mode and path')
        role, mode, path = fields[:3]
        if role not in ROLES or len(fields) not in MODE_FIELDS.get(mode, ()):
            raise ValueError(f'manifest line {line_no}: unexpected role, mode or fields')
        # Match the current runner's path form; the inventory can be wider.
        if (len(path) > 240 or not path.startswith('test/') or not path.endswith('.js')
                or any(part in ('.', '..') or not re.fullmatch(r'[a-zA-Z0-9._-]+', part)
                       for part in path.split('/'))):
            raise ValueError(f'manifest line {line_no}: invalid path {ascii(path)}')
        if path in selected:
            raise ValueError(f'manifest line {line_no}: duplicate path {ascii(path)}')
        category = paths.get(path, 'absent from pinned test/')
        if category not in TEST_KINDS:
            raise ValueError(f'manifest line {line_no}: {ascii(path)} is {category}')
        selected[path] = (role, mode)
    return selected


def load_inventory(root):
    commit = git(root, 'rev-parse', '--verify', 'HEAD').strip().decode('ascii')
    entries = list(tree_entries(git(root, 'ls-tree', '-z', commit, '--', SUBMODULE)))
    if len(entries) != 1 or entries[0][:2] != ('160000', 'commit'):
        raise ValueError(f'{SUBMODULE} is not a gitlink at superproject HEAD')
    pin = entries[0][2]
    repo = root / SUBMODULE
    if not (repo / '.git').exists():
        raise ValueError(f'{SUBMODULE} is not initialized; the pinned objects must be available locally')
    paths = inventory(git(repo, 'ls-tree', '-rz', pin, '--', 'test/',
                          max_bytes=TREE_OUTPUT_LIMIT),
                      REVIEWED_TESTS.get(pin, set()))
    return commit, pin, paths


def load_manifest(root, commit, paths):
    manifest = git(root, 'cat-file', 'blob', f'{commit}:{MANIFEST}').decode('utf-8')
    return manifest_paths(manifest, paths)


def summarize(paths, selected, prefix):
    counts = Counter()
    selections = Counter()
    groups = defaultdict(Counter)
    for path, category in paths.items():
        if not path.startswith(prefix + '/'):
            continue
        remainder = path[len(prefix) + 1:]
        directory, slash, _ = remainder.partition('/')
        group = directory + slash if slash else ''
        counts[category] += 1
        groups[group][category] += 1
        if path in selected:
            selections[selected[path]] += 1
            groups[group]['selected'] += 1
    if not counts:
        raise ValueError(f'no pinned entries under directory {ascii(prefix)}')
    return counts, selections, groups


def test_count(counts):
    return sum(counts[kind] for kind in TEST_KINDS)


def report(paths, selected, prefix, audit):
    counts, selections, groups = summarize(paths, selected or {}, prefix)
    tests = test_count(counts)
    yield f'Directory: {ascii(prefix)}'
    yield (f'Inventory: {sum(counts.values())} tree entries, {tests} standalone test paths '
           f'({counts["js-test"]} .js, {counts["reviewed-test"]} reviewed filename exceptions)')
    yield (f'Other entries: {counts["js-fixture"] + counts["other-fixture"]} fixtures '
           f'({counts["js-fixture"]} .js, {counts["other-fixture"]} other), '
           f'{counts["unknown"]} unknown')
    if selected is None:
        yield 'Manifest: unavailable; selection counts withheld'
    else:
        chosen = sum(selections.values())
        yield f'Manifest: {chosen} selected test paths, {tests - chosen} not selected (not runtime outcomes)'
        for mode in MODE_FIELDS:
            required, frontier = (selections[role, mode] for role in ROLES)
            yield f'  {mode}: {required + frontier} selected ({required} required, {frontier} frontier)'

    yield 'Next-level groups (largest by test-path count; use --prefix to drill down):'
    columns = '  selected  not-selected' if selected is not None else ''
    yield f'  tests{columns}  fixtures  unknown  group'
    ranked = sorted(groups.items(), key=lambda pair: (-test_count(pair[1]), pair[0]))

    def row(name, group):
        tests = test_count(group)
        fixtures = group['js-fixture'] + group['other-fixture']
        columns = '' if selected is None else f'  {group["selected"]:8}  {tests - group["selected"]:12}'
        return f'{tests:7}{columns}  {fixtures:8}  {group["unknown"]:7}  {name}'

    for name, group in ranked[:GROUP_LIMIT]:
        yield row(ascii(name) if name else '(direct files)', group)
    if len(ranked) > GROUP_LIMIT:
        others = sum((group for _, group in ranked[GROUP_LIMIT:]), Counter())
        yield row(f'({len(ranked) - GROUP_LIMIT} other groups combined)', others)
    if audit:
        yield 'Audit (all fixtures, unknown entries and reviewed filename exceptions in this directory):'
        for path in sorted(paths):
            category = paths[path]
            if path.startswith(prefix + '/') and category in AUDIT_KINDS:
                yield f'  {category}: {ascii(path)}'


def self_test():
    """Fixed synthetic Git records and manifest bytes; requires no Git checkout."""
    def entry(path, mode='100644', kind='blob'):
        return f'{mode} {kind} {"e" * 40}\t'.encode() + path + b'\0'

    exception = 'test/a/exceptionjs'
    tree = b''.join((
        entry(b'test/a/one.js'), entry(b'test/a/_FIXTURE_dir/two.js', '100755'),
        entry(exception.encode()), entry(b'test/a/dep_FIXTURE-extra.js'),
        entry(b'test/a/asset_FIXTURE'), entry(b'test/a/odd\t\n\xff.txt'),
        entry(b'test/a/link.js', '120000'), entry(b'test/a/external', '160000', 'commit'),
        entry(b'test/a2/other.js'), entry(b'test/direct.js'),
    ))
    paths = inventory(tree, {exception})
    selected = manifest_paths('# comment\r\n\r\nrequired\tadapted\ttest/a/one.js\t0:5\t0:1\t2:3\r\n'
                              'frontier\traw\ttest/direct.js\n'
                              'frontier\traw-negative\ttest/a2/other.js\n', paths)
    counts, selections, groups = summarize(paths, selected, 'test')
    assert counts == {'js-test': 4, 'reviewed-test': 1, 'js-fixture': 1,
                      'other-fixture': 1, 'unknown': 3}
    assert selections == {('required', 'adapted'): 1, ('frontier', 'raw'): 1,
                          ('frontier', 'raw-negative'): 1}
    assert '  raw-negative: 1 selected (0 required, 1 frontier)' in report(paths, selected, 'test', False)
    assert set(groups) == {'a/', 'a2/', ''} and groups['a/']['selected'] == 1
    local, choices, _ = summarize(paths, selected, 'test/a')
    assert test_count(local) == 3 and sum(choices.values()) == 1
    assert paths[exception] == 'reviewed-test' and inventory(tree, set())[exception] == 'unknown'
    output = '\n'.join(report(paths, selected, 'test/a', True))
    assert '3 standalone test paths (2 .js, 1 reviewed filename exceptions)' in output
    assert '1 selected test paths, 2 not selected (not runtime outcomes)' in output
    assert "'test/a/odd\\t\\n\\udcff.txt'" in output and 'test/a2/' not in output
    unavailable = '\n'.join(report(paths, None, 'test/a', True))
    assert 'Manifest: unavailable; selection counts withheld' in unavailable
    assert 'unknown: \'test/a/link.js\'' in unavailable
    assert 'js-fixture: \'test/a/dep_FIXTURE-extra.js\'' in unavailable
    assert 'selected' not in unavailable
    many = {f'test/{i:02}/case.js': 'js-test' for i in range(GROUP_LIMIT + 2)}
    rows = list(report(many, {}, 'test', False))
    assert rows[-1].endswith('(2 other groups combined)')
    assert rows[-2].endswith(f"'{GROUP_LIMIT - 1:02}/'") and not any('Audit' in row for row in rows)

    def rejects(message, operation, *args):
        try:
            operation(*args)
        except ValueError as exc:
            assert message in str(exc), str(exc)
        else:
            raise AssertionError(f'expected error containing {message!r}')

    raw = 'required\traw\ttest/a/one.js\n'
    rejects('duplicate', manifest_paths, raw + raw, paths)
    rejects('absent', manifest_paths, 'required\traw\ttest/missing.js', paths)
    rejects('js-fixture', manifest_paths, 'required\traw\ttest/a/dep_FIXTURE-extra.js', paths)
    rejects('unknown', manifest_paths, 'required\traw\ttest/a/link.js', paths)
    rejects('invalid path', manifest_paths, 'required\traw\ttest/a/../direct.js', paths)
    rejects('unexpected role', manifest_paths, raw.replace('raw', 'other'), paths)
    rejects('unexpected role', manifest_paths, raw.replace('raw', 'adapted'), paths)
    negative = raw.replace('raw', 'raw-negative')
    rejects('unexpected role', manifest_paths, negative.rstrip('\n') + '\t0:1', paths)
    rejects('duplicate', manifest_paths, raw + negative, paths)
    rejects('unterminated', inventory, tree[:-1], set())
    rejects('unexpected', inventory, entry(b'harness/assert.js'), set())
    rejects('duplicate', inventory, tree + tree, set())
    rejects('stale', inventory, tree, {'test/missing'})
    rejects('no pinned entries', summarize, paths, selected, 'test/absent')
    assert bounded_read(BytesIO(b'ab'), 2) == b'ab'
    rejects('exceeded', bounded_read, BytesIO(b'abc'), 2)
    print('Test262 inventory synthetic self-test passed')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prefix', default='test', metavar='DIR', help='directory under test/ (default: test)')
    parser.add_argument('--audit', action='store_true', help='list every exclusion and reviewed exception in scope')
    parser.add_argument('--self-test', action='store_true', help='run synthetic checks only')
    args = parser.parse_args()
    prefix = args.prefix.rstrip('/')
    if args.self_test:
        if args.audit or args.prefix != 'test' or not __debug__:
            parser.error('--self-test requires assertions and no report options')
        self_test()
        return 0
    parts = prefix.split('/')
    if parts[0] != 'test' or any(part in ('', '.', '..') for part in parts):
        parser.error('--prefix must be a directory under test/')
    root = Path(__file__).resolve().parent.parent
    manifest_error = None
    try:
        commit, pin, paths = load_inventory(root)
        try:
            selected = load_manifest(root, commit, paths)
        except (ValueError, OSError) as exc:
            selected = None
            manifest_error = exc
        lines = list(report(paths, selected, prefix, args.audit))
    except (ValueError, OSError) as exc:
        print(f'Test262 inventory: {exc}', file=sys.stderr)
        return 2
    print(f'Test262 pin: {pin}\nSuperproject commit: {commit}')
    print('\n'.join(lines))
    if manifest_error is not None:
        print(f'Test262 inventory: {manifest_error}', file=sys.stderr)
        return 2
    if 'unknown' in paths.values():
        print('Test262 inventory: unclassified pinned paths; audit with --audit', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
