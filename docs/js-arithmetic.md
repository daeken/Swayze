# JavaScript arithmetic reference evaluator

`swayze_js::parse_script` validates source in the supported arithmetic subset.
Call `Script::evaluate(&self) -> Result<f64, EvalError>` on the resulting
`Script` if a Number result is required. For example,
`swayze_js::parse_script("2 + 3 * 4")?.evaluate()?` produces `14.0`.
`Script::evaluate_primitive()` returns `PrimitiveValue::Scalar(Value)` or
`PrimitiveValue::String(&[u16])` (facade: `JsPrimitiveValue`). The String
slice borrows immutable Script storage or a static `typeof` spelling. A final
concatenated String (even empty) returns `RuntimeStringResultRequiresScratch`
from that API. To accept all kinds of String, use `Script::evaluate_primitive_with_scratch(&mut scratch)`
with `EvalScratch` (facade: `JsEvalScratch`); the result borrows both inputs.
Each call clears the scratch's logical length without zeroizing its storage
and can reuse retained capacity. `evaluate_value()` still returns
`Value::Number`, `Value::Boolean`, `Value::Null`, or `Value::Undefined` without
coercion; a String result yields `NonScalarResult`. `evaluate()` returns
`NonNumericResult` for any non-Number. An unbound read of `undefined` returns
the default value, but a block-local declaration of that name takes precedence.
A top-level declaration of `undefined` is malformed in a classic script.

Unary `typeof` evaluates its operand once and returns `"number"`, `"boolean"`,
`"string"`, `"object"` for `null`, or `"undefined"`. The spelling itself does
not allocate or consume literal or concatenation storage. Only a direct
unbound name, optionally wrapped in pure parentheses, gets the unbound-name
exception. For example, `typeof (missing)` yields `"undefined"`, but
`typeof (0, missing)` still rejects the ordinary unbound read. A lexical binding
wins over the exception, including a later declaration whose access is rejected
before initialization. This is the closed primitive environment, not a full
JavaScript Realm or implementation of runtime TDZ errors.

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

Standalone blocks, empty statements (`;`), expression statements, `if`, and
braced `while`, `do…while`, and `for` statements may precede the required final
expression.
Braced bodies can contain `let` and `const` declarations,
assignments and updates to earlier visible `let` bindings, and other supported
statements. Empty statements and standalone braces emit no instructions. A block-local declaration shadows enclosing
bindings throughout its block. The parser resolves every access to the nearest
lexical declaration after parsing the script. Access before initialization is
unsupported, even in code that would not run; it never falls back to an outer
binding. Sibling blocks can reuse a name. Unlabeled `break;` exits the innermost
loop. `continue;` reevaluates the test after the update, if there is one.
Both require explicit semicolons. One or more ASCII identifier labels can
precede a standalone block, an `if` statement, or a `while`, `do`, or
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
are still checked. Each `if` arm can be a block or a single supported
statement. A single arm does not introduce a lexical scope and cannot be a
bare `let` or `const` declaration. `else` binds to the nearest unmatched
`if`. An `else if` chain tests conditions in order and skips later arms once
one matches. Each test uses the enclosing scope, not the preceding arm's scope. The parser uses at
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

## Hashbang and normal completion

At byte zero, `#!` starts a metered comment through LF, CR, LS, PS or EOF,
with no token emitted. A BOM, whitespace, or any other preceding bytes prevent
it from being a hashbang. The ordinary source and parse work limits still apply.
If adjacent `#!` is the first nontrivia token at a nonzero byte offset, the
parse error carries `ParseErrorType::SyntaxError` (`JsParseErrorType` in the
facade) in addition to its existing coarse `Unsupported` category. The type
is optional: an untyped error is not evidence that the source is invalid
JavaScript. This classification does not cover `#!` after another token.

`parse_script_for_completion` and its `_with_limits` variant allow EOF after
complete statements, including an empty or comment-only source. Execute the
result with `execute_with_scratch`; all reached operations still run with the
usual checks and limits, but any final value is discarded. This is not access
to ECMAScript Script completion values and does not add ASI. The existing
`parse_script`/`evaluate*` value APIs still require a final expression.

## String literals

Single- and double-quoted literals decode to exact UTF-16 during parsing,
including raw Unicode, `\xHH`, `\uHHHH`, `\u{…}`, lone escaped surrogates,
and NUL. Backslash line continuations add no units; raw CR or LF is malformed
inside a literal. In sloppy quoted strings, `\8` and `\9` each cook to one
ASCII digit without consuming any following digit. Legacy octal escapes
(including `\08` and `\09`) and root `use strict` directives remain outside
the subset. Source ingress and diagnostic offsets remain UTF-8; a Script
does not retain source text.

An untagged backtick literal produces a cooked UTF-16 String. Raw CR and
CRLF normalize to LF; raw LF, LS and PS are retained. Backslash line
continuations emit no units, and `\${` produces literal `${` text. An
unescaped `${` begins a supported primitive expression (which may itself
contain a template). Substitutions run once, in source order, with primitive
`ToString`; the complete expression can take `.length`. Tagged templates,
raw access and object coercion remain outside the subset. Decimal and octal
escapes are malformed in templates. Backticks never introduce directives.

Each quoted token or cooked template segment may occupy up to 4,096 UTF-8
bytes, including its delimiters. An open template with substitutions counts
toward expression nesting. Substituted strings use evaluation scratch. Each
substitution appends the previous prefix, the converted value and the next
cooked segment in one dispatch. This copies the prefix on each append; all
copies count toward the shared 4,094-unit result and 32,768-unit evaluation
limits, so a short result can still exceed the total budget.
All literals in a script together may decode to at most 16,384 UTF-16 code
units, including literals in skipped code. Exhaustive `Limit`/`JsLimit`
matches must handle `StringLiteralBytes` and `StringLiteralUnits`; the
caller-configurable `Limits`/`JsLimits` fields did not change. Reading a
literal does not allocate runtime String storage.

In the arithmetic API, a primary (including a parenthesized expression) may
take one read-only `.length` suffix. It returns the exact UTF-16 code-unit
count as a Number from a literal, static, or concatenated String in one
dispatch, without scanning or allocating.
Trivia, including line breaks, is allowed around the dot but the name must
be raw ASCII `length`. A reached non-String receiver produces
`UnsupportedPropertyReceiver`, a Swayze subset error, not JavaScript
`undefined` or a `TypeError`. Property writes, updates, indexing, arbitrary
properties, and ungrouped suffix chains are outside the subset. Existing
numeric adjacency restrictions still apply; `(1).length` and `1 .length`
parse, but reach the receiver error when evaluated. The opt-in Realm uses
the separate dot slice described below.

Only the empty String is falsy, and every String is non-nullish. String
equality and ordering between two Strings compare UTF-16 code units without normalization. For
ordering, a proper prefix sorts before the longer String. Both comparisons
share a budget of 65,536 code-unit pairs per evaluation, separate from the
dispatch budget. Equality charges the full length for equal-length Strings
and zero otherwise; ordering charges the shorter length. Both charge before
comparing, even on an early mismatch. Strings compare strictly unequal to
other types and loosely unequal to `null` and `undefined`, without coercion.
Mixed loose equality with Numbers or Booleans and mixed String/scalar ordering
compare numerically instead. For example, `'10' < '2'` is true, but `'10' < 2`
is false. Mixed ordering treats `null` as `+0` and `undefined` as NaN; a NaN
operand makes all four relations false. Mixed comparisons spend no code-unit
pairs.

Binary `+` and `+=` concatenate when either operand is a String, in
left-to-right UTF-16 order without normalizing code units. Booleans, `null`,
and `undefined` use their literal spellings. Numbers use the shortest
round-tripping decimal, choosing the closest and then the even significand
on an exact tie. Both zeros print `0`; `NaN` and signed infinities have
fixed spellings. The notation uses fixed form for decimal exponents −6
through 20 and lowercase `e` with a signed exponent otherwise. The private
formatter uses at most 25 UTF-16 units of stack scratch per reached Add;
it does not use String-to-Number credits or a separate formatter meter.
When neither operand is a String, `+` and `+=` still perform numeric addition.

The result may have at most 4,094 code units; each evaluation may append
at most 32,768 code units, including scalar spellings and intermediate
results that are overwritten or discarded. These inclusive caps
produce `StringConcatenationLengthLimitExceeded` and
`StringConcatenationBudgetExceeded`, respectively, and are independent of
dispatch, comparison, and conversion budgets. Reached, nonempty concatenations
may grow the scratch allocation fallibly. Empty or skipped concatenations do
not allocate String storage. This scratch is not a GC heap.

Unary `+`, `-`, and `~` and binary `-`, `*`, `/`, `%`, `**`, `&`, `^`, `|`,
`<<`, `>>`, and `>>>` (including their compound assignments) convert Strings to
Numbers, as do the mixed numeric comparisons above and name updates with
`++` and `--`. Conversion trims
ECMAScript whitespace and line terminators, then accepts signed decimal and
`Infinity` forms and unsigned `0x`, `0o`, and `0b` integers. Empty text becomes
`+0`, negative zero is preserved, and invalid numeric text produces NaN rather
than a parse error. Decimal parsing rounds the full magnitude to binary64
through the pinned Rust parser; radix conversion rounds directly from integer
digits. Unary `-` then negates the Number, and `~` applies bitwise NOT after
converting the rounded Number to a signed 32-bit integer. Binary bitwise and
shift operators use the same 32-bit reduction rules as for Numbers, with shift
counts masked to their low five bits. The supported binary
operators evaluate both expressions before converting operands left to right.
Their compound forms read the old left-hand value before the right-hand side
runs and store the result only if the operation succeeds. Name updates convert
once before adding or subtracting one. Prefix expressions return the new Number;
postfix expressions return the old converted Number, including signed zero or
NaN. Consuming updates store the result without yielding it. Concatenation
does not convert either String operand to a Number.

Each evaluation has 262,144 conversion credits, separate from comparison
and dispatch limits. A reached String conversion charges `1 + original
UTF-16 length`. Trimmed-empty text, exact optionally signed `Infinity`, and
recognized unsigned radix prefixes pay only that amount. Every other
candidate also pays `256 + 3 × trimmed magnitude length`, excluding the
optional sign, before syntax validation. Skipped conversions and comparisons
that do not coerce cost no conversion credits.
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

## Opt-in Realm: ordinary functions and dot properties

`realm::CompiledScript` admits anonymous `function (...) { ... }` primaries in
Script expressions, including in blocks, control tests and classic `var` loop
headers. Use parentheses to start an expression statement. A statement starting
with `function` is still a declaration (Script root only). Named expressions,
function forms inside a function body, and non-global lexical captures are
excluded. Realm lexical declarations remain Script-root only. The arithmetic
API and the default engine front door do not opt into this grammar.

The parser drives one statement/expression continuation at a time. At a function
primary it parks the Script's code, operands, pending reducers, statement frames,
labels, loop/header state, delimiter continuation, lookahead and directive
state. The same lexer (cursor and aggregate resource ledgers unchanged) and
body compiler handle formals, local `var`, and statements. Closing the body
restores the Script at the completed primary, before its suffix. A single
parked owner is enough because nested functions are excluded. Parked and
active frames count toward the same 128-statement-frame ceiling; delimiter
depth is independently capped at 128 across both owners. Body strictness and
quoted escape witnesses are definition-site and per owner.

Instantiation retains an immutable artifact for any Script with a callable
body, even if its Ready guard is dropped or every expression creation is
skipped. The artifact owns body code, binding sites, names, UTF-16 literals
and dot metadata; root instructions, layout and dot sites are read from the
borrowed Ready code. Body dot sites are resolved in the actual retained
artifact and body. Dropping the original compiled Script does not invalidate
an installed function. Free names are read from the Realm's current global
records, not from old Ready links or caller slots. Skips and abandonment create
no *expression* identities. Each reached creation is fresh, and a later failed
store does not undo it. There is no public object or function handle.

### Dot operations and barriers

The Realm accepts chained `base.key` reads, simple `base.key = value` writes,
and `base.key(args)` calls. Dot keys are raw ASCII IdentifierNames (letters,
`_` or `$` initially, also digits thereafter), including keywords. Trivia is
allowed around the dot; existing numeric-literal adjacency rules still apply.
Computed keys, escaped or non-ASCII keys, compound/logical field assignments,
field updates, object literals, `new`, and `this` remain outside this slice.
For example, given `function f(x){ return x; }`,
`f.prototype.constructor(3)` calls `f` and yields `3` within the Script.

An ordinary callable has its own read-only `name` and `length` (the formal
count), and a writable `prototype` value. The fresh prototype object has a
writable `constructor` backlink to the callable. Initial rows are not enumerable;
`name`, `length` and the backlink are configurable, and the callable's
`prototype` row is not. There is no descriptor reflection API. A new field is
writable, enumerable and configurable; replacing an own data field preserves
its attributes. A declaration's `name` is its spelling. An anonymous function
gets an inferred name only from an eligible direct identifier initializer,
assignment or reached logical assignment, not a field write. Parentheses around
the anonymous RHS preserve inference; value operators can erase it. An
uninferred `name` is empty, and no name introduces a self binding.

A lookup follows real prototype links. A miss through complete shapes yields
`undefined`. The `Object.prototype` builtins `hasOwnProperty`, `isPrototypeOf`,
`propertyIsEnumerable`, `toLocaleString`, `toString` and `valueOf`, and the
`Function.prototype` builtins `apply`, `bind`, `call` and `toString` are known
writable data descriptors but have no callable value implemented here. Their
Get stops with a builtin fault; a write may shadow a known inherited writable
row without fetching its value. The restricted `arguments` and `caller`
accessors and the Annex B keys `__proto__`, `__defineGetter__`, `__defineSetter__`,
`__lookupGetter__` and `__lookupSetter__` instead stop on reached Get or Put.
An unknown own key on the private `Object` constructor stops before the lookup
tries inheritance: its own shape is incomplete. Neither that constructor nor
`Object.prototype` has a direct guest value path in this subset.

Member calls read and pin both the callee and the raw base before arguments.
Pure parentheses preserve the member reference; a value-producing comma,
conditional, logical or assignment expression detaches it. Inherited methods
retain the original base, not the row's owner. The raw receiver is not exposed
as `this`. After arguments, a user function enters its body; calling
`Function.prototype` returns `undefined` without a guest frame. Calls of the
`Function` and `Object` constructors have distinct unsupported-invocation
faults, not a noncallable `TypeError`. An actual noncallable produces a
`TypeError` after arguments, whereas a failed property Get stops before them.

A write captures the base, evaluates the RHS, then checks the *current* row
and receiver shape. RHS writes or calls can thus change the descriptor seen by
the outer write, but rebinding the base name cannot change the captured base.
A read-only Set is ignored in sloppy code and raises `TypeError` in strict code;
the assignment expression yields the original RHS on success. A failed write
does not publish a partial field or String copy and does not roll back prior
RHS effects. Property Get on `null` or `undefined` throws `TypeError`; Put on
those bases throws only after a successful RHS. String `length` is the exact
UTF-16 length and is read-only. Other primitive properties have no boxing or
prototype lookup; a Number or Boolean `.length` keeps
`UnsupportedPropertyReceiver`, and other keys on non-nullish primitives stop
with a primitive-receiver fault. Objects and callables are truthy, compare by
identity to others of the same kind, and report `typeof` `"object"` or
`"function"`. Operations requiring object-to-primitive conversion stop rather
than guessing a value; unsupported callable conversion keeps its own fault.

### Lifetime, work, and backing

- Dot metadata has one immutable occurrence per spelling, separate from name
  linking: at most 4,096 occurrences, 256 UTF-16 units in a key and 16,384
  pooled key units per Script. Parse work includes scanning and storing the
  key. Root and body layout and all property packet edges are checked before
  publication, including untaken paths and roots with no calls or artifact.
  Site checks use the owning code's validated table. These checks, artifact
  copies and hoisted object setup share the 131,072-credit instantiation
  ledger; work exhaustion, allocation failure and invalid code are distinct.
- There are at most 16 retained artifacts, 512 KiB of accounted immutable
  artifact backing, and 512 user function identities per Realm. The artifact
  budget includes dot metadata, the original literal prefix and the validated
  UTF-16 name tail (up to 16,384 units each). The literal accessors cannot read
  the name tail. Code names, empty or static Strings and already-retained
  aliases need no copy to persist. Binding and field writes share 16,384 copied
  UTF-16 units per execution and 32,768 retained per Realm, including new
  dynamic keys. Replacement does not copy a key again. Overwriting cells or
  fields does not reclaim old storage; the Realm owns it until teardown.
- The first callable creation installs four private roots and 27 rows. Each
  actual ordinary callable adds two objects and four initial rows. The heap
  caps are 1,280 objects, 4,096 rows, 64 own rows per object and 32 visited
  prototype nodes per lookup. A full row table does not prevent replacement of
  an existing writable row. These are bounding limits, not promises that all
  maxima are simultaneously reachable.
- Each execution has 131,072 property-work credits shared across all its calls,
  separate from the existing 131,072-credit call/setup/name-lookup ledger
  and the dispatch and String budgets. A descriptor lookup charges one per visited node and row, plus
  compared UTF-16 pairs for equal-length keys up to and including the first
  mismatch. A pending assignment or member-call record costs one plus any
  moved records. Field replacement costs two; insertion costs four; both add
  any moved rows and copied key/value units. Reached creation charges initial
  rows/nodes and moves here, and identity setup in the existing lookup ledger.
  Object/row growth requests have a separate 512 KiB budget per instantiation
  or execution.
- Existing execution limits remain 64 arguments/formals, 128 total frame
  locals including formals, 32 guest frames, 4,096 aggregate live slots and
  16,384 dispatches. Pending reference records and live member receivers count
  against the same live-slot ceiling, including at call entry. The packet
  backing requests at most 4,096 records, or 6,144 for old plus new during
  growth. On the pinned ARM64 build the object, row, origin and packet records
  are 14, 40, 6 and 24 bytes respectively. Old-plus-new logical tier requests
  can cover up to 2,304 object slots, 6,144 row slots and 768 origin slots,
  excluding separately staged metadata. At those record sizes, the tier sums
  give a conservative 374,218-byte bound on cumulative object/row requests
  and a 282,624-byte old-plus-new bound for objects, rows and origins combined.
  The retained String arena requests up to 64 KiB of backing, or 96 KiB for
  old plus new during growth. These are derived backing bounds from measured
  record sizes, not measured allocator footprint or whole-process memory bounds.

This is an opt-in Script slice, not general object support. The Rust regressions
exercise this slice; they do not show that a Test262 helper loads or runs. The
change adds no runner or reference-helper coverage and makes no ECMAScript
conformance claim.
