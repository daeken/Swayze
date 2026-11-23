//! Bounded WebAssembly binary validation and typed execution.
//!
//! # Supported slice
//!
//! [`validate_module`] accepts the [WebAssembly Core 1.0] header, any number of
//! custom sections within [`Limit`], and optional table, memory, global,
//! start, element, and data sections. Modules with functions must have one to eight
//! types, one to eight function declarations, and exactly as many code bodies
//! as function declarations, in that section order. Each type must have zero
//! to four parameters in any order and either no result (void) or one result,
//! even if unused. Each parameter and result can be `i32`, `i64`, `f32`, or `f64`.
//! Every function declares its own zero-based index into the type table; types
//! may be shared. Each body may declare up to 16 locals in total, in addition
//! to its parameters. The number of groups and each group's local count are
//! unsigned LEB32. Every group, even a zero-count group, must specify `i32`
//! (`0x7f`), `i64` (`0x7e`), `f32` (`0x7d`), or `f64` (`0x7c`).
//! Supported instructions are `local.get`, `local.set`, `local.tee`, `global.get`,
//! `global.set` (each with an unsigned LEB32 index), `i32.const <signed LEB32>`,
//! `i64.const <signed LEB64>`, `i64.extend_i32_s`, `i64.extend_i32_u`, `i32.wrap_i64`,
//! `f32.const`, `f64.const`, `i32.reinterpret_f32`, `i64.reinterpret_f64`,
//! `f32.reinterpret_i32`, `f64.reinterpret_i64`,
//! `f32.convert_i32_s`, `f32.convert_i32_u`, `f32.convert_i64_s`, `f32.convert_i64_u`,
//! `f64.convert_i32_s`, `f64.convert_i32_u`, `f64.convert_i64_s`, `f64.convert_i64_u`,
//! `i32.trunc_f32_s`, `i32.trunc_f32_u`, `i32.trunc_f64_s`, `i32.trunc_f64_u`,
//! `i64.trunc_f32_s`, `i64.trunc_f32_u`, `i64.trunc_f64_s`, `i64.trunc_f64_u`,
//! `f32.demote_f64`, `f64.promote_f32`,
//! `f32.abs`, `f32.neg`, `f32.copysign`, `f64.abs`, `f64.neg`, `f64.copysign`,
//! `f32.ceil`, `f32.floor`, `f32.trunc`, `f32.nearest`, `f32.sqrt`,
//! `f64.ceil`, `f64.floor`, `f64.trunc`, `f64.nearest`, `f64.sqrt`,
//! `f32.add`, `f32.sub`, `f32.mul`, `f32.div`, `f64.add`, `f64.sub`, `f64.mul`, `f64.div`,
//! `f32.eq`, `f32.ne`, `f32.lt`, `f32.gt`, `f32.le`, `f32.ge`,
//! `f64.eq`, `f64.ne`, `f64.lt`, `f64.gt`, `f64.le`, `f64.ge`,
//! `i64.eqz`, `i64.eq`, `i64.ne`, `i64.lt_s`, `i64.lt_u`, `i64.gt_s`, `i64.gt_u`,
//! `i64.le_s`, `i64.le_u`, `i64.ge_s`, `i64.ge_u`, `i64.add`, `i64.sub`, `i64.mul`,
//! `i64.div_s`, `i64.div_u`, `i64.rem_s`, `i64.rem_u`,
//! `i64.clz`, `i64.ctz`, `i64.popcnt`, `i64.and`, `i64.or`, `i64.xor`, `i64.shl`,
//! `i64.shr_s`, `i64.shr_u`, `i64.rotl`, `i64.rotr`,
//! `i32.eqz`, `i32.eq`, `i32.ne`, `i32.lt_s`, `i32.lt_u`, `i32.gt_s`, `i32.gt_u`, `i32.le_s`,
//! `i32.le_u`, `i32.ge_s`, `i32.ge_u`, `i32.clz`, `i32.ctz`, `i32.popcnt`,
//! `i32.add`, `i32.sub`, `i32.mul`, `i32.div_s`, `i32.div_u`, `i32.rem_s`, `i32.rem_u`,
//! `i32.and`, `i32.or`, `i32.xor`, `i32.shl`, `i32.shr_s`, `i32.shr_u`, `i32.rotl`,
//! `i32.rotr`, `i32.load`, `i32.load8_s`, `i32.load8_u`, `i32.load16_s`,
//! `i32.load16_u`, `i32.store`, `i32.store8`, `i32.store16`,
//! `i64.load`, `i64.load8_s`, `i64.load8_u`, `i64.load16_s`, `i64.load16_u`,
//! `i64.load32_s`, `i64.load32_u`, `i64.store`, `i64.store8`, `i64.store16`,
//! `i64.store32`, `f32.load`, `f64.load`, `f32.store`, `f64.store`, `memory.size`,
//! `memory.grow` (`0x40`), `unreachable` (`0x00`), `nop`, `drop`, untyped `select`, `call`,
//! `call_indirect` (`0x11`), `if`, `block`, `loop`, `br <depth>` (`0x0c`),
//! `br_if <depth>`, `br_table` (`0x0e`), and `return` (`0x0f`). Local indices
//! start at zero and address parameters first, then locals in declaration order.
//! Both can be assigned.
//! `call` takes an unsigned LEB32 index into the exact function declarations,
//! including forward and self references. `call_indirect` takes an unsigned
//! LEB32 type index followed by the literal reserved byte `0x00` (not a LEB32
//! table index). It requires a defined table even in unreachable code; a
//! defined zero-entry table qualifies. The control constructs may nest together.
//! `if` and `block` accept block type byte `0x40` (void) or one result type:
//! `0x7f` (i32), `0x7e` (i64), `0x7d` (f32), or `0x7c` (f64). They do not
//! accept block parameters. A void `if` may omit `else`; a value-producing
//! `if` requires it. `loop` accepts only `0x40`.
//! `br` and `br_if` take an unsigned LEB32 label depth: zero names the innermost
//! frame, and each enclosing frame, including `if`, adds one to the depth. A
//! branch may cross any of these frames and target an `if`, a `block`, a `loop`,
//! or the implicit function label (depth equal to the number of open explicit
//! frames). Both arms of an `if` share one label. `return` has no immediate and
//! always targets the function label. `br_table` takes an unsigned LEB32 count
//! (0 to 16), exactly that many unsigned LEB32 label depths, and a mandatory
//! unsigned LEB32 default depth. All listed labels and the default must have
//! the same label result: void (including a loop continue), i32, i64, f32, or f64.
//! Other block types and typed `select` are unsupported.
//!
//! The optional table section (ID 4) follows any function section and precedes
//! any memory, global, export, start, element, and code sections. It may be
//! empty (a zero-length vector) or declare one private `funcref` table, using
//! literal element type byte `0x70`. Its limits use flag byte `0x00` (minimum only) or
//! `0x01` (minimum and maximum), followed by unsigned LEB32 entry counts. The
//! minimum and any explicit maximum are at most 16, and the maximum must not
//! be less than the minimum. The minimum is the fixed instance size, which may
//! be zero. The table cannot be imported, exported, or mutated after
//! instantiation. Guest code can only use it as the target of `call_indirect`.
//!
//! If present, the memory section follows any function and table sections and
//! precedes any global, export, start, element, and code sections. It may be
//! empty (a zero-length vector) or declare one private, unshared memory32. The
//! limits use flag byte `0x00` (minimum only) or `0x01` (minimum and maximum), followed by
//! unsigned LEB32 page counts. The minimum and any explicit maximum must be zero
//! or one 64-KiB page, and the maximum must not be less than the minimum. The
//! minimum is the initial size; an omitted maximum has an effective limit of one
//! page. Loads, stores, `memory.size`, and `memory.grow` require a defined
//! memory, even in dead code; a zero-page memory qualifies. Loads and stores
//! take an unsigned LEB32 alignment exponent and an unsigned LEB32 byte offset.
//! Valid padding is accepted for both fields. The exponent must not exceed the
//! natural alignment of the access width: 0, 1, 2, or 3 for 1, 2, 4, or 8 bytes.
//! `memory.size` and `memory.grow` take a literal reserved byte `0x00`, not a
//! general LEB32 index. Other memory instructions are unsupported.
//!
//! The optional global section (ID 6) follows any type, function, table, and
//! memory sections and precedes any export, start, element, and code sections.
//! It may contain zero to eight private globals total, indexed from zero in
//! declaration order. Each declaration uses a value type byte (`0x7f` for i32,
//! `0x7e` for i64, `0x7d` for f32, or `0x7c` for f64) and a literal mutability
//! byte (`0x00` for immutable or `0x01` for mutable). The initializer is exactly
//! the matching `i32.const`, `i64.const`, `f32.const`, or `f64.const`, followed
//! by `end`. Integer immediates are signed LEB32 or LEB64 with valid padding
//! accepted; float immediates are four or eight little-endian bytes. The
//! initializer cannot read past the global section frame. Every body can read
//! any declared global and can set only mutable globals. Global imports,
//! exports, and initializer reads are unsupported.
//!
//! The optional start section (ID 8) comes after any exports and before any
//! elements, code, and data. Its entire payload is one unsigned LEB32 index
//! (valid padding is allowed) into the defined function declarations. The
//! referenced function must take no parameters and return no value; it may be
//! private or exported.
//!
//! The optional element section (ID 9) follows any table, memory, global,
//! export, and start sections and precedes any code and data sections. It may
//! contain zero to eight active segments, retaining at most 64 function indices total,
//! counting repeats and overlaps. Each segment requires a defined table and
//! encodes the Core 1.0 table index zero as unsigned LEB32, exactly
//! `i32.const <signed LEB32> end`, and an unsigned LEB32 count followed by that
//! many unsigned LEB32 indices into the defined function declarations. Padded
//! LEB32 encodings are accepted if valid. Functions of any supported signature
//! can be referenced. An element section with no segments does not require a table;
//! a segment with no function indices still does. Passive and declarative
//! segments, element expressions, and global offsets are unsupported.
//!
//! The optional data section (ID 11) follows any memory, global, export, start,
//! and code sections. It may contain zero to eight active segments with at most
//! 64 KiB of payload bytes total, counting overlaps. Each segment requires a defined
//! memory and encodes the Core 1.0 memory index zero as unsigned LEB32, exactly
//! `i32.const <signed LEB32> end`, and a length-prefixed vector of raw bytes.
//! An empty segment vector does not require memory. Passive segments, bulk
//! memory, and global offsets are unsupported.
//!
//! An optional export section after any table, memory, and globals and before
//! any start, elements, and code may contain zero to eight function exports.
//! Tables, memory, and globals cannot be exported. Name matching is exact (no
//! Unicode normalization), and duplicate export names are rejected. Each export
//! refers to a defined function by its zero-based declaration index. Exports
//! can use any supported function signature. Distinct names may refer to the
//! same function.
//! Both export and custom names must be UTF-8.
//! Custom payloads after the name are opaque and discarded, even for well-known
//! names. All other standard sections (including import and data count)
//! are rejected, even when empty.
//!
//! Parameters do not start on the operand stack. Validation starts with an empty
//! stack and checks each stack effect: `local.get` pushes the indexed slot's
//! declared type; `local.set` stores and consumes a value of that type, and
//! `local.tee` stores it and leaves it on the stack. These moves preserve all
//! bits, including the sign of zero and signaling NaN payloads.
//! `global.get` pushes the declared type; `global.set` consumes that type.
//! Like local moves, global reads and writes preserve all bits. Local and
//! global stack effects are checked even in dead code.
//! `i32.const` pushes one i32.
//! `i64.const` pushes one i64 and accepts valid padding within ten signed LEB128
//! bytes. In a ten-byte encoding the terminal byte must be `0x00` or `0x7f` to
//! match the sign bit. The immediate cannot read beyond its body frame.
//! In function bodies, `f32.const` (`0x43`) and `f64.const` (`0x44`) read
//! exactly four and eight little-endian bytes respectively, without crossing
//! the body frame. Each pushes one operand holding the exact IEEE 754 bits.
//! The four reinterpretations (`0xbc` through `0xbf` in the order listed above)
//! consume one operand and change only its type, between i32 and f32 or between
//! i64 and f64. In contrast, `f64.promote_f32` (`0xbb`) and `f32.demote_f64`
//! (`0xb6`) consume one float and convert its numeric value to the other width.
//! Promotion is exact for all finite inputs, including subnormals. Demotion
//! rounds to nearest with ties to even, including at subnormal and overflow
//! boundaries; underflow to zero and overflow to infinity retain the input sign.
//! Both operations preserve signed zeros and infinities and map every NaN to
//! the destination width's positive canonical quiet NaN, discarding its sign
//! and payload.
//! The integer-to-float conversions (`0xb2`–`0xb5` for f32 and `0xb7`–`0xba`
//! for f64, in the order listed above) consume one `i32` or `i64`. The suffix
//! `_s` interprets the operand as signed, and `_u` interprets its bits as an
//! unsigned integer. Conversion rounds directly to the nearest value of the
//! destination width, with ties to even, using only integer arithmetic. Zero
//! produces positive zero and all results are finite. Each conversion costs
//! one dispatch.
//! The trapping float-to-integer conversions (`0xa8`–`0xab` for i32 and
//! `0xae`–`0xb1` for i64, in the order listed above) consume one float of the
//! specified width. They truncate toward zero using only integer arithmetic
//! on the raw bits, then check the resulting integer against the destination
//! range (`_s` signed, `_u` unsigned). Both zero signs, subnormals, and any
//! other input with magnitude less than one produce integer zero, including
//! negative inputs for `_u`. A value below the signed minimum is accepted if
//! it truncates to that minimum. Any NaN traps with
//! [`Trap::InvalidConversionToInteger`]; either infinity or a truncated value
//! outside the destination range traps with [`Trap::IntegerOverflow`]. Each
//! conversion costs one dispatch; exhaustion is checked before it can trap.
//! Post-Core-1.0 saturating conversions (`trunc_sat`) are unsupported.
//! For both widths, `abs` clears the sign bit and `neg` flips it;
//! each consumes one operand and produces one of the same type. `copysign`
//! consumes two operands of its width, taking the magnitude bits from the first
//! (lhs) and only the sign bit from the second (rhs). Sign operations leave all
//! non-sign bits, including NaN payloads, unchanged. The arithmetic rounding ops
//! `ceil`, `floor`, `trunc`, and `nearest` (`0x8d`–`0x90` for f32, `0x9b`–`0x9e`
//! for f64) each consume one operand and produce a result of the same width.
//! They round to an integral floating-point value toward positive infinity,
//! negative infinity, zero, or the closest integer (ties to even), respectively.
//! They preserve the input sign even when rounding to zero, and leave infinities
//! and finite integral values unchanged. For any NaN, Swayze deterministically
//! returns the positive canonical quiet NaN (`0x7fc00000` for f32,
//! `0x7ff8000000000000` for f64), not the input NaN's sign or payload.
//! `sqrt` (`0x91` for f32, `0x9f` for f64) consumes one operand and produces
//! a result of the same width. Both signed zeros and positive infinity are
//! unchanged; negative nonzero inputs and all NaNs return the positive canonical
//! quiet NaN above. Positive finite inputs, including subnormals, are rounded
//! to nearest (ties to even; no exact midpoint is possible). The square root
//! uses exact integer arithmetic with a radicand of at most 106 bits and at most
//! 24 (f32) or 53 (f64) restoring steps within one guest dispatch.
//! `add` and `sub` (`0x92`/`0x93` for f32, `0xa0`/`0xa1` for f64) consume two
//! operands of their width and return a result of that width, in one dispatch.
//! `sub` uses the opposite sign for the right operand. With those effective
//! signs, two negative zeros give negative zero, other zero pairs and exact
//! cancellation give positive zero, and conflicting infinities give the positive
//! canonical quiet NaN. Any NaN gives that same encoding; all other infinite
//! operands determine the infinity result, with their effective sign.
//! Finite arithmetic either computes the exact aligned magnitude in a scalar
//! `u128` (at most 108 bits), or returns the larger operand's magnitude when
//! the smaller is strictly below both adjacent half-gaps: the inward spacing
//! at a power of two can be half the outward spacing, and the outward endpoint
//! above max finite is the hypothetical next power of two. No bits are lost
//! before cancellation; the exact result rounds once to nearest, ties to even.
//! Subnormal results are exact multiples of the least subnormal, and rounding
//! can carry the significand into the next exponent or to infinity.
//! `mul` (`0x94` for f32, `0xa2` for f64) consumes two operands and returns
//! a result of the same width. For finite nonzero inputs it uses a full exact
//! integer significand product (at most 48 or 106 bits) and rounds once to the
//! destination, nearest with ties to even, including gradual underflow and
//! overflow to infinity. The result has the XOR of the operand signs, including
//! zero and infinity, except that any NaN or zero times infinity returns the
//! positive canonical quiet NaN.
//! `div` (`0x95` for f32, `0xa3` for f64) divides the first operand by the
//! second and returns a result of their width. For finite nonzero inputs it
//! rounds an exact integer ratio at the destination precision, nearest with ties
//! to even, including gradual underflow and overflow. Any NaN, zero divided by
//! zero, or infinity divided by infinity returns the positive canonical quiet
//! NaN. Otherwise the result sign is the XOR of the operand signs; dividing a
//! nonzero value by zero returns signed infinity, not a trap. An infinity
//! numerator gives signed infinity, and a zero numerator or infinity denominator
//! gives signed zero in the remaining cases. Each `div` uses one guest dispatch.
//!
//! In a normal binade, the exact quotient's normalized significand is at most
//! `2 - 1/H`, where `H = 2^h` for `h` fraction bits. For normalized integer
//! significands `H <= a,b < 2H`, either `a >= b` and `a/b <= (2H-1)/H`, or
//! `a < b` and `2a/b <= 2(2H-2)/(2H-1) < (2H-1)/H`. Thus division cannot
//! land in the gap above the last finite value of that binade, including the
//! overflow half-gap above max finite. Overflow requires an exact magnitude
//! of at least `2^(emax+1)`, where `emax` is the largest normal exponent.
//!
//! Below the normal range the destination quantum is the least subnormal,
//! and exact ties are possible. Half of the least subnormal rounds to signed
//! zero; the midpoint between the greatest subnormal and least normal rounds
//! to the least normal.
//!
//! `min` and `max` (`0x96`/`0x97` for f32, `0xa4`/`0xa5` for f64) consume two
//! operands of their width and return the exact encoding of the lesser or
//! greater operand, respectively. Mixed-sign zeros return -0 for `min` and +0
//! for `max`; same-sign zeros retain their sign. If either operand is a NaN,
//! both instructions return the same positive canonical quiet NaN as rounding.
//! For both widths, `eq`, `ne`, `lt`, `gt`, `le`, and `ge` (`0x5b`–`0x60` for f32,
//! `0x61`–`0x66` for f64) consume two operands of their exact width in lhs, rhs
//! order and push an i32 Boolean (0 or 1). Either quiet or signaling NaN makes
//! the pair unordered:
//! only `ne` returns 1. Positive and negative zero compare equal; all other
//! non-NaNs follow numeric order, including subnormals and infinities. Comparisons
//! use raw bits without changing the input encodings. Constants, reinterpretations,
//! width conversions, sign operations, rounding, square root, `mul`, `min`/`max`,
//! and comparisons each count as one dispatch. None uses native floating-point
//! operations. Floats are supported as operands, private globals, locals, `block`
//! and `if` results, and function parameters and results, including across the
//! host boundary. Fallthrough, `return`, and branch exits preserve all result bits.
//! Loop results and floating-point numeric instructions beyond those listed
//! remain unsupported.
//! The two extensions pop an i32 and push an i64,
//! sign-extending for `_s` and zero-extending for `_u`. Wrapping pops an i64
//! and pushes its low 32 bits as i32. `i64.eqz` consumes one i64 and pushes
//! one i32. The other i64 comparisons consume two i64s and push one i32;
//! i64 arithmetic and binary bit operations consume two i64s and push one i64.
//! `i64.clz`, `i64.ctz`, and `i64.popcnt` each consume and produce one i64.
//! i64 values can be operands (including load results and stored values),
//! locals, private globals, `block` and `if` results, and function parameters
//! or results, including across the host boundary.
//! `nop` has no stack effect; `drop` discards an operand of any supported type.
//! Untyped `select` (`0x1b`) consumes two candidates of the same type and an
//! i32 condition, pushed in first-candidate, second-candidate,
//! condition order above the active floor. In one dispatch it pushes the first
//! candidate for any nonzero condition and the second for zero. Unlike `if`,
//! `select` does not skip either candidate's computation; a producer may trap
//! even if its result would be unselected or dropped.
//! `i32.eqz`, `i32.clz`, `i32.ctz`, and `i32.popcnt` each consume one i32 and
//! push one i32; the i32 binary operators consume two i32s and push one i32.
//! `call` consumes arguments in declared parameter order (the top operand
//! is the last parameter) and pushes the declared result, if any.
//! `call_indirect` consumes an i32 selector above the arguments and uses the
//! immediate's signature for the argument and result types. Both calls check
//! arity and each positional type above the active frame's entry floor, and
//! any result is concrete even when the floor supplied implicit arguments.
//! Loads consume an i32 base address and push a value of the opcode's type
//! (i32, i64, f32, or f64). Stores consume an i32 base address and a value of the
//! opcode's type (value on top) and produce no result. `memory.size`
//! takes no operands and pushes the instance's actual page count (0 or 1).
//! `memory.grow` consumes an i32 page delta as unsigned and pushes the old
//! page count on success or `-1` on failure. A zero delta always succeeds
//! without allocating; the only permitted positive growth is from zero to
//! one page if the maximum allows it. That transition fallibly reserves a
//! single 64-KiB page and initializes it to zero. An excessive delta or a
//! failed reserve leaves memory unchanged and does not trap.
//! `if` pops an i32 condition in the enclosing frame and saves the remaining
//! depth as the floor for both arms. `block` and `loop`
//! save the current depth as their floor without consuming any operands.
//! For validation, `unreachable`, `br`, `br_table`, and `return` discard
//! operands above the active floor and mark only that frame unreachable, after
//! checking any required operands. A pop at the floor of an unreachable frame
//! supplies an implicit operand of the required type without reducing the depth.
//! For example, a `local.tee` at that floor still pushes a concrete operand of
//! the local's declared type. In a dead tail, any known candidate type determines
//! the `select` result; mismatched known candidates still fail at `select`.
//! Without a known candidate type, the result is unknown and can satisfy any
//! supported operand type. Every push, including an unknown result, occupies a slot and counts
//! against the stack cap. No operand access crosses the active floor.
//! At `else` or `end`, an unreachable frame may supply a missing result
//! implicitly, but a known result of the wrong type or any excess operands
//! are still rejected. Every new frame starts reachable,
//! even inside an unreachable frame. `else` checks the then result and starts
//! a reachable else arm at the same floor. `end` checks the final result, then
//! leaves the declared result on the parent's stack (none for void) without
//! changing the parent's reachability flag.
//! A transfer to a value-producing label requires an operand of the declared
//! type; transfers to void labels require no result. `br_if` pops an i32
//! condition above any result, and `br_table` pops an i32 selector above any
//! common result. Validation checks these pops against the active floor, even
//! for an outer target. Only `br_if`
//! restores the result for fallthrough, and execution with a zero condition pops
//! only the condition and continues. `br_table` treats its selector as `u32`:
//! it selects the zero-based explicit entry, or the default if out of range.
//! A taken branch to a `block` or `if` truncates to the target's floor, keeps
//! the full tagged result if required, and resumes after the target's `end`.
//! A taken loop branch truncates to the target loop's floor and resumes at its
//! body start. Both preserve the target's operand prefix and locals. A function
//! exit discards all callee operands except the declared result, if any, and
//! completes the call.
//! Both `if` arms and the entire `br_if` fallthrough path are validated regardless
//! of the condition. Unreachable code is fully decoded and checked, including
//! all immediates, indices, mutability, delimiters, and limits.
//! The function has a floor of zero and its own unreachable flag; its `end`
//! must be the final byte of the body frame and checks for the declared result
//! type and arity, including that a void function leaves no operands. The inclusive
//! limits are 256 body bytes (including the locals vector and all delimiters),
//! 127 decoded instructions (including `unreachable`, `nop`,
//! `drop`, `select`, `if`, `else`, `br`, `br_if`, `br_table`, and `return`, but
//! not `block`, `loop`, or `end`), 32 stacked operands including unknown results
//! and operands below floors, and 16 simultaneously open `block`,
//! `loop`, and `if` frames combined, excluding the function frame.
//!
//! Validation resolves every function's type index and validates all global
//! declarations before decoding any body. It checks the entire input, even
//! unexported bodies and untaken arms. It returns an immutable, owned [`Module`]
//! with private decoded bodies (including signatures and local types), named
//! export entries, an optional start index, the table size and declared memory
//! limits (if defined), global initial values, and active element and data
//! segments. Validation does not allocate linear memory.
//! A separate fallible [`Module::instantiate`] step
//! checks all segment ranges against the initial sizes, before initializing
//! either table or memory. Growth cannot make an out-of-bounds active data
//! segment valid. The first out-of-bounds element segment takes precedence over
//! the first out-of-bounds data segment,
//! and both take precedence over memory allocation failure. Offsets are
//! interpreted as `u32` bits and widened before adding the segment length.
//! An empty segment may
//! start at the end of its table or memory, but not beyond it. Out-of-bounds
//! segments are reported by zero-based source index as
//! [`InstantiationError::ElementSegmentOutOfBounds`] or
//! [`InstantiationError::DataSegmentOutOfBounds`], not validation errors or
//! invocation traps. The fixed private table starts with all entries null;
//! fresh linear memory starts with all bytes zero. Segments of each kind are
//! applied in source order; the last write to an entry or byte wins. Global
//! initial values are copied into fixed private slots with no allocation.
//! Only after preflight, allocation, and all initialization succeed does it run
//! the optional start function once by index, with no host authority and with
//! the same execution budgets as an explicit invocation. Completed start stores
//! and memory growth are visible on success. A guest trap
//! ([`InstantiationError::StartTrap`]) or
//! host execution limit ([`InstantiationError::StartLimitExceeded`]) discards
//! the unpublished instance. No instantiation failure exposes an instance.
//! [`Instance::invoke_with_values`] accepts tagged [`Value`] arguments and
//! returns `None` for void or the exact tagged numeric result. Float arguments
//! and results carry raw IEEE 754 bits, not native floats. The i32-argument
//! methods share the same preflight: scan at most eight export names for an
//! exact match, check arity, check the declared result kind, then check each
//! positional argument type from first to last. There is no numeric coercion.
//! Any preflight error returns before executing a guest instruction
//! or changing guest state. Execution does not recurse in Rust. The only guest
//! execution allocation is the fallible single-page
//! reserve for an eligible `memory.grow`, during start or invocation. After a
//! page is installed, it is never resized or reallocated. A failed reserve can
//! be retried by a later dispatch within the same execution limits. Zero selects the else arm (or skips straight to the join if
//! no `else` is present); every nonzero i32 selects the then arm.
//! Validation uses a fixed stack of operand types and a fixed stack of control
//! frames with declared result types (if any).
//! Each `block` and `if` label keeps a linked chain of incoming branch edges in
//! fixed scratch storage, patched once per edge at `end`. Edges are identified by
//! slot, so a table can refer to several labels and repeat the same label. The
//! chain survives `else` and is distinct from the jumps that skip untaken `if`
//! arms. These forward edges target strictly later decoded instruction indices
//! or the one-past-last index in the same body. Loop continues target the loop's
//! saved body start at or before the branch. Each branch stores its target's
//! operand floor; a `block` or `if` exit also records the target's result type,
//! if any.
//! `return` and branches to the function label share an exit target at the
//! one-past-last instruction index.
//! Each table decodes to one instruction that indexes a separate per-body array
//! of resolved edges. Each table occupies consecutive slots starting at its
//! base, with the default at `base + explicit_count`. Dispatch clamps the `u32`
//! selector to `explicit_count` (at most 16) before converting it to an index;
//! adding that index to the base cannot go past the stored default. Validation
//! retains at most 256 edges per body because each depth consumes at least one
//! of the 256 body bytes. The scratch fixup storage has one link slot per
//! decoded instruction and one per possible table target; execution never uses
//! it. Validation retains decoded code only after all edges have been resolved
//! and the exact body frame has been checked.
//! Execution does not dispatch instructions skipped by a control transfer.
//! Each active call frame has its own fixed operand stack and separate fixed
//! local storage. Operand, local, and instance global slots use the same
//! private tagged values, including raw f32 and f64 encodings. Each frame starts
//! by copying a fixed array of typed zeros, including the exact +0 encoding
//! for every float local, even when reusing a frame slot. Tagged arguments
//! then fill the parameter prefix; floats retain their exact bits across the
//! call. A callee starts with no operands. A callee returning a value pushes
//! the full tagged result onto the caller's remaining operands; a void callee
//! leaves them unchanged. At the host invocation boundary, the private tagged
//! values and public [`Value`] map directly without re-encoding any bits.
//! `call_indirect` interprets the selector as `u32` and traps with
//! [`Trap::TableOutOfBounds`] for a selector outside the table's logical length,
//! [`Trap::UninitializedElement`] for a null entry, or
//! [`Trap::IndirectCallTypeMismatch`] if the selected function's parameter
//! count, any positional parameter type, or result differs from the expected
//! signature. Type indices need not be equal if the signatures match. A
//! successful lookup uses the same frame machinery as `call`.
//! [`Instance::invoke_i32`] and [`Instance::invoke_void`] are equivalent to
//! passing an empty argument slice to their respective typed methods.
//! The top operand is the right-hand side of each binary operation. At either
//! width, `eqz` tests for zero, and `eq` and `ne` compare all operand bits for
//! equality or inequality. Ordering comparisons interpret the bits as signed
//! for `_s` and unsigned for `_u`, at the input width. These tests and
//! comparisons do not trap and return an i32 `1` if true or `0` if false.
//! Addition, subtraction, and multiplication wrap modulo 2³² for i32 and 2⁶⁴
//! for i64. No i64 numeric operations beyond those listed above are supported.
//! Signed division truncates toward zero, and a nonzero signed remainder
//! has the sign of the dividend. Unsigned division and remainder interpret
//! the operand bits as `u32` or `u64` at the input width and preserve the
//! result bits in the returned value.
//! The bit operations do not trap. At either width, `clz` and `ctz` count leading
//! and trailing zero bits (both return the input width for zero); `popcnt`
//! counts set bits. `and`, `or`, and `xor` operate independently on each pair of
//! bits. Shifts and rotates use only the low five bits of the i32 right operand
//! or the low six bits of the i64 right operand, even for negative counts. `shl`
//! shifts left, discards bits shifted out of the input width, and fills with
//! zeros. `shr_s` fills with the sign bit and `shr_u` fills with zeros. `rotl`
//! and `rotr` wrap the shifted-out bits to the opposite end in the indicated
//! direction.
//! Loads and stores add the base bits interpreted as `u32` and the `u32` offset
//! in `u64`, without wrapping at 32 bits. The entire 1-, 2-, 4-, or 8-byte range
//! must be in bounds, or the instruction returns [`Trap::MemoryOutOfBounds`]
//! without reading or writing any bytes. Accesses are little-endian and may
//! be unaligned; validated alignment annotations do not constrain addresses.
//! Narrow loads sign-extend (`_s`) or zero-extend (`_u`) to the opcode's
//! result type. Narrow stores write only the low 8, 16, or (for `i64.store32`)
//! 32 bits of the value. Float loads and stores move the full 32- or 64-bit
//! encoding without floating-point arithmetic or conversion, preserving signed
//! zeros and signaling NaN payloads. They share the same bytes as integer
//! accesses. Each load or store uses one dispatch.
//!
//! A zero right operand raises [`Trap::IntegerDivideByZero`] for all division
//! and remainder instructions at either width. Signed division of the minimum
//! value by `-1` raises [`Trap::IntegerOverflow`]; signed remainder for those
//! operands returns zero. Executing `unreachable` immediately raises
//! [`Trap::Unreachable`].
//! These are runtime outcomes, not validation errors. [`InvokeError::Trap`]
//! or [`InstantiationError::StartTrap`] reports the first guest trap, skipping
//! all remaining instructions, including in callers. Host resource limits are
//! separate: [`InvokeError::LimitExceeded`] or
//! [`InstantiationError::StartLimitExceeded`] stops the execution before it
//! would exceed either 16 live frames (including the entry function) or 1024
//! total instruction dispatches. Every executed decoded instruction costs one dispatch,
//! including `unreachable`, `nop`, `drop`, `select`, local, memory, and global
//! instructions, either kind of call, an `if` test, an executed `else` jump,
//! a `br`, a `br_if` with either outcome, a `br_table` regardless of its length
//! or selected target, or a `return`.
//! An omitted `else` introduces no dispatch. Skipped instructions, `block`,
//! `loop`, and `end` delimiters, and unwinding a completed call frame cost nothing.
//! A continue does not reset the budget: all loop
//! iterations and calls share it, including recursive guest calls.
//! An invocation can return just after its last allowed dispatch. If the next
//! instruction would exceed the dispatch budget, that limit takes precedence
//! over executing it. For an indirect call with a dispatch available, the
//! bounds, null, and signature checks take precedence over the call-depth limit.
//! Each explicit invocation has fresh call-local state and budgets, including
//! the first one after a successful start. All functions in an instance share
//! its memory and globals. Completed memory and global stores and memory growth
//! persist across calls, including explicit invocations that trap or hit a host
//! limit.
//! Separate instances have independent tables, memory, and globals. Empty,
//! custom-only, and other unexported modules have no callable exports.
//! [`prepare`] remains a stub.
//!
//! # Evidence and gaps
//!
//! Local tests exercise framing, UTF-8, size limits, section order and contents,
//! function/type/code and export relationships, parameter types and counts,
//! exact invocation arity, bounded LEB32 decoding (including local counts and
//! indices), local declaration types and limits, each stack effect and body
//! budget, reads and writes in the combined local index space, fresh locals on
//! successive calls, wrapping arithmetic after discarding the input, signed and
//! unsigned division and remainder edge cases, ordered name, arity, and result
//! kind checks before execution, and stopping at the first guest trap and
//! reusing the instance.
//! Multi-function tests check dispatch by declaration index, exact UTF-8 names
//! and aliases, shared and mixed arities, traps and fresh locals across dispatches,
//! duplicate names, exact type and function index bounds, independent count caps,
//! malformed unused types, and later unexported bodies and their frame boundaries.
//! Direct-call tests cover nested and repeated calls, forward and self targets,
//! calls between different signatures, argument order, frame isolation, bounded
//! index decoding, underflow at both arm floors, zero-argument calls at the
//! operand cap, and propagation and reuse after traps or execution limits.
//! Budget tests exercise the last allowed frame and dispatch, the next
//! attempted frame and dispatch, and budget sharing across completed callees.
//! Comparison tests cover exact 0/1 results, equal operands and signed versus
//! unsigned boundary values, unary and binary underflows at the function and
//! arm floors, conditional selection, and comparisons at the instruction and
//! operand limits.
//! Bit-operation tests distinguish each opcode using exact 32-bit results.
//! They cover zero, the high bit, all bits set, masked counts including negative
//! values and `i32::MIN`, signed and unsigned right shifts, preserved operand
//! prefixes, underflow at control floors (including both untaken arms), and
//! exact decoded-instruction and dispatch budget boundaries.
//! Integer stack-operation tests check selection order for both widths, including i64
//! candidates with identical low words, and zero, positive, and negative i32
//! conditions. They check eager producer traps for either unselected candidate,
//! preserved operands across mixed control, exact underflow offsets including
//! in skipped code, and exact body byte, instruction, operand, and dispatch
//! limits, including limit precedence over a guest trap. Dead-tail tests check
//! mixed-candidate and condition diagnostics at `select`, use of unknown results
//! as either type or as inputs to later selects, concretization by a known
//! candidate, and depth and floor accounting for unknown results.
//! i64 operand tests check exact extended and wrapped values, signed LEB64
//! padding, terminal bytes and frame offsets, i32-only boundaries in live and
//! dead code, and preservation through control and calls. They distinguish
//! signed and unsigned comparisons with equal low words, verify all 64 result
//! bits for each wrapping arithmetic operation, and check concrete input and
//! result types in polymorphic tails. Boundary tests cover the shared 32-slot
//! stack (including frozen prefixes) and one dispatch per i64 opcode at the
//! 1024-dispatch limit.
//! Float operand tests check all four reinterpretations through exact integer
//! returns, including both zero signs and signaling and quiet NaN payloads.
//! They cover body- and section-framed truncation, delimiter-valued bytes,
//! type errors and polymorphic floors, candidate selection, preserved operand
//! prefixes, rejection of unsupported float features, and the shared body-byte,
//! instruction, operand, and dispatch caps. Size probes with the pinned toolchain
//! on arm64 macOS found unchanged operand slots (16 bytes), call frames (856
//! bytes each), and validation scratch layouts, including the decoded-instruction
//! array (4,064 bytes), in both debug and release.
//! Float sign-operation tests compare integer-reinterpreted results for both
//! signs of zero, infinities, normals, subnormals, and quiet and signaling NaNs.
//! They check `copysign` operand order, typed live and dead operands and floor
//! diagnostics, and shared stack, instruction, and dispatch limits.
//! Float comparison tests exercise every predicate and width over pairs in both
//! orders, including both zero signs, subnormal boundaries, opposite signs,
//! infinities, and both signs of quiet and signaling NaNs. They check exact i32
//! Booleans, operand and result types in live and dead code, underflow precedence
//! and polymorphic floors, shared caps, and exact fuel for each opcode.
//! Float rounding tests check exact result encodings for all four ops on both
//! widths, including neighbors of half ties and exponent boundaries, both zero
//! signs, subnormals, infinities, and canonicalization of quiet and signaling
//! NaNs. They check live and dead floor diagnostics, shared caps, and a one-dispatch
//! charge per op at the fuel limit. The fixed vectors are evidence, not a proof.
//! Float `min`/`max` tests check exact tagged results for ordered pairs in both
//! widths, including equal finite values, both zero signs, subnormals and
//! infinities, and positive canonical quiet NaNs for quiet or signaling NaNs in
//! either operand. They check type and floor diagnostics in live and dead code,
//! the shared body-byte, instruction, and operand limits, and exact dispatch
//! charging. Before/after layout probes with pinned Rust on arm64 macOS agreed
//! in debug and release: 16 bytes per operand, 32 per decoded instruction,
//! 856 per call frame, and 17,512 bytes summed across body-validation scratch.
//! Float square-root tests check exact bits at zeros, infinities, NaNs,
//! subnormal and normal boundaries, near rounding midpoints, and at the largest
//! finite input. Deterministic per-exponent samples compared with native roots
//! are evidence, not a proof. Validation checks include live and dead floors
//! and concrete operand and result types; execution checks the dispatch limit.
//! Float multiplication tests check exact midpoint and neighbor encodings at
//! normal, subnormal, and overflow boundaries, including underflow to signed
//! zero and a case that would double-round. They exercise all special-value
//! classes in both operand orders and signs, live and dead typing, and exact
//! dispatch charging. Deterministic finite pairs compared with native products
//! provide independent evidence, not a proof.
//! Float addition and subtraction tests pin signed zeros, infinities and NaNs,
//! cancellation and subnormal transitions, the exponent-gap cutoff, and both
//! sides and parities of ties, including carries at binades and overflow. They
//! also check live and polymorphic typing and exact dispatch charging. Native
//! sums and differences on deterministic finite pairs are evidence, not a proof.
//! With pinned Rust on arm64 macOS, before/after probes in debug and release
//! found unchanged sizes for the float binary op (1 byte), decoded instruction
//! (32), operand and public value (16 each), and call frame (856).
//! Float division tests cover the signed special-value cross product, normal
//! results on both sides of a rounding midpoint, overflow and gradual underflow,
//! and attainable subnormal ties and their neighbors. Exact vectors distinguish
//! single rounding from double rounding in both directions. The tests also check
//! live and dead typing and exact dispatch charging, including zero divisors.
//! Deterministic finite pairs compared with native division provide independent
//! evidence, not a proof.
//! Float width-conversion tests check exact round trips, signed zeros and
//! infinities, canonical NaNs, and both neighbors of ties at normal, subnormal,
//! underflow, and overflow boundaries. They also cover live and dead typing
//! and exact dispatch charging. Deterministic finite samples compared with
//! native casts provide independent evidence, not a proof.
//! Integer-to-float tests cover all eight opcodes, signed and unsigned bit
//! interpretations, ties and their neighbors, carry into the exponent, and
//! cases that would differ if rounded first to f64 and then to f32. They also
//! check live and dead typing and one dispatch per opcode at the fuel limit.
//! Direct Rust casts of deterministic samples supplement the fixed bit vectors.
//! Trapping float-to-integer tests cover all eight opcodes, exact neighbors of
//! signed and unsigned range boundaries, NaNs, infinities, both zero signs,
//! subnormals, and negative fractions. They check live and dead typing, fuel
//! precedence, and preservation of writes that executed before a trap.
//! Native truncation on fixed in-range inputs supplements the exact vectors.
//! Before/after probes with pinned Rust on arm64 macOS (debug and release)
//! measured the conversion enum growing from 2 to 3 bytes, with no change to
//! the decoded instruction (32), operand (16), call frame (856), or trap (1).
//! Mixed-local tests check parameter-first slot order across typed and empty
//! groups, exact count and index bounds, full-width reads and writes, and
//! concrete types in reachable and polymorphic code. They exercise fresh
//! zero-initialization and frame isolation on repeated direct and indirect
//! calls, and one dispatch per local instruction at the fuel boundary.
//! Float-local tests interleave both float and integer widths after integer
//! parameters, check exact zero and stored bits through reinterpretation, and
//! check isolation across repeated invocations and recursion. They also cover
//! live and dead local index and type diagnostics, stack floors, and shared
//! declaration, operand, and dispatch limits. With the pinned compiler on
//! arm64 macOS, `size_of` probes before and after float locals agreed in debug
//! and release: 320 bytes for 20 local slots, 856 bytes per call frame (13,696
//! for 16), and 17,512 bytes summed across body-validation scratch (local zeros,
//! operand types and depth, control frames, instructions, table edges, and
//! fixup links). These are layout observations, not cross-target guarantees.
//! i64 bit-operation tests compare full-width results using `i64.eq`. They
//! distinguish high-bit patterns and sign filling, check count endpoints and
//! low-six-bit masking (including negative counts), and exercise typed input and
//! result checks, dead-tail polymorphism, and exact dispatch charging per opcode.
//! Conditional tests exercise zero and nonzero conditions, nested joins,
//! preserved outer operands and selected-arm effects, suppression of untaken
//! traps, rejection of untaken invalid code, arm result counts and floor
//! violations (even when depth would later recover), delimiter errors, and
//! control, instruction and operand bounds.
//! Block and branch tests cover taken exits across mixed frame kinds, separate
//! fixup chains for inner and outer blocks, stack truncation versus fallthrough,
//! skipped traps and rejected skipped syntax, padded and out-of-range depths,
//! delimiter errors, and exact body and execution budgets.
//! i64 control tests check full-width results through both `if` arms, normal
//! `block` exits, and taken and untaken branches. They check typed table labels,
//! arm and transfer validation at the active floor, counted unknown operands,
//! and shared body, control, operand, and dispatch limits.
//! Float control tests check exact negative-zero and signaling NaN bits through
//! both `if` arms, normal block exits, and `br`, `br_if` (taken and untaken),
//! and `br_table` (explicit and default). They check typed table diagnostics at
//! the incompatible label immediate, active floors and concrete dead-tail
//! results, and the unchanged body, stack, control, table, and dispatch ceilings.
//! Loop tests cover outer continues across mixed frame kinds, preserved locals
//! and stack prefixes, different taken and fallthrough stack effects, full body
//! validation, shared caps, and the exact dispatch boundary in a called nested
//! loop and instance reuse after hitting the limit. Transfers check operand
//! underflow against the innermost floor even on an untaken path.
//! Void-control tests cover omitted, empty, and nonempty `else` arms, joins at
//! enclosing delimiters and function exit, unchanged result-`if` diagnostics,
//! zero-result branch exits across mixed frames and preserved operand prefixes,
//! floor and depth errors even in skipped code, shared inclusive caps, exact
//! dispatch charging across calls and loops, and instance reuse.
//! `unreachable` tests cover implicit operands and results, truncation to the
//! current floor, concrete result counts, fresh inner frames and alternate
//! arms, and preservation of the parent's flag. They also check syntax, indices,
//! mutability, and limits in dead code, taken branches that bypass the trap,
//! trap propagation across calls, store persistence, repeated invocation, and
//! precedence of the dispatch limit over a trap beyond the budget.
//! `br` tests exercise mixed `br`/`br_if` fixups for nested targets, void and
//! i32 exits across frame kinds, active-floor operand checks and polymorphic
//! dead tails, shared label diagnostics and instruction caps, and loop back
//! edges followed by an exit on the last allowed dispatch.
//! If-label tests cover shared fixups from both arms, exits across intervening
//! frames, omitted `else`, operand floors and reachability in dead tails, and
//! exact admission and dispatch limits.
//! Branch-table tests cover mixed target kinds and floors, case and default
//! selection (including empty tables and repeated labels), framed LEB32 errors,
//! arity agreement, dead-tail validation, and exact table, body, and dispatch
//! limits.
//! Function-exit tests cover implicit label depths through mixed control, result
//! preservation on conditional fallthrough, active-floor and dead-tail checks,
//! caller resumption, and exact decoded-instruction, dispatch, and call limits.
//! Void-function tests cover padded zero result counts and export indices,
//! mixed calls to private and exported helpers, caller operand and local
//! preservation, and shared state. They exercise fallthrough, `return`, `br`,
//! `br_if`, and `br_table` function exits, table typing against void loop labels
//! and i32-result labels, exact operand and result errors at active floors and
//! delimiters, and polymorphic dead tails without synthetic results. Typed
//! export tests cover aliases, error precedence in both directions, no memory
//! or global writes on a result mismatch, and persistence of stores before a
//! guest trap. Recursive void calls and free completion and resumption are
//! checked at the exact frame, operand, and dispatch bounds.
//! Memory tests cover exact limits, width-specific alignment and framed memarg
//! decoding, actual page counts and the reserved `memory.size`/`memory.grow`
//! bytes, absent and zero-page memory, ordering, unsupported features, operand floors and
//! validation in untaken arms and unexported bodies, and zero initialization.
//! They exercise narrow-load sign boundaries, truncating stores, overlapping
//! little-endian and unaligned accesses, end-of-page accesses and straddles,
//! partial-store prevention, widened address arithmetic, independent instances,
//! and sharing across calls and exports. They also check store persistence
//! and suppression of later stores after guest traps or dispatch exhaustion.
//! i64 memory tests compare all 64 result bits, check i64 and i32 accesses to
//! overlapping bytes, and cover typed stack effects and all four access widths
//! before and after growth and at the exact instruction and dispatch caps.
//! Float memory tests check both zero signs and signaling NaN payloads against
//! independent integer accesses, including little-endian bytes at unaligned
//! addresses with natural or smaller hints. They cover framed memarg errors,
//! absent and zero-page memory, exact-end and overflowing addresses, no partial
//! stores, concrete types above live and dead floors, and the exact dispatch
//! boundary for each opcode.
//! Growth tests check optional maxima, unsigned deltas, reserve bounds and
//! failure, stack and dispatch limits, and persistence through exports, calls,
//! start, and traps. Start growth cannot bypass initial data preflight.
//! Data tests cover section order and framing, index and offset encodings, the
//! count and aggregate byte limits, absent and zero-page memories, full-range
//! preflight (including wrapping offsets and empty segments at the end), overlap
//! order, and reinitialization from the same module into independent instances.
//! Table and element tests cover exact caps, frame and padded LEB32 decoding,
//! malformed and unsupported encodings, validation of every referenced function,
//! absent versus zero-entry tables, null slots, overlap order, fresh instances,
//! full-range preflight, and coexistence and failure precedence with data.
//! Indirect-call tests cover exact immediate encoding, operand floors and caps,
//! parameter order, matching duplicate type declarations, void and i32 results,
//! target traps and their ordering against both budgets, and shared memory and
//! globals through indirect calls.
//! Mixed-signature tests cover full-width arguments and returns, structural
//! equality and same-arity mismatches, active floors and concrete results in
//! dead tails, and the unchanged void-only, parameterless start boundary.
//! Private float call tests include all four parameter tags, exact signaling NaN
//! and signed-zero bits across function exits, same-arity indirect mismatches,
//! padded export and start indices, and shared frame and dispatch budgets.
//! Host invocation tests check exact mixed arguments and tagged results, shared
//! side-effect-free preflight and error precedence, exact names and aliases,
//! and state persistence across successes, traps, and host limits.
//! Global tests cover section order, the shared declaration cap, exact typed
//! initializer expressions and framing, index decoding and bounds, mutability
//! and type-check precedence, operand floors and caps, and unsupported imports,
//! exports, and global offsets. They exercise full-width reads and writes,
//! shared state across start, exports and calls, fresh instances, and
//! suppression of later writes after a trap or exhausted dispatch budget.
//! Mixed global tests check exact signed-zero and signaling NaN bits using
//! integer reinterpretations, including writes by start and before traps.
//! Start tests cover section order, exact framing, padded and invalid indices,
//! signature errors and validation precedence, private and exported targets,
//! one automatic execution per instance, independence, and visibility of start
//! stores. They check that start sees initialized globals, data, and elements,
//! that preflight errors precede start traps, and that execution reports typed
//! traps or limits with the existing dispatch and frame bounds. A store before
//! a start trap does not leak into another attempt to instantiate. An explicit
//! invocation after start gets a fresh dispatch budget.
//! Parsing uses fixed scratch tables for up to eight type signatures, eight
//! resolved function signatures, and eight global declarations. It checks local
//! group counts against the remaining local budget without expanding groups,
//! uses bounded borrowed frames, and decodes instructions into a fixed-size
//! scratch array. After checking `end` and the complete body frame, it calls
//! `Vec::try_reserve_exact` for the count and copies those instructions. Body,
//! export, element segment, and data segment vectors are reserved fallibly
//! after checking their counts. Each name and segment payload is reserved
//! fallibly after checking its length, with separate aggregate limits for
//! element indices and data bytes. The count caps bound aggregate decoding to
//! eight body budgets and duplicate checks to at most 28 comparisons of bounded
//! names. There is no unsafe code, Rust recursion, or external dependency.
//! This is not full Core 1.0 validation or a conformance or security proof.
//! No upstream Wasm spec suite or fuzzing has been run.
//!
//! [WebAssembly Core 1.0]: https://www.w3.org/TR/2019/REC-wasm-core-1-20191205/

mod binary;
mod memory;
mod table;

pub use binary::{
    Error, ErrorKind, IntegerField, LengthField, Limit, ValType, Value, validate_module,
};

use memory::{DataSegment, LinearMemory, MemoryLimits};
use std::{cell::Cell, convert::Infallible, fmt};
use table::{ElementSegment, FunctionTable};

/// An owned, opaque module produced only by successful [`validate_module`].
///
/// ```compile_fail
/// use swayze_wasm::Module;
/// let forged = Module {
///     functions: Vec::new(), exports: Vec::new(), start_function: None,
///     memory_limits: None, data_segments: Vec::new(),
///     global_initial_values: std::array::from_fn(|_| unreachable!()),
///     table_entries: None, element_segments: Vec::new(),
/// };
/// ```
#[derive(Debug)]
pub struct Module {
    functions: Vec<binary::ValidatedCode>,
    exports: Vec<Export>,
    start_function: Option<usize>,
    // None means absent; Some may have a minimum of zero and no declared maximum.
    memory_limits: Option<MemoryLimits>,
    global_initial_values: [binary::Operand; binary::MAX_GLOBALS as usize],
    data_segments: Vec<DataSegment>,
    // None means absent; Some(0) means a declared zero-entry table.
    table_entries: Option<usize>,
    element_segments: Vec<ElementSegment>,
}

#[derive(Debug)]
struct Export {
    name: String,
    function_index: usize,
}

impl Module {
    /// Create an independent instance and run its optional start function.
    ///
    /// Check active segment ranges in source order within each kind: elements
    /// before data, using only the initial sizes. Neither table nor memory is
    /// initialized until all checks pass. An initial page is allocated fallibly
    /// and zeroed; an absent or initially zero-page memory allocates nothing at
    /// this step. A defined table uses fixed storage and starts all null.
    /// Segments of each kind are applied in source order.
    /// Typed global initial values are copied into fixed, independent instance slots.
    /// Once initialization succeeds, start (if specified) runs once with fresh
    /// execution budgets. Its stores and memory growth are visible on the
    /// returned instance.
    /// A start trap or execution limit discards the instance instead.
    pub fn instantiate(&self) -> Result<Instance<'_>, InstantiationError> {
        FunctionTable::preflight(self.table_entries.unwrap_or(0), &self.element_segments)?;
        LinearMemory::preflight(self.memory_limits, &self.data_segments)?;

        let memory = LinearMemory::new_preflighted(self.memory_limits, &self.data_segments)?;
        let table = self
            .table_entries
            .map(|len| FunctionTable::new_preflighted(len, &self.element_segments));
        let instance = Instance {
            module: self,
            memory,
            globals: self.global_initial_values.map(Cell::new),
            table,
        };
        if let Some(index) = self.start_function {
            binary::execute_start(&instance, index).map_err(|error| match error {
                binary::ExecutionError::Trap(trap) => InstantiationError::StartTrap(trap),
                binary::ExecutionError::LimitExceeded { limit } => {
                    InstantiationError::StartLimitExceeded { limit }
                }
            })?;
        }
        Ok(instance)
    }
}

/// Why creating an instance of a validated module failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstantiationError {
    /// The instance's initial linear memory could not be reserved.
    MemoryAllocationFailed,
    /// An active data segment does not fit in the initial linear memory.
    DataSegmentOutOfBounds {
        /// Zero-based index of the first out-of-bounds segment in source order.
        segment_index: usize,
    },
    /// An active element segment does not fit in the initial table.
    ElementSegmentOutOfBounds {
        /// Zero-based index of the first out-of-bounds segment in source order.
        segment_index: usize,
    },
    /// The start function or one of its callees trapped.
    StartTrap(Trap),
    /// The start execution would exceed a host resource bound, not a Core trap.
    StartLimitExceeded { limit: ExecutionLimit },
}

impl fmt::Display for InstantiationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MemoryAllocationFailed => {
                f.write_str("could not reserve WebAssembly linear memory")
            }
            Self::ElementSegmentOutOfBounds { segment_index } => {
                write!(
                    f,
                    "WebAssembly element segment {segment_index} is out of bounds"
                )
            }
            Self::DataSegmentOutOfBounds { segment_index } => {
                write!(
                    f,
                    "WebAssembly data segment {segment_index} is out of bounds"
                )
            }
            Self::StartTrap(trap) => write!(f, "WebAssembly start function trapped: {trap}"),
            Self::StartLimitExceeded { limit } => write!(
                f,
                "WebAssembly start execution {limit:?} limit exceeded (maximum {})",
                limit.maximum()
            ),
        }
    }
}

impl std::error::Error for InstantiationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::StartTrap(trap) => Some(trap),
            Self::MemoryAllocationFailed
            | Self::ElementSegmentOutOfBounds { .. }
            | Self::DataSegmentOutOfBounds { .. }
            | Self::StartLimitExceeded { .. } => None,
        }
    }
}

/// An instance of a validated module, with a private table, memory, and globals.
///
/// Invocation through `&self` can change this state. An instance is `Send`
/// (it can move to another thread when its module borrow permits), but not
/// `Sync`: sharing one instance between threads requires synchronization.
/// The immutable module can be shared to create independent instances.
#[derive(Debug)]
pub struct Instance<'module> {
    module: &'module Module,
    memory: LinearMemory,
    // Validation allows stores only to mutable slots and with the initial value's type.
    globals: [Cell<binary::Operand>; binary::MAX_GLOBALS as usize],
    table: Option<FunctionTable>,
}

impl Instance<'_> {
    /// Invoke the exact named export as a `() -> i32` function.
    /// See [`Self::invoke_i32_with_args`] for signature checks and runtime errors.
    ///
    /// ```
    /// use swayze_wasm::validate_module;
    /// let bytes = b"\0asm\x01\0\0\0\x01\x05\x01\x60\0\x01\x7f\x03\x02\x01\0\
    ///               \x07\x0a\x01\x06answer\0\0\x0a\x09\x01\x07\0\x41\x28\x41\x02\x6a\x0b";
    /// let module = validate_module(bytes)?;
    /// assert_eq!(module.instantiate()?.invoke_i32("answer")?, 42);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn invoke_i32(&self, name: &str) -> Result<i32, InvokeError> {
        self.invoke_i32_with_args(name, &[])
    }

    /// Invoke the exact named i32-result export, passing arguments in parameter order.
    ///
    /// Checks, in order, for a matching name ([`InvokeError::ExportNotFound`]),
    /// the declared argument count ([`InvokeError::ArityMismatch`]), an i32
    /// result ([`InvokeError::ResultMismatch`]), and that all parameters are
    /// i32 ([`InvokeError::ArgumentTypeMismatch`]). These checks happen before
    /// executing any guest instruction or changing memory or globals.
    /// Assignments to parameters do not affect the supplied slice, and declared
    /// locals are initialized to zero on each invocation. A guest trap stops
    /// execution and returns [`InvokeError::Trap`]; exceeding a host execution
    /// limit returns [`InvokeError::LimitExceeded`]. Later calls start with
    /// fresh frames and execution budgets but observe completed memory and
    /// global stores and memory growth, even before a trap or limit. The only
    /// allocation during execution is an eligible `memory.grow` attempting to
    /// reserve the single 64-KiB page of an initially empty memory.
    pub fn invoke_i32_with_args(&self, name: &str, args: &[i32]) -> Result<i32, InvokeError> {
        // Preflight checked the same signature that chooses the result.
        match self.invoke(name, args.iter().copied().map(Value::I32), ResultKind::I32)? {
            Some(Value::I32(value)) => Ok(value),
            _ => unreachable!("checked i32 result kind"),
        }
    }

    /// Invoke the exact named export as a `() -> ()` function.
    /// See [`Self::invoke_void_with_args`] for signature checks and runtime errors.
    ///
    /// ```
    /// use swayze_wasm::validate_module;
    /// let bytes = b"\0asm\x01\0\0\0\x01\x04\x01\x60\0\0\x03\x02\x01\0\
    ///               \x07\x08\x01\x04idle\0\0\x0a\x04\x01\x02\0\x0b";
    /// let module = validate_module(bytes)?;
    /// module.instantiate()?.invoke_void("idle")?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn invoke_void(&self, name: &str) -> Result<(), InvokeError> {
        self.invoke_void_with_args(name, &[])
    }

    /// Invoke the exact named void export, passing arguments in parameter order.
    ///
    /// Checks the name, argument count, declared result kind, and that all
    /// parameters are i32, in that order, before guest execution. Returns
    /// [`InvokeError::ResultMismatch`] for a value result rather than discarding
    /// it. Other errors and effects are the same as for
    /// [`Self::invoke_i32_with_args`], including persistence of stores and
    /// growth completed before a guest trap or host limit.
    pub fn invoke_void_with_args(&self, name: &str, args: &[i32]) -> Result<(), InvokeError> {
        self.invoke(name, args.iter().copied().map(Value::I32), ResultKind::Void)
            .map(|_| ())
    }

    /// Invoke an exact named export with up to four tagged arguments in parameter order.
    ///
    /// Checks the name, argument count, declared result kind, and argument types,
    /// in that order. The first argument of the wrong type is reported with its
    /// zero-based index. No guest instructions run and no guest state changes
    /// unless all checks pass. No argument or result is coerced or truncated.
    /// Float arguments and results are passed as exact IEEE 754 bits in
    /// [`Value::F32`] and [`Value::F64`], preserving signed zeros and NaN encodings
    /// at the host boundary without using native floating-point values.
    /// Returns `None` for [`ResultKind::Void`] or the exact tagged result otherwise.
    /// Execution has the same effects, traps, and limits as
    /// [`Self::invoke_i32_with_args`].
    ///
    /// ```
    /// use swayze_wasm::{ResultKind, Value, validate_module};
    /// // (func (export "echo") (param f32) (result f32) local.get 0)
    /// let bytes = b"\0asm\x01\0\0\0\x01\x06\x01\x60\x01\x7d\x01\x7d\x03\x02\x01\0\
    ///               \x07\x08\x01\x04echo\0\0\x0a\x06\x01\x04\0\x20\0\x0b";
    /// let module = validate_module(bytes)?;
    /// let instance = module.instantiate()?;
    /// let args = [Value::F32(0x7f80_0b12)]; // A signaling NaN with a nonzero payload.
    /// assert_eq!(instance.invoke_with_values("echo", &args, ResultKind::F32)?, Some(args[0]));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn invoke_with_values(
        &self,
        name: &str,
        args: &[Value],
        expected: ResultKind,
    ) -> Result<Option<Value>, InvokeError> {
        self.invoke(name, args.iter().copied(), expected)
    }

    fn invoke(
        &self,
        name: &str,
        args: impl ExactSizeIterator<Item = Value> + Clone,
        expected: ResultKind,
    ) -> Result<Option<Value>, InvokeError> {
        let export = self
            .module
            .exports
            .iter()
            .find(|export| export.name == name)
            .ok_or(InvokeError::ExportNotFound)?;
        binary::invoke_index(self, export.function_index, args, expected)
    }
}

/// A result kind supported by a typed invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResultKind {
    /// The function returns no value.
    Void,
    /// The function returns one `i32` value.
    I32,
    /// The function returns one `i64` value.
    I64,
    /// The function returns one `f32` value as raw bits in [`Value::F32`].
    F32,
    /// The function returns one `f64` value as raw bits in [`Value::F64`].
    F64,
}

impl fmt::Display for ResultKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Void => "void",
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::F32 => "f32",
            Self::F64 => "f64",
        })
    }
}

/// Why invoking a typed export failed: host-side lookup, signature check or
/// resource limit, or a guest trap.
/// Module validation failures are reported separately as [`Error`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvokeError {
    /// This module has no export with the requested name.
    ExportNotFound,
    /// The export exists but the supplied argument count does not match its type.
    ArityMismatch { expected: usize, actual: usize },
    /// The invocation method expects a different result kind than the export declares.
    ResultMismatch {
        /// Result kind required by the invocation method.
        expected: ResultKind,
        /// Result kind declared by the exported function.
        actual: ResultKind,
    },
    /// An argument's type differs from the corresponding parameter's type.
    ArgumentTypeMismatch {
        /// Zero-based position in the parameter list.
        index: usize,
        /// Type declared by the exported function at this position.
        expected: ValType,
        /// Type of the supplied argument at this position.
        actual: ValType,
    },
    /// Guest execution stopped at a trapping instruction.
    Trap(Trap),
    /// The invocation would exceed a host resource bound, not a Core trap.
    LimitExceeded { limit: ExecutionLimit },
}

impl fmt::Display for InvokeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExportNotFound => f.write_str("WebAssembly export not found"),
            Self::ArityMismatch { expected, actual } => {
                write!(
                    f,
                    "WebAssembly export expects {expected} arguments, got {actual}"
                )
            }
            Self::ResultMismatch { expected, actual } => {
                write!(
                    f,
                    "WebAssembly invocation expects result kind {expected}, got {actual}"
                )
            }
            Self::ArgumentTypeMismatch {
                index,
                expected,
                actual,
            } => write!(
                f,
                "WebAssembly export expects {expected} for argument {index}, got {actual}"
            ),
            Self::Trap(trap) => write!(f, "WebAssembly trapped: {trap}"),
            Self::LimitExceeded { limit } => write!(
                f,
                "WebAssembly execution {limit:?} limit exceeded (maximum {})",
                limit.maximum()
            ),
        }
    }
}

impl std::error::Error for InvokeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Trap(trap) => Some(trap),
            Self::ExportNotFound
            | Self::ArityMismatch { .. }
            | Self::ResultMismatch { .. }
            | Self::ArgumentTypeMismatch { .. }
            | Self::LimitExceeded { .. } => None,
        }
    }
}

/// Fixed host resource bounds for one execution (start or explicit invocation).
/// These are not Wasm traps.
///
/// Dispatch is budgeted before executing an instruction. If no dispatches
/// remain, the dispatch limit takes precedence over executing the next
/// instruction. A `call_indirect` with a dispatch available checks table
/// bounds, null, and signature before checking the frame limit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionLimit {
    /// Simultaneously live function frames, including the entry function.
    CallDepth,
    /// Total executed decoded instructions across all frames in the execution.
    InstructionDispatches,
}

impl ExecutionLimit {
    /// The inclusive maximum for this resource.
    pub const fn maximum(self) -> usize {
        match self {
            Self::CallDepth => 16,
            Self::InstructionDispatches => 1024,
        }
    }
}

/// The reason a validated guest instruction trapped at runtime.
///
/// This enum now includes [`Trap::InvalidConversionToInteger`]; downstream
/// exhaustive matches must handle the added variant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Trap {
    /// An integer division or remainder instruction had a zero divisor.
    IntegerDivideByZero,
    /// Signed integer division could not represent its quotient, or a trapping
    /// float-to-integer conversion received an infinity or truncated out of range.
    IntegerOverflow,
    /// The full range of a load or store was not within the instance's memory.
    MemoryOutOfBounds,
    /// A `call_indirect` selector was outside the table's logical length.
    TableOutOfBounds,
    /// A `call_indirect` selected a null table entry.
    UninitializedElement,
    /// A `call_indirect` selected a function with a different structural type.
    IndirectCallTypeMismatch,
    /// An `unreachable` instruction was executed.
    Unreachable,
    /// A trapping float-to-integer conversion received a NaN.
    InvalidConversionToInteger,
}

impl fmt::Display for Trap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable => f.write_str("unreachable instruction executed"),
            Self::IntegerDivideByZero => f.write_str("integer divide by zero"),
            Self::IntegerOverflow => f.write_str("integer overflow"),
            Self::InvalidConversionToInteger => f.write_str("invalid conversion to integer"),
            Self::MemoryOutOfBounds => f.write_str("out of bounds memory access"),
            Self::TableOutOfBounds => f.write_str("out of bounds table access"),
            Self::UninitializedElement => f.write_str("uninitialized table element"),
            Self::IndirectCallTypeMismatch => f.write_str("indirect call type mismatch"),
        }
    }
}

impl std::error::Error for Trap {}

/// The WebAssembly frontend is not implemented; this says nothing about input validity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Unsupported;

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WebAssembly frontend is not implemented")
    }
}

impl std::error::Error for Unsupported {}

/// Always returns [`Unsupported`] without reading or retaining the bytes.
pub fn prepare(_bytes: &[u8]) -> Result<Infallible, Unsupported> {
    Err(Unsupported)
}
