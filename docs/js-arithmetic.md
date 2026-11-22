# JavaScript arithmetic reference evaluator

`swayze_js::parse_script` validates source in the supported arithmetic subset.
Call `Script::evaluate(&self) -> Result<f64, EvalError>` on the resulting
`Script`. For example, `swayze_js::parse_script("2 + 3 * 4")?.evaluate()?`
produces `14.0`.

This path evaluates an expression over supported integer literals with binary
`+`, `-`, and `*`, including parenthesized grouping. The parser determines
precedence and associativity. There are no variables, calls, coercions or other
language execution facilities in this evaluator.

The evaluator sees only the private postfix instructions, not the original
source. Before allocating, it checks an execution ceiling of 4,096 instructions
(one per literal or operator), independent of custom parse limits. It visits each
instruction at most once. Each literal adds one operand and each binary
operation consumes two operands and leaves one. The evaluator reserves storage
for the peak stack depth calculated by the parser, after checking that it is
at most `ceil(n/2)` `f64` operands for `n` instructions. It rejects any
attempt to exceed the reserved depth, missing operands, or anything other
than a single final result. Evaluation uses no recursion, host callbacks, dynamic code generation,
external dependencies, or unsafe code.

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
