# JavaScript arithmetic reference evaluator

`swayze_js::parse_script` validates source in the supported arithmetic subset.
Call `Script::evaluate(&self) -> Result<f64, EvalError>` on the resulting
`Script` if a Number result is required. For example,
`swayze_js::parse_script("2 + 3 * 4")?.evaluate()?` produces `14.0`.
`Script::evaluate_primitive()` returns `PrimitiveValue::Scalar(Value)` or
`PrimitiveValue::String(&[u16])` (facade: `JsPrimitiveValue`). The String
slice borrows immutable Script storage. `evaluate_value()` still returns
`Value::Number`, `Value::Boolean`, `Value::Null`, or `Value::Undefined` without
coercion; a String result yields `NonScalarResult`. `evaluate()` returns
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
Unary `void` evaluates its operand once, then returns `undefined` without
coercion. It has the same precedence and right associativity as the other
prefix operators.
Exponentiation (`**`) binds more tightly than multiplication and associates
right to left. A prefix unary base must be parenthesized (`(-2) ** 2`), but
a unary exponent is allowed (`2 ** -2`). Number special cases are handled
explicitly; ordinary finite powers use a platform-dependent approximation.
The statement-only `**=` uses the same exponentiation operation.
The parser determines precedence and associativity. `const` and `let`
declarations initialize slots in source order, including left to right within
comma-separated lists. Each `const` item requires an initializer. A `let`
item without an initializer initializes to `undefined` each time it executes,
without reading any binding named `undefined`. Top-level commas separate
declarators; a comma expression inside an initializer requires parentheses.
Trailing commas in declaration lists are unsupported.
Simple and compound assignments update earlier `let` slots. Arithmetic and
bitwise compound assignments read the target once, evaluate the right operand,
apply the corresponding operator, and store the result. Logical assignments
(`&&=`, `||=`, `??=`) read the target once and may skip both the right operand
and the store, using the same tests as `&&`, `||`, and `??`. Skipped operands
are still checked during parsing, and `const` targets are rejected even when
no store would run. Prefix and postfix `++` and `--` convert an initialized
`let` value to Number, add or subtract one, and store the result. Postfix
forms cannot cross a line terminator between the name and operator, including
a line terminator in a comment. Updates can be operands in expressions such
as `1 + ++x`, declaration initializers such as `let y = x++;`, or control
tests such as `if (x++)`. Prefix returns the new Number; postfix returns
the old coerced Number, including signed zero. At the start of a root or
block statement or a non-declaration classic `for` initializer or updater,
a complete update followed by a supported binary operator or `?` starts an
ordinary expression, as in `x++ + 1` or `++x ** 2`. A line terminator after
the complete update is allowed before these operators. A lone update or one
immediately followed by a comma still uses the path that emits the write
without leaving a value. Assignments
can also appear as explicitly grouped expressions, such as `(x = a)`,
`(x += a)`, and `(x ??= a)`. Simple assignment stores and returns its
right-hand value. An eager compound reads the target before evaluating the
right-hand side, then stores and returns the numeric result. A logical
compound returns the old value on a skip, with no write. Otherwise it stores
and returns the right-hand value without coercion. In `(x = a, b)`, the
assignment stores `a` and the sequence yields `b`. In `(x = (a, b))`, the
sequence is the right-hand side, so the assignment stores and returns `b`.
A skipped logical assignment still proceeds to the next sequence operand.
A lexical initializer, an existing assignment's right-hand side, either
ternary arm, or a control test can start with a right-associative chain of
simple or compound name assignments, as in `let r = x += y = 3;` and
`if (x ||= y)`. An eager compound reads the old target before evaluating its
right-hand side; a logical compound can skip the entire nested chain. Only
the selected ternary arm runs. A comma at test level follows the assignment,
so `if (x = 0, true)` stores `0` and tests `true`. After a comma in a
supported sequence, the next operand can start with an assignment chain,
as in `0, x += y = 3, x + y`.
Declaration separators and commas disallowed by the enclosing assignment or
ternary syntax do not start sequence operands.
Declarations, assignments, updates, and nonfinal expression statements require
explicit semicolons. A root or block statement that starts with an assignment
or a prefix or same-line postfix update can continue with a comma sequence:
`x ||= 4, y += x;` or `x++, y += x;`. The leading write finishes or skips
before the remaining operands run, and the statement yields no value.
Expression statements discard their values. The script
must end in a root-level expression, whose trailing semicolon is optional;
only trivia may follow it. This is not general JavaScript Script completion.
Unavailable bindings
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

Comma expressions bind more loosely than `?:`. They evaluate operands
left to right, discard each nonfinal value, and return the final value
without coercion. Each discard costs one instruction dispatch. Comma
expressions are allowed in expression statements, the final expression, and control tests;
within a ternary arm or an assignment right-hand side they require grouping.

Standalone blocks, empty statements (`;`), expression statements, and braced `if`, `while`,
`do…while`, and `for` statements may precede the required final expression.
Braced bodies can contain `let` and `const` declarations,
assignments and updates to earlier visible `let` bindings, and other supported
statements. Empty statements and standalone braces emit no instructions. A block-local declaration shadows enclosing
bindings throughout its block. The parser resolves every access to the nearest
lexical declaration after parsing the script. Access before initialization is
unsupported, even in code that would not run; it never falls back to an outer
binding. Sibling blocks can reuse a name. Unlabeled `break;` exits the innermost
loop. `continue;` reevaluates the test after the update, if there is one.
Both require explicit semicolons. One or more ASCII identifier labels can
precede a standalone block, a braced `if` chain, or a `while`, `do`, or
classic `for` loop. All labels on a statement share the same target, and an
`if` label is active in every arm. `break name;` exits the whole labeled
statement; `continue name;` can target only a loop. Neither permits a line
terminator between the keyword and name. Labels are separate from lexical
bindings, and active duplicates are rejected. Alias validation, target lookup,
and name comparisons are charged to parse work; aliases add no statement frames.
`for (init; test; update)` permits any clause
to be empty; a missing test is true. The initializer can be a `let` or `const`
declaration list. Its bindings are visible in the initializer, test, update, and body, but
not after the loop. The initializer runs once each time execution enters the
loop statement, not on each iteration. The update and any non-declaration
initializer discard their results. A comma sequence can start with an
assignment, prefix update, or same-line postfix update, as in
`for (++x, y = 0; y < 2; x++, ++y) {}`. The first assignment or update
finishes before the next operand, and a short-circuited logical assignment
still proceeds to the rest of the sequence. `continue;` runs the whole
updater sequence; `break;` skips it. The subset has no
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

## String literals

Single- and double-quoted literals decode to exact UTF-16 during parsing,
including raw Unicode, `\xHH`, `\uHHHH`, `\u{…}`, lone escaped surrogates,
and NUL. Backslash line continuations add no units; raw CR or LF is malformed
inside a literal. Legacy decimal and octal escapes and root `use strict`
directives are outside the subset. Source ingress and diagnostic offsets
remain UTF-8; a Script does not retain source text.

Each quoted token may occupy up to 4,096 UTF-8 bytes, including the quotes.
All literals in a script together may decode to at most 16,384 UTF-16 code
units, including literals in skipped code. Exhaustive `Limit`/`JsLimit`
matches must handle `StringLiteralBytes` and `StringLiteralUnits`; the
caller-configurable `Limits`/`JsLimits` fields did not change. Evaluation
allocates no String storage and moves only private checked ranges.

Only the empty String is falsy, and every String is non-nullish. String
equality and ordering compare UTF-16 code units without normalization. For
ordering, a proper prefix sorts before the longer String. Both comparisons
share a budget of 65,536 code-unit pairs per evaluation, separate from the
dispatch budget. Equality charges the full length for equal-length Strings
and zero otherwise; ordering charges the shorter length. Both charge before
comparing, even on an early mismatch. Strings compare strictly unequal to
other types and loosely unequal to `null` and `undefined`. Mixed loose equality
with Numbers or Booleans, mixed String/scalar ordering, concatenation and
updates on Strings are unsupported. An unsupported String operation is an
evaluation error only when it executes.

Unary `+`, `-`, and `~` convert a String to a Number. Conversion trims
ECMAScript whitespace and line terminators, then accepts signed decimal and
`Infinity` forms and unsigned `0x`, `0o`, and `0b` integers. Empty text becomes
`+0`, negative zero is preserved, and invalid numeric text produces NaN rather
than a parse error. Decimal parsing rounds the full magnitude to binary64
through the pinned Rust parser; radix conversion rounds directly from integer
digits. Unary `-` then negates the Number, and `~` applies bitwise NOT after
converting the rounded Number to a signed 32-bit integer. Binary operators
and updates do not perform this String-to-Number conversion yet.

Each evaluation has 262,144 conversion credits, separate from comparison
and dispatch limits. A reached String conversion charges `1 + original
UTF-16 length`. Trimmed-empty text, exact optionally signed `Infinity`, and
recognized unsigned radix prefixes pay only that amount. Every other
candidate also pays `256 + 3 × trimmed magnitude length`, excluding the
optional sign, before syntax validation. Skipped conversions cost nothing.
Exhausting this budget yields `StringNumericConversionLimitExceeded`.

## Evidence and limits

`cargo test -p swayze-js --frozen` exercises the parse-to-result path for arithmetic,
precedence, operand order, associativity, binary64 rounding, and signed zero.
Evaluator unit tests also check malformed postfix, the work budget, overflow,
and NaN. These are focused implementation tests, not a proof of correctness or
ECMAScript conformance. They do not run Test262, and there is no JIT in this path.

With Node.js installed, `sh tools/check-js-reference.sh` independently compares
synthetic fixtures in debug and release, comparing value tags, Boolean values,
String UTF-16 code units, and Number bits except for NaN payloads. It does not
execute Test262 or user-supplied code and is
not required by the normal merge gate.
