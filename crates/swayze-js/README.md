## Supported slice

`parse_script` parses one arithmetic expression as a JavaScript Script, with an optional final semicolon. Call `Script::evaluate` on that artifact to get its `f64` result. `prepare` is still an unsupported compatibility shim.

```text
Script   ::= Sum ';'? EOF
Sum      ::= Product (('+' | '-') Product)*
Product  ::= Unary (('*' | '/' | '%') Unary)*
Unary    ::= ('+' | '-') Unary | Primary
Primary  ::= Decimal | '(' Sum ')'
Decimal  ::= (Integer ('.' [0-9]*)? | '.' [0-9]+) Exponent?
Integer  ::= '0' | [1-9] [0-9]*
Exponent ::= [eE] [+-]? [0-9]+
```

ECMAScript WhiteSpace and LineTerminator characters are permitted between tokens and at the source boundaries. Prefix `+` and `-` associate to the right and bind tighter than `*`, `/`, and `%`, which share a precedence level above binary `+` and `-`. Binary operators associate to the left. A single `/` can be division only after an operand; regular expression literals and `//` or `/*` comments are outside the slice. Longer JavaScript punctuators such as `++`, `--`, `/=`, `%=`, and `**` are not split into shorter supported operators.

A decimal literal is one token, with no whitespace inside it. It may include a decimal point (`.5`, `1.`, `1.25`), an exponent (`1e3`, `1E-3`), or both (`1.e3`). A sign before the mantissa is a unary operator; a sign after `e` or `E` is part of the exponent. Property access and spread, identifier suffixes, numeric separators, BigInt, nondecimal bases and legacy leading-zero integers are outside the slice.

Decimal literals are rounded directly to binary64 (nearest, ties to even), including positive infinity on overflow and subnormals or positive zero for sufficiently small magnitudes. There is no intermediate integer truncation or separate rounded scaling by the exponent. Each operation works on binary64 Numbers, including signed zeros, infinities, and NaN. Remainder truncates the exact quotient toward zero without rounding it to binary64 first; a zero remainder has the sign of the dividend.

Diagnostics distinguish malformed syntax encountered in this grammar from unsupported input. `Unsupported` does **not** mean the input is valid JavaScript; rejecting an out-of-slice construct does not require validating the rest of it. For example, `1 +`, `(1 + 2` and `1e+` are malformed, whereas `1..toString()`, `1e2_3`, `2**3`, comments, the empty Script and `1; 2` are unsupported. `1 2` is malformed; `1\n2` (with an actual line break) is unsupported rather than accepted by automatic semicolon insertion. A successful parse always accounts for the complete source, including anything after the optional semicolon. Spans are half-open UTF-8 byte ranges; EOF errors use an empty range. Errors do not retain or print source text.

## Bounds and evidence

The default ceilings are 64 KiB of source, 4,096 tokens, 128 nested parentheses and 128 Ki work units. `parse_script_with_limits` takes explicit per-call limits. Tokens include punctuation and the semicolon but not whitespace or EOF. The work meter charges consumed source bytes, decimal-conversion input bytes, pushes of pending operators and opening-parenthesis markers, and emitted instructions; units are not cycle counts. Finite lookahead, amortized vector growth and bounded conversion keep work bounded. Literals with a decimal point or exponent have a fixed inclusive ceiling of 4,096 bytes per literal (`Limit::DecimalLiteralBytes`), independent of caller limits. Integer-only literals of 310 digits or more necessarily round to infinity and need no conversion after lexing; the same shortcut is not applied to fractions or exponents. All other literals use Rust's `f64` parser within the conversion bound.

Parsing and artifact destruction are iterative, not recursive. Scratch storage and output are proportional to the token count; vector growth is fallible. The private postfix contains no source text, and the parser checks that each operator has the required operands and that exactly one value remains. Prefix chains use the token and work budgets, not the parenthesis nesting budget. A successful parse does not run the expression. The separate evaluator accepts at most 4,096 instructions even when the parser is given higher limits, and uses at most 2,048 `f64` operand slots.

The crate tests cover precedence/associativity, numeric rounding and lexical boundaries, complete-input rejection, UTF-8 spans, and parse limits, as well as arithmetic evaluation (including rounding and signed zero). Run `cargo test --locked -p swayze-js` (also with `--release`). These are focused tests, not a formal proof, a full ECMAScript conformance run or browser deployment evidence.

## Gaps

This is a standalone prototype against ECMA-262, 16th edition (2025), especially its lexical grammar, numeric literals and arithmetic expression grammar. It does not implement general Script or Module syntax, early errors, general automatic semicolon insertion, other unary or binary operators, calls, declarations, regular expression literals, comments/Annex B behavior, alternate numeric literal syntax or JavaScript source represented as arbitrary UTF-16 code units. It does not implement host cancellation, general JavaScript execution, or renderer integration. Allocator, toolchain and platform behavior are outside the parser's guarantees.
