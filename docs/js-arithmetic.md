# JavaScript arithmetic reference evaluator

`swayze_js::parse_script` validates source in the supported arithmetic subset.
Call `Script::evaluate(&self) -> Result<f64, EvalError>` on the resulting
`Script` if a Number result is required. For example,
`swayze_js::parse_script("2 + 3 * 4")?.evaluate()?` produces `14.0`.
`Script::evaluate_value()` returns `Value::Number`, `Value::Boolean`,
`Value::Null`, or `Value::Undefined` without coercion. `evaluate()` returns
`NonNumericResult` for any non-Number. An unbound read of `undefined` returns
the default value, but a block-local declaration of that name takes precedence.
A top-level declaration of `undefined` is malformed in a classic script.

Number literals include decimal fractions and signed exponents, plus binary
(`0b`), octal (`0o`), and hexadecimal (`0x`) integers with either case of prefix.
Explicit-radix literals round to binary64 with ties to even, including at the
overflow boundary. A numeric separator (`_`) is allowed only between digits
of the same component and base. Separators count toward the 4,096-byte cap
for decimal fractions and exponents. Decimal conversion strips separators
with a metered, fallible pass; explicit-radix conversion skips them while
scanning. Legacy octal and BigInt remain outside the subset. Arithmetic includes binary `+`, `-`, `*`, `/`, and `%`, plus prefix
`+` and `-` and parenthesized grouping.
The parser determines precedence and associativity. `const` and `let`
declarations initialize slots in source order, including left to right within
comma-separated lists. Each `const` item requires an initializer. A `let`
item without an initializer initializes to `undefined` each time it executes,
without reading any binding named `undefined`. Comma expressions and trailing
commas in declaration lists are unsupported.
Simple and compound assignments update earlier `let` slots. Arithmetic and
bitwise compound assignments read the target once, evaluate the right operand,
apply the corresponding operator, and store the result. Logical assignments
(`&&=`, `||=`, `??=`) read the target once and may skip both the right operand
and the store, using the same tests as `&&`, `||`, and `??`. Skipped operands
are still checked during parsing, and `const` targets are rejected even when
no store would run. Prefix and postfix `++` and `--` convert an initialized
`let` value to Number, add or subtract one, and store the result. Postfix
forms cannot cross a line terminator between the name and operator, including
a line terminator in a comment. Assignments and updates do not produce
expression values in this subset.
Declaration, assignment, and update statements require an explicit semicolon; the
final expression's semicolon is optional. Unavailable bindings
and writes to `const` are rejected as unsupported rather than modeling global
lookup, temporal dead zone behavior or a runtime `TypeError`. There are no
calls or objects.

Arithmetic and relational comparisons (`<`, `<=`, `>`, `>=`) convert `true`
to `1`, `false` and `null` to `+0`, and `undefined` to NaN. Every relational
comparison involving NaN is false. Strict equality (`===`, `!==`) compares
types without coercion: signed zeros compare equal, while NaN is unequal to
itself. Loose equality (`==`, `!=`) uses the same comparison for matching
types, equates `null` with `undefined`, and converts Booleans to Numbers
when compared to a Number. Other mixed types are unequal in this subset.
Comparisons return Booleans.

Bitwise operations (`~`, `&`, `^`, `|`, `<<`, `>>`, `>>>`) convert operands to
32-bit integers by truncating and reducing modulo 2³². Nonfinite values and
signed zeros become zero. Shifts use the low five bits of the count. Results
are signed 32-bit integers represented as Numbers, except `>>>`, which returns
an unsigned result. From tighter to looser, binary precedence is arithmetic,
shifts, relational comparisons, equality (loose and strict), `&`, `^`, `|`, then logical
operators.

Conditional expressions (`test ? consequent : alternate`) evaluate only the
selected arm, preserving its value type. Boolean tests use their own value.
For numeric tests, either signed zero and NaN are false; other numbers are
true. `null` and `undefined` are also false in tests. The same truthiness drives
`!`, `&&`, and `||`. `!` returns a Boolean;
`&&` and `||` short-circuit and return the selected operand without coercion.
`&&` binds tighter than `||`, and both bind tighter than the right-associative `?:`.
`??` evaluates its right operand only when the left is `null` or `undefined`.
It is left-associative, binds between bitwise OR and `?:`, and cannot mix with
`&&` or `||` without parentheses at the same expression level. All operands
and arms must use supported syntax and bindings.

Standalone blocks, empty statements (`;`), and braced `if`, `while`,
`do…while`, and `for` statements may precede the required final expression.
Braced bodies can contain `let` and `const` declarations,
assignments and updates to earlier visible `let` bindings, and other supported
statements. Empty statements and standalone braces emit no instructions. A block-local declaration shadows enclosing
bindings throughout its block. The parser resolves every access to the nearest
lexical declaration after parsing the script. Access before initialization is
unsupported, even in code that would not run; it never falls back to an outer
binding. Sibling blocks can reuse a name. Unlabeled `break;` exits the innermost
loop. `continue;` reevaluates the test after the update, if there is one.
Both require explicit semicolons. `for (init; test; update)` permits any clause
to be empty; a missing test is true. The initializer can be a `let` or `const`
declaration list. Its bindings are visible in the initializer, test, update, and body, but
not after the loop. The initializer runs once each time execution enters the
loop statement, not on each iteration. The update and any non-declaration
initializer must be a supported assignment, increment, or decrement of an
initialized mutable binding. `break;` skips the update. The subset has no
closures and does not model their observable per-iteration environments.
`do { ... } while (test);` runs the body before testing, and requires the
trailing semicolon. The test is outside the body's lexical scope. `continue;`
jumps to the test; `break;` skips it. Untaken arms and skipped loop bodies
are still checked. An `else if` chain tests conditions in order, skips later
arms once one matches, and requires braces around every arm body. Each test
uses the enclosing scope, not the preceding arm's scope. The parser uses at
most 128 active statement frames, separately from the expression nesting
limit. Each `if` in a chain counts as a frame, and each `if` branch fixup
costs one unit of parse work, including in a plain `if`.

The evaluator sees only private instructions, not the original source. Before
allocating, it checks a ceiling of 4,096 prepared instructions, counting
skipped paths and jumps, independent of custom parse limits. At runtime,
each evaluation gets 16,384 dispatches. Repeated tests, assignments and loop
back edges count each time they execute; skipped paths and exiting do not.
An evaluation that needs another dispatch returns `DispatchLimitExceeded`.
Each literal adds one operand, and each eager binary operation consumes two
operands and leaves one. A unary operation replaces the
top operand. Each declaration has its own slot, initialized on every execution
of that declaration, including on loop reentry. A read pushes that slot's value;
assignment replaces the value of a mutable slot. The
evaluator checks bounds before reserving at most 2,048
operand slots and 2,047 binding slots. Reads of uninitialized slots, missing
operands and invalid final stack depth are rejected. Every evaluation uses
fresh slots; there is no shared global state. There is no recursion, host
callback, dynamic code generation, third-party dependency, or unsafe code.

Each operation is a separate binary64 (`f64`) operation, not host integer
arithmetic. Signed zero is preserved; overflow to infinity and arithmetic that
produces NaN return numeric values, not evaluation errors. No specific NaN bit
pattern is promised.

## Evidence and limits

`cargo test -p swayze-js --frozen` exercises the parse-to-result path for arithmetic,
precedence, operand order, associativity, binary64 rounding, and signed zero.
Evaluator unit tests also check malformed postfix, the work budget, overflow,
and NaN. These are focused implementation tests, not a proof of correctness or
ECMAScript conformance. They do not run Test262, and there is no JIT in this path.

With Node.js installed, `sh tools/check-js-reference.sh` independently compares
synthetic fixtures in debug and release, comparing Boolean values exactly and
Numbers by their binary64 bits except for NaN payloads. It does not execute Test262 or user-supplied code and is
not required by the normal merge gate.
