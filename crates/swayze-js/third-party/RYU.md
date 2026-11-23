# Third-party notice: Ryū (binary64 digit generation)

The digit algorithm in `src/number_format/ryu.rs` and multiplier data in
`src/number_format/tables.rs` are adapted from [dtolnay/ryu] as published on
crates.io: **`ryu` 1.0.23**, VCS revision
`f0b52bb194befe6fd242154f2182fafd43a819b8` (from `.cargo_vcs_info.json`),
archive SHA-256
`9774ba4a74de5f7b1c1451ed6cd5285a32eddb5cccb8cc655a4e50009e06477f`.
The Rust source files are `src/d2s.rs`, `src/d2s_intrinsics.rs`,
`src/common.rs`, and `src/d2s_full_table.rs`.

The Rust package credits Ulf Adams's [original C Ryū] implementation, pinned
in the package README to `e3e090c66efd258a0cee50ee93411ec99668d416`.
The corresponding C sources are `ryu/d2s.c`, `ryu/d2s_intrinsics.h`,
`ryu/common.h`, and `ryu/d2s_full_table.h`. The notice below applies to **both
the algorithm source and the multiplier data**:

> Copyright 2018 Ulf Adams

We select the **Boost Software License 1.0** option offered by the upstream
sources. Its full text is in [LICENSE-BOOST](LICENSE-BOOST), copied from the
Rust package. This notice and that license accompany the adapted material;
the MIT license in the repository root still applies to Swayze's own code.
Neither a Cargo dependency nor upstream's separate float renderer is used.

[dtolnay/ryu]: https://github.com/dtolnay/ryu/tree/f0b52bb194befe6fd242154f2182fafd43a819b8
[original C Ryū]: https://github.com/ulfjack/ryu/tree/e3e090c66efd258a0cee50ee93411ec99668d416

## Adaptation and bounds

- **Binary endpoints.** For a finite nonzero magnitude, two extra bits give
  the exact midpoint numerators: center `4*m2`, upper `4*m2+2`, lower
  `4*m2-1-mm_shift`. The lower gap is half-size only at a normal power of two
  above the smallest normal, where `mm_shift` is zero. A midpoint belongs to
  the input only if its binary significand is even. The upper-endpoint and
  trailing-zero handling from Ryū preserve that distinction after scaling;
  the lower-endpoint loop runs only when the lower endpoint is available.
- **Shortest, closest, and decimal halfway.** The digit core retains Ryū's
  interval tests, divisibility conditions, and digit-removal branches.
  Safe tuple returns replace out-pointers, `%` replaces the modular remainder
  idioms, and bounded division by five and `trailing_zeros` are equivalent
  divisibility tests for the nonzero interval numerators here. In particular,
  an exact discarded `5` followed only by zeros is not a generic round-up:
  Ryū's explicit tie branch leaves an even retained decimal significand
  unchanged, or rounds an odd one up. This decimal tie is distinct from
  binary midpoint ownership. The binary64 shortest-digit bound is 17.
- **Table indices.** Raw exponents 1 through 2046 map to `e2 = exponent -
  1077`, and subnormals share `e2 = -1076` with the first normal bin. For
  `0 <= e2 <= 969`, the inverse key is
  `floor(78913*e2 / 2^18) - [e2 > 3]`. This increases by zero or one at each
  step (including 3 to 4, where the floor increases by one and the correction
  starts). Its endpoints are 0 and 290. For `t = -e2` in 1 through 1076,
  the direct key is `t - floor(732923*t / 2^20) + [t > 1]`, also increasing by
  zero or one (including 1 to 2, where the floor increases by one). Its
  endpoints are 1 and 325. Thus all and only inverse powers 0–290 and direct
  powers 1–325 are retained; the direct index is the key minus one. No
  assumption about the fraction is needed for the lookups.
- **Exact table checks and arithmetic bounds.** Cargo tests regenerate
  every retained pair from exact `P = 5^i` and its bit length `b`: direct is
  `floor(P * 2^(125-b))`, inverse is `floor(2^(124+b) / P) + 1` (including
  the `+1` at `i = 0`). The tests also compare against exact decimal logs,
  assert the shift ranges (inverse 118–125, direct 118–122), and visit every
  retained index across the exponent domain. These finite checks are
  independent of the float-to-decimal format tests. The core's product and
  digit-loop bounds are documented beside the corresponding operations.
- **Renderer.** The in-crate renderer handles both zeros, both infinities,
  and all NaN payloads directly from the raw bits. For positive magnitude it
  renders `k` shortest integer digits with decimal-point position `n = k +
  power`. The fixed branches use `n <= 21` and `n > -6` as specified by
  ECMA-262; the other branches emit exponent `n - 1` with a sign. The valid
  interval lies between half the smallest subnormal (greater than `10^-324`)
  and the upper midpoint of the largest finite number (less than `10^309`),
  so at most three exponent digits are needed. The longest branch is small
  fixed notation: sign, `0.`, up to five zeros, and up to 17 digits, totaling
  25 UTF-16 code units. The caller supplies that fixed stack buffer; there is
  no rendering allocation or float `Display` conversion.

Canonical, boundary, round-trip, and independent differential fixtures are
regression evidence, not a proof of the adapted implementation. At this
increment everything is private to the crate's test build, with no JS
operator, parser, value representation, or host API change.

## Optional differential check

The independent `tools/number-format-oracle.mjs` lane supplies a fixed
synthetic corpus checked against Node. From a checkout with that tool, run
from the repository root:

```sh
fixtures=$(mktemp)
trap 'rm -f "$fixtures"' EXIT
node tools/number-format-oracle.mjs --tsv > "$fixtures" && \
  SWAYZE_NUMBER_FORMAT_TSV="$fixtures" cargo test -p swayze-js --lib --frozen \
  number_format::tests::node_oracle_tsv -- --ignored
```

The file contains lowercase 16-digit raw hex bits, a tab, and the ASCII
expected spelling on each line. The fixture reader is ignored by default;
the pinned Cargo gate neither uses Node nor requires generated files.
