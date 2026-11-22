## Supported slice

`parse_script` parses one arithmetic expression as a JavaScript Script, with an optional final semicolon. Call `Script::evaluate` on that artifact to get its `f64` result. `prepare` is still an unsupported compatibility shim.

```text
Script  ::= Sum ';'? EOF
Sum     ::= Product (('+' | '-') Product)*
Product ::= Primary ('*' Primary)*
Primary ::= Decimal | '(' Sum ')'
Decimal ::= '0' | [1-9] [0-9]*
```

ECMAScript WhiteSpace and LineTerminator characters are permitted between tokens and at the source boundaries. Comments are not whitespace in this slice. The operators are binary and left-associative, with multiplication binding tighter than addition and subtraction. Decimal literals are rounded directly to binary64 (nearest, ties to even), including positive infinity for sufficiently long integers. There is no intermediate integer truncation or silent reinterpretation of leading zeros. Longer JavaScript punctuators such as `++` and `**` are not split into shorter supported operators.

Diagnostics distinguish malformed syntax encountered in this grammar from unsupported input. `Unsupported` does **not** mean the input is valid JavaScript; rejecting an out-of-slice construct does not require validating the rest of it. For example, `1 +` and `(1 + 2` are malformed, whereas `1e2`, `-1`, comments, the empty Script and `1; 2` are unsupported. `1 2` is malformed; `1\n2` (with an actual line break) is unsupported rather than accepted by automatic semicolon insertion. A successful parse always accounts for the complete source, including anything after the optional semicolon. Spans are half-open UTF-8 byte ranges; EOF errors use an empty range. Errors do not retain or print source text.

## Bounds and evidence

The default ceilings are 64 KiB of source, 4,096 tokens, 128 nested parentheses and 128 Ki work units. `parse_script_with_limits` takes explicit per-call limits. Tokens include punctuation and the semicolon but not whitespace or EOF. The work meter charges consumed source bytes, decimal-conversion input bytes, pending-operator pushes and emitted instructions; units are not cycle counts. Finite lookahead, amortized vector growth and binary64 conversion of at most 309 digits keep work bounded. Longer canonical integers necessarily round to infinity. Parsing and artifact destruction are iterative, not recursive. Scratch storage and output are proportional to the token count; vector growth is fallible. The private postfix contains no source text, and the parser checks that each binary operation has two operands and that exactly one value remains. A successful parse does not run the expression. The separate evaluator accepts at most 4,096 instructions even when the parser is given higher limits, and uses at most 2,048 `f64` operand slots.

The crate tests cover precedence/associativity, numeric rounding and lexical boundaries, complete-input rejection, UTF-8 spans, and parse limits, as well as arithmetic evaluation (including rounding and signed zero). Run `cargo test --locked -p swayze-js` (also with `--release`). These are focused tests, not a formal proof, a full ECMAScript conformance run or browser deployment evidence.

## Gaps

This is a standalone prototype against ECMA-262, 16th edition (2025), especially its lexical grammar, numeric literals and arithmetic expression grammar. It does not implement general Script or Module syntax, early errors, general automatic semicolon insertion, unary or other binary operators, calls, declarations, comments/Annex B behavior, alternate numeric literal syntax or JavaScript source represented as arbitrary UTF-16 code units. It does not implement host cancellation, general JavaScript execution, or renderer integration. Allocator, toolchain and platform behavior are outside the parser's guarantees.
