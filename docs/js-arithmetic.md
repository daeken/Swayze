# JavaScript arithmetic reference evaluator

`swayze_js::parse_script` validates source in the supported arithmetic subset.
Call `Script::evaluate(&self) -> Result<f64, EvalError>` on the resulting
`Script`. For example, `swayze_js::parse_script("2 + 3 * 4")?.evaluate()?`
produces `14.0`.

This path evaluates an expression over decimal literals (including fractions and
signed exponents) with binary
`+`, `-`, `*`, `/`, and `%`, plus prefix `+` and `-` and parenthesized grouping.
The parser determines precedence and associativity. Optional `const` declarations
initialize numeric slots in source order before the final expression. Reads must
refer to earlier declarations; global lookup and temporal dead zone behavior are
not implemented. There are no calls, objects or nonnumeric coercions.

The evaluator sees only the private postfix instructions, not the original
source. Before allocating, it checks an execution ceiling of 4,096 instructions
(including reads and initializations), independent of custom parse limits. It visits each
instruction at most once. Each literal adds one operand and each binary
operation consumes two operands and leaves one. A unary operation replaces the
top operand. Initialization consumes a value and appends a slot; a read pushes
that slot's value. The evaluator checks bounds before reserving at most 2,048
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
synthetic arithmetic fixtures in debug and release, using exact binary64 bits
except for NaN payloads. It does not execute Test262 or user-supplied code and is
not required by the normal merge gate.
