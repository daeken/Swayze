use std::fmt;

// Fixed ceiling on source bytes (including separators) in decimals with a point or exponent.
const MAX_DECIMAL_LITERAL_BYTES: usize = 4096;

// Independent of the caller's expression nesting limit.
const MAX_STATEMENT_FRAMES: usize = 128;

/// A half-open UTF-8 byte range in the original source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    fn at(offset: usize) -> Self {
        Self {
            start: offset,
            end: offset,
        }
    }
}

/// Host-configurable ceilings for one parse. A ceiling is inclusive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    /// Maximum UTF-8 source length, checked before scanning.
    pub max_source_bytes: usize,
    /// Maximum number of tokens, including punctuation but excluding trivia.
    pub max_tokens: usize,
    /// Maximum combined depth of open parentheses (including control headers)
    /// and pending conditional expressions or ungrouped assignment RHSs.
    /// Statement frames have a separate fixed cap.
    pub max_nesting_depth: usize,
    /// Maximum work units (see the crate's slice notes for accounting).
    pub max_work: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_source_bytes: 64 * 1024,
            max_tokens: 4096,
            max_nesting_depth: 128,
            max_work: 128 * 1024,
        }
    }
}

/// The resource that prevented parsing from continuing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Limit {
    SourceBytes,
    Tokens,
    /// The caller's expression nesting limit or the fixed statement frame cap.
    NestingDepth,
    Work,
    /// Fixed source-byte ceiling for a decimal literal with a point or exponent.
    DecimalLiteralBytes,
    Allocation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    /// Recognized syntax is incomplete or violates a static rule of the subset.
    Malformed,
    /// Input requires syntax or semantics outside the slice. This is not a validity claim.
    Unsupported,
    /// A resource ceiling or available allocation was exhausted.
    LimitExceeded(Limit),
}

/// A parse diagnostic that does not retain or print the source text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Error {
    kind: ErrorKind,
    span: Span,
    message: &'static str,
}

impl Error {
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn span(&self) -> Span {
        self.span
    }

    pub fn message(&self) -> &'static str {
        self.message
    }

    fn malformed(span: Span, message: &'static str) -> Self {
        Self {
            kind: ErrorKind::Malformed,
            span,
            message,
        }
    }

    fn unsupported(span: Span, message: &'static str) -> Self {
        Self {
            kind: ErrorKind::Unsupported,
            span,
            message,
        }
    }

    fn limit(span: Span, limit: Limit) -> Self {
        let message = match limit {
            Limit::SourceBytes => "source byte ceiling exceeded",
            Limit::Tokens => "token ceiling exceeded",
            Limit::NestingDepth => "expression nesting ceiling exceeded",
            Limit::Work => "parse work ceiling exceeded",
            Limit::DecimalLiteralBytes => "decimal literal byte ceiling exceeded",
            Limit::Allocation => "unable to allocate parser storage",
        };
        Self {
            kind: ErrorKind::LimitExceeded(limit),
            span,
            message,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            ErrorKind::Malformed => "malformed JavaScript syntax",
            ErrorKind::Unsupported => "unsupported JavaScript input",
            ErrorKind::LimitExceeded(_) => "JavaScript parse limit",
        };
        write!(
            f,
            "{kind} at UTF-8 bytes {}..{}: {}",
            self.span.start, self.span.end, self.message
        )
    }
}

impl std::error::Error for Error {}

/// A validated Script in the supported primitive-value subset. Only the parser constructs it.
pub struct Script {
    instructions: Vec<Instruction>,
    max_stack_depth: usize,
    binding_slots: usize,
}

impl Script {
    /// The number of prepared instructions, including jumps and skipped paths.
    /// A loop can dispatch the same instruction more than once during evaluation.
    pub fn instruction_count(&self) -> usize {
        self.instructions().len()
    }

    /// The peak operand stack size required across all control-flow paths.
    pub fn max_stack_depth(&self) -> usize {
        self.max_stack_depth
    }

    pub(crate) fn instructions(&self) -> &[Instruction] {
        &self.instructions
    }

    pub(crate) fn binding_slots(&self) -> usize {
        self.binding_slots
    }
}

impl fmt::Debug for Script {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Script")
            .field("instruction_count", &self.instruction_count())
            .field("max_stack_depth", &self.max_stack_depth())
            .field("binding_slots", &self.binding_slots)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BindingKind {
    Const,
    Let,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShortCircuitCondition {
    Falsy,
    Truthy,
    NonNullish,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Instruction {
    Number(f64),
    Boolean(bool),
    Null,
    Undefined,
    LoadBinding(usize),
    InitBinding {
        slot: usize,
        kind: BindingKind,
    },
    StoreBinding(usize),
    StoreBindingKeep(usize),
    UpdateBinding {
        slot: usize,
        op: Update,
        result: UpdateResult,
    },
    Discard,
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    Exponentiate,
    Positive,
    Negate,
    Not,
    BitwiseNot,
    Void,
    BitwiseAnd,
    BitwiseXor,
    BitwiseOr,
    ShiftLeft,
    ShiftRight,
    ShiftRightUnsigned,
    LooseEqual,
    LooseNotEqual,
    StrictEqual,
    StrictNotEqual,
    LessThan,
    LessThanOrEqual,
    GreaterThan,
    GreaterThanOrEqual,
    JumpIfFalse(usize),
    // A statement test must be the only operand when the jump consumes it.
    JumpIfFalseStatement(usize),
    // Keep the test value when jumping; otherwise pop it before evaluating the RHS.
    ShortCircuit {
        target: usize,
        condition: ShortCircuitCondition,
    },
    // A standalone logical assignment consumes its only operand on both paths.
    ShortCircuitStatement {
        target: usize,
        condition: ShortCircuitCondition,
    },
    Jump(usize),
    // Unconditional statement transfers require an empty operand stack.
    JumpStatement(usize),
    // The only backward edge; enter a loop test, update or body with an empty stack.
    LoopBack(usize),
}

/// Parse a complete Script in the supported subset with default [`Limits`].
///
/// A root expression at EOF yields the result, with or without a trailing
/// semicolon. If another token follows its semicolon, it is a statement and
/// its value is discarded. A final root expression is still required.
///
/// A control test or a noninitial operand of a comma sequence may begin
/// with a simple name assignment chain.
pub fn parse_script(source: &str) -> Result<Script, Error> {
    parse_script_with_limits(source, Limits::default())
}

/// Parse a complete Script in the supported subset with caller-supplied limits.
pub fn parse_script_with_limits(source: &str, limits: Limits) -> Result<Script, Error> {
    if source.len() > limits.max_source_bytes {
        // Starting within the source, back up at most three continuation bytes.
        let mut start = limits.max_source_bytes;
        while !source.is_char_boundary(start) {
            start -= 1;
        }
        let end = source[start..]
            .chars()
            .next()
            .map_or(start, |c| start + c.len_utf8());
        return Err(Error::limit(Span { start, end }, Limit::SourceBytes));
    }
    Parser {
        lexer: Lexer {
            source,
            offset: 0,
            tokens_left: limits.max_tokens,
            work_left: limits.max_work,
        },
        lookahead: None,
        max_nesting_depth: limits.max_nesting_depth,
        nesting_depth: 0,
        pending: Vec::new(),
        statement_frames: Vec::new(),
        innermost_loop: None,
        innermost_target: None,
        labels: Vec::new(),
        for_header_scope: None,
        instructions: Vec::new(),
        scopes: Vec::new(),
        bindings: Vec::new(),
        accesses: Vec::new(),
        stack_depth: 0,
        max_stack_depth: 0,
    }
    .parse()
}

#[derive(Clone, Copy, Debug)]
enum Operator {
    Coalesce,
    LogicalOr,
    LogicalAnd,
    BitwiseOr,
    BitwiseXor,
    BitwiseAnd,
    LooseEqual,
    LooseNotEqual,
    StrictEqual,
    StrictNotEqual,
    LessThan,
    LessThanOrEqual,
    GreaterThan,
    GreaterThanOrEqual,
    ShiftLeft,
    ShiftRight,
    ShiftRightUnsigned,
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    Exponentiate,
    Positive,
    Negate,
    Not,
    BitwiseNot,
    Void,
}

impl Operator {
    fn precedence(self) -> u8 {
        match self {
            // Mixing `??` with either logical operator at this level is forbidden.
            Self::Coalesce | Self::LogicalOr => 1,
            Self::LogicalAnd => 2,
            Self::BitwiseOr => 3,
            Self::BitwiseXor => 4,
            Self::BitwiseAnd => 5,
            Self::LooseEqual | Self::LooseNotEqual | Self::StrictEqual | Self::StrictNotEqual => 6,
            Self::LessThan
            | Self::LessThanOrEqual
            | Self::GreaterThan
            | Self::GreaterThanOrEqual => 7,
            Self::ShiftLeft | Self::ShiftRight | Self::ShiftRightUnsigned => 8,
            Self::Add | Self::Subtract => 9,
            Self::Multiply | Self::Divide | Self::Remainder => 10,
            Self::Exponentiate => 11,
            Self::Positive | Self::Negate | Self::Not | Self::BitwiseNot | Self::Void => 12,
        }
    }

    fn instruction(self) -> Instruction {
        match self {
            Self::Coalesce | Self::LogicalOr | Self::LogicalAnd => Instruction::ShortCircuit {
                target: UNPATCHED,
                condition: match self {
                    Self::LogicalAnd => ShortCircuitCondition::Falsy,
                    Self::LogicalOr => ShortCircuitCondition::Truthy,
                    _ => ShortCircuitCondition::NonNullish,
                },
            },
            Self::BitwiseOr => Instruction::BitwiseOr,
            Self::BitwiseXor => Instruction::BitwiseXor,
            Self::BitwiseAnd => Instruction::BitwiseAnd,
            Self::ShiftLeft => Instruction::ShiftLeft,
            Self::ShiftRight => Instruction::ShiftRight,
            Self::ShiftRightUnsigned => Instruction::ShiftRightUnsigned,
            Self::LooseEqual => Instruction::LooseEqual,
            Self::LooseNotEqual => Instruction::LooseNotEqual,
            Self::StrictEqual => Instruction::StrictEqual,
            Self::StrictNotEqual => Instruction::StrictNotEqual,
            Self::LessThan => Instruction::LessThan,
            Self::LessThanOrEqual => Instruction::LessThanOrEqual,
            Self::GreaterThan => Instruction::GreaterThan,
            Self::GreaterThanOrEqual => Instruction::GreaterThanOrEqual,
            Self::Add => Instruction::Add,
            Self::Subtract => Instruction::Subtract,
            Self::Multiply => Instruction::Multiply,
            Self::Divide => Instruction::Divide,
            Self::Remainder => Instruction::Remainder,
            Self::Exponentiate => Instruction::Exponentiate,
            Self::Positive => Instruction::Positive,
            Self::Negate => Instruction::Negate,
            Self::Not => Instruction::Not,
            Self::BitwiseNot => Instruction::BitwiseNot,
            Self::Void => Instruction::Void,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Update {
    Increment,
    Decrement,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum UpdateResult {
    New,
    Old,
}

const UPDATE_TARGET_REQUIRED: &str = "increment and decrement require a single identifier target";

fn check_postfix_line_break(operator: Span, after_line_break: bool) -> Result<(), Error> {
    // The lexer records LineTerminator trivia, including inside comments.
    if after_line_break {
        return Err(Error::unsupported(
            operator,
            "a line terminator cannot precede a postfix update",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum TokenKind {
    Number(f64),
    Boolean(bool),
    Null,
    Identifier,
    Declaration(BindingKind),
    If,
    Else,
    While,
    For,
    Do,
    Break,
    Continue,
    Assignment(Option<Operator>),
    Update(Update),
    Operator(Operator),
    Open,
    Close,
    OpenBrace,
    CloseBrace,
    Question,
    Colon,
    Comma,
    Semicolon,
}

struct Token {
    kind: TokenKind,
    span: Span,
    after_line_break: bool,
}

struct Lexer<'s> {
    source: &'s str,
    offset: usize,
    tokens_left: usize,
    work_left: usize,
}

#[derive(Default)]
struct RadixInteger {
    bit_len: u32,
    top: u64,
    sticky: bool,
}

impl RadixInteger {
    const PRECISION: u32 = f64::MANTISSA_DIGITS;
    const MAX_BITS: u32 = f64::MAX_EXP as u32;

    fn push(&mut self, digit: u32, bits_per_digit: u32) {
        if self.bit_len > Self::MAX_BITS || (self.bit_len == 0 && digit == 0) {
            return;
        }
        // Each radix digit appends 1, 3 or 4 bits. Trim leading zeros only
        // from the first nonzero digit. Keep the top 53 bits and one guard
        // bit; all remaining bits contribute only to the sticky flag.
        let width = if self.bit_len == 0 {
            u32::BITS - digit.leading_zeros()
        } else {
            bits_per_digit
        };
        let keep = width.min((Self::PRECISION + 1).saturating_sub(self.bit_len));
        let discard = width - keep;
        self.top = (self.top << keep) | u64::from(digit >> discard);
        self.sticky |= digit & ((1 << discard) - 1) != 0;
        // Anything with more than 1024 significant bits is already >= 2^1024.
        // Saturate at that sentinel, independent of source length.
        self.bit_len = self.bit_len.saturating_add(width).min(Self::MAX_BITS + 1);
    }

    fn number(self) -> f64 {
        if self.bit_len <= Self::PRECISION {
            return self.top as f64; // Exact, including positive zero.
        }
        if self.bit_len > Self::MAX_BITS {
            return f64::INFINITY;
        }

        // The guard bit indicates at least half an ULP. Sticky distinguishes
        // larger remainders from exact ties, which round to an even low bit.
        let significand = self.top >> 1;
        let round_up = self.top & 1 != 0 && (self.sticky || significand & 1 != 0);
        let fraction_bits = Self::PRECISION - 1;
        let exponent_bias = Self::MAX_BITS - 1;
        let biased_exponent = u64::from(self.bit_len - 1 + exponent_bias);
        let fraction = significand + u64::from(round_up) - (1 << fraction_bits);
        // Addition (not OR) carries a rounded significand of 2^53 into
        // the exponent. At 2^1024 - 2^970 (and up), the carry produces
        // exponent 2047 and a zero fraction: positive infinity.
        f64::from_bits((biased_exponent << fraction_bits) + fraction)
    }
}

fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn is_trivia_space(c: char) -> bool {
    // ECMA-262 WhiteSpace and LineTerminator (not Rust's `char::is_whitespace`).
    is_line_terminator(c)
        || matches!(
            c,
            '\t' | '\u{000B}' | '\u{000C}' | ' ' | '\u{00A0}' | '\u{1680}' | '\u{2000}'
                ..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
        )
}

fn is_identifier_start(c: char) -> bool {
    c.is_ascii_alphabetic() || matches!(c, '_' | '$')
}

fn is_excluded_name(name: &str) -> bool {
    matches!(
        name,
        // Other keywords, reserved literals, future/strict reserved words, and `await`.
        "case" | "catch" | "class" | "debugger" | "default"
            | "delete" | "enum" | "export" | "extends"
            | "finally" | "function" | "import" | "in" | "instanceof"
            | "new" | "return" | "super" | "switch" | "this" | "throw"
            | "try" | "typeof" | "var" | "with" | "await" | "yield"
            | "implements" | "interface" | "package" | "private" | "protected"
            | "public" | "static"
            // Contextual syntax, strict-mode restricted names, and selected globals.
            | "as" | "async" | "from" | "get" | "of" | "set" | "using" | "eval"
            | "arguments" | "Infinity" | "NaN" | "globalThis"
    )
}

impl Lexer<'_> {
    fn rest(&self) -> &str {
        // `offset` starts at zero and only advances by complete UTF-8 scalars.
        &self.source[self.offset..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn charge(&mut self, units: usize, span: Span) -> Result<(), Error> {
        self.work_left = self
            .work_left
            .checked_sub(units)
            .ok_or_else(|| Error::limit(span, Limit::Work))?;
        Ok(())
    }

    fn consume(&mut self, c: char) -> Result<Span, Error> {
        // `c` is the first scalar in `rest`, so the sum is at most `source.len()`.
        let span = Span {
            start: self.offset,
            end: self.offset + c.len_utf8(),
        };
        self.charge(c.len_utf8(), span)?;
        self.offset = span.end;
        Ok(span)
    }

    fn skip_trivia(&mut self) -> Result<bool, Error> {
        let mut line_break = false;
        while let Some(c) = self.peek() {
            if is_trivia_space(c) {
                line_break |= is_line_terminator(c);
                self.consume(c)?;
            } else if c == '/' && self.rest().starts_with("//") {
                self.consume('/')?;
                self.consume('/')?;
                // Leave the line terminator for the next trivia iteration.
                while let Some(c) = self.peek().filter(|&c| !is_line_terminator(c)) {
                    self.consume(c)?;
                }
            } else if c == '/' && self.rest().starts_with("/*") {
                line_break |= self.skip_block_comment()?;
            } else {
                break;
            }
        }
        Ok(line_break)
    }

    fn skip_block_comment(&mut self) -> Result<bool, Error> {
        let start = self.offset;
        self.consume('/')?;
        self.consume('*')?;
        let mut line_break = false;
        while let Some(c) = self.peek() {
            self.consume(c)?;
            if c == '*' && self.peek() == Some('/') {
                self.consume('/')?;
                return Ok(line_break);
            }
            line_break |= is_line_terminator(c);
        }
        Err(Error::malformed(
            Span {
                start,
                end: self.offset,
            },
            "unterminated block comment",
        ))
    }

    fn next(&mut self) -> Result<Option<Token>, Error> {
        let after_line_break = self.skip_trivia()?;
        let Some(c) = self.peek() else {
            return Ok(None);
        };
        let start = self.offset;
        let first = Span {
            start,
            end: start + c.len_utf8(),
        };
        self.tokens_left = self
            .tokens_left
            .checked_sub(1)
            .ok_or_else(|| Error::limit(first, Limit::Tokens))?;
        self.consume(c)?;

        let kind = match c {
            '0'..='9' => self.number(c, start)?,
            '.' if matches!(self.peek(), Some('0'..='9')) => self.number(c, start)?,
            _ if is_identifier_start(c) => self.identifier(start)?,
            '=' | '!' => self.equals_or_bang(c, start)?,
            '&' | '|' | '^' => self.bitwise_or_logical(c)?,
            '~' => TokenKind::Operator(Operator::BitwiseNot),
            // Annex B HTML-like comments remain outside the subset.
            '<' if self.rest().starts_with("!--") => {
                return Err(Error::unsupported(
                    first,
                    "HTML comments are outside the subset",
                ));
            }
            '<' | '>' => self.relational_or_shift(c)?,
            '(' => TokenKind::Open,
            ')' => TokenKind::Close,
            '{' => TokenKind::OpenBrace,
            '}' => TokenKind::CloseBrace,
            '?' => self.question(start)?,
            ':' => TokenKind::Colon,
            ',' => TokenKind::Comma,
            ';' => TokenKind::Semicolon,
            '+' | '-' | '*' | '/' | '%' => self.operator(c)?,
            _ => {
                return Err(Error::unsupported(
                    first,
                    "token is outside the arithmetic subset",
                ));
            }
        };
        Ok(Some(Token {
            kind,
            span: Span {
                start,
                end: self.offset,
            },
            after_line_break,
        }))
    }

    fn identifier(&mut self, start: usize) -> Result<TokenKind, Error> {
        while let Some(c) = self
            .peek()
            .filter(|&c| is_identifier_start(c) || c.is_ascii_digit())
        {
            self.consume(c)?;
        }
        // Don't mistake the ASCII prefix of a Unicode or escaped name for a keyword.
        if let Some(c) = self
            .peek()
            .filter(|&c| c == '\\' || (!c.is_ascii() && !is_trivia_space(c)))
        {
            let span = self.consume(c)?;
            return Err(Error::unsupported(
                span,
                "Unicode and escaped identifiers are outside the subset",
            ));
        }
        let name = &self.source[start..self.offset];
        match name {
            "const" => Ok(TokenKind::Declaration(BindingKind::Const)),
            "let" => Ok(TokenKind::Declaration(BindingKind::Let)),
            "if" => Ok(TokenKind::If),
            "else" => Ok(TokenKind::Else),
            "while" => Ok(TokenKind::While),
            "for" => Ok(TokenKind::For),
            "do" => Ok(TokenKind::Do),
            "break" => Ok(TokenKind::Break),
            "continue" => Ok(TokenKind::Continue),
            "true" => Ok(TokenKind::Boolean(true)),
            "false" => Ok(TokenKind::Boolean(false)),
            "null" => Ok(TokenKind::Null),
            "void" => Ok(TokenKind::Operator(Operator::Void)),
            _ if is_excluded_name(name) => Err(Error::unsupported(
                Span {
                    start,
                    end: self.offset,
                },
                "reserved or excluded name is outside the subset",
            )),
            _ => Ok(TokenKind::Identifier),
        }
    }

    fn equals_or_bang(&mut self, first: char, start: usize) -> Result<TokenKind, Error> {
        if self.peek() == Some('=') {
            self.consume('=')?;
            let strict = self.peek() == Some('=');
            if strict {
                self.consume('=')?;
            }
            return Ok(TokenKind::Operator(match (first, strict) {
                ('=', false) => Operator::LooseEqual,
                ('!', false) => Operator::LooseNotEqual,
                ('=', true) => Operator::StrictEqual,
                ('!', true) => Operator::StrictNotEqual,
                _ => unreachable!("not an equality operator"),
            }));
        }
        if first == '=' {
            if self.peek() != Some('>') {
                return Ok(TokenKind::Assignment(None));
            }
            self.consume('>')?;
        } else {
            return Ok(TokenKind::Operator(Operator::Not));
        }
        Err(Error::unsupported(
            Span {
                start,
                end: self.offset,
            },
            "punctuator is outside the subset",
        ))
    }

    fn bitwise_or_logical(&mut self, first: char) -> Result<TokenKind, Error> {
        let doubled = matches!(first, '&' | '|') && self.peek() == Some(first);
        if doubled {
            self.consume(first)?;
        }
        let op = match (first, doubled) {
            ('&', false) => Operator::BitwiseAnd,
            ('^', false) => Operator::BitwiseXor,
            ('|', false) => Operator::BitwiseOr,
            ('&', true) => Operator::LogicalAnd,
            ('|', true) => Operator::LogicalOr,
            _ => unreachable!("not a bitwise or logical operator"),
        };
        self.operator_or_assignment(op)
    }

    fn relational_or_shift(&mut self, first: char) -> Result<TokenKind, Error> {
        if self.peek() == Some(first) {
            self.consume(first)?;
            let op = if first == '<' {
                Operator::ShiftLeft
            } else if self.peek() == Some('>') {
                self.consume('>')?;
                Operator::ShiftRightUnsigned
            } else {
                Operator::ShiftRight
            };
            return self.operator_or_assignment(op);
        }
        let inclusive = self.peek() == Some('=');
        if inclusive {
            self.consume('=')?;
        }
        Ok(TokenKind::Operator(match (first, inclusive) {
            ('<', false) => Operator::LessThan,
            ('<', true) => Operator::LessThanOrEqual,
            ('>', false) => Operator::GreaterThan,
            ('>', true) => Operator::GreaterThanOrEqual,
            _ => unreachable!("not a relational operator"),
        }))
    }

    fn question(&mut self, start: usize) -> Result<TokenKind, Error> {
        if self.peek() == Some('?') {
            self.consume('?')?;
            return self.operator_or_assignment(Operator::Coalesce);
        }
        if self.rest().strip_prefix('.').is_some_and(|rest| {
            // `?.` is not optional chaining when followed by a decimal digit.
            !matches!(rest.as_bytes().first(), Some(b'0'..=b'9'))
        }) {
            self.consume('.')?;
        } else {
            return Ok(TokenKind::Question);
        }
        Err(Error::unsupported(
            Span {
                start,
                end: self.offset,
            },
            "longer punctuator is outside the arithmetic subset",
        ))
    }

    fn operator(&mut self, c: char) -> Result<TokenKind, Error> {
        if matches!(c, '+' | '-' | '*') && self.peek() == Some(c) {
            self.consume(c)?;
            return match c {
                '+' => Ok(TokenKind::Update(Update::Increment)),
                '-' => Ok(TokenKind::Update(Update::Decrement)),
                _ => self.operator_or_assignment(Operator::Exponentiate),
            };
        }
        self.operator_or_assignment(match c {
            '+' => Operator::Add,
            '-' => Operator::Subtract,
            '*' => Operator::Multiply,
            '/' => Operator::Divide,
            '%' => Operator::Remainder,
            _ => unreachable!("not an arithmetic operator"),
        })
    }

    fn operator_or_assignment(&mut self, op: Operator) -> Result<TokenKind, Error> {
        if self.peek() == Some('=') {
            self.consume('=')?;
            Ok(TokenKind::Assignment(Some(op)))
        } else {
            Ok(TokenKind::Operator(op))
        }
    }

    // Scan one digit sequence. `has_digit` is initially true only when the
    // caller has already consumed its first digit.
    fn number_digits(
        &mut self,
        radix: u32,
        mut has_digit: bool,
        mut push_digit: impl FnMut(u32),
    ) -> Result<usize, Error> {
        let mut separators = 0;
        while let Some(c) = self.peek() {
            if let Some(digit) = c.to_digit(radix) {
                self.consume(c)?;
                push_digit(digit);
                has_digit = true;
            } else if c == '_' {
                let span = self.consume(c)?;
                if !has_digit || !self.peek().is_some_and(|next| next.is_digit(radix)) {
                    return Err(Error::malformed(
                        span,
                        "numeric separator must be between digits of the same sequence",
                    ));
                }
                separators += 1;
            } else {
                break;
            }
        }
        Ok(separators)
    }

    fn reject_numeric_suffix(&mut self) -> Result<(), Error> {
        // Decline adjacent non-ASCII non-whitespace without classifying Unicode identifiers.
        if let Some(c) = self.peek()
            && (c.is_ascii_alphabetic()
                || matches!(c, '.' | '$' | '\\')
                || (!c.is_ascii() && !is_trivia_space(c)))
        {
            let span = self.consume(c)?;
            return Err(Error::unsupported(
                span,
                "adjacent characters require non-subset numeric or identifier lexing",
            ));
        }
        Ok(())
    }

    fn radix_integer(&mut self, bits_per_digit: u32) -> Result<TokenKind, Error> {
        let digits_start = self.offset;
        let mut value = RadixInteger::default();
        self.number_digits(1 << bits_per_digit, false, |digit| {
            value.push(digit, bits_per_digit);
        })?;
        // Check the boundary even if the accumulated value already overflows.
        if let Some(c) = self.peek()
            && (c.is_ascii_digit() || (self.offset == digits_start && c.is_ascii_alphabetic()))
        {
            let span = self.consume(c)?;
            return Err(Error::malformed(span, "invalid digit for numeric base"));
        }
        self.reject_numeric_suffix()?;
        if self.offset == digits_start {
            return Err(Error::malformed(
                Span::at(digits_start),
                "expected a digit after numeric base prefix",
            ));
        }
        Ok(TokenKind::Number(value.number()))
    }

    fn number(&mut self, first: char, start: usize) -> Result<TokenKind, Error> {
        let mut separators = 0;
        if first == '0' {
            if let Some(prefix @ ('x' | 'X' | 'o' | 'O' | 'b' | 'B')) = self.peek() {
                let bits_per_digit = match prefix {
                    'x' | 'X' => 4,
                    'o' | 'O' => 3,
                    _ => 1,
                };
                self.consume(prefix)?;
                return self.radix_integer(bits_per_digit);
            }
            if let Some(c @ ('0'..='9' | '_')) = self.peek() {
                let span = self.consume(c)?;
                return Err(Error::unsupported(
                    span,
                    "leading-zero numeric syntax is outside the subset",
                ));
            }
        } else if first != '.' {
            separators += self.number_digits(10, true, |_| {})?;
        }

        let mut integer_only = first != '.';
        if !integer_only || self.peek() == Some('.') {
            if integer_only {
                self.consume('.')?;
            }
            separators += self.number_digits(10, false, |_| {})?;
            integer_only = false;
        }
        if let Some(marker @ ('e' | 'E')) = self.peek() {
            self.consume(marker)?;
            if let Some(sign @ ('+' | '-')) = self.peek() {
                self.consume(sign)?;
            }
            let digits_start = self.offset;
            separators += self.number_digits(10, false, |_| {})?;
            if self.offset == digits_start {
                // An adjacent identifier is still unsupported, not necessarily
                // an attempt to write an exponent in this grammar.
                self.reject_numeric_suffix()?;
                return Err(Error::malformed(
                    Span::at(self.offset),
                    "expected a decimal exponent digit",
                ));
            }
            integer_only = false;
        }
        self.reject_numeric_suffix()?;

        let span = Span {
            start,
            end: self.offset,
        };
        let bytes = span.end - span.start;
        if integer_only && bytes - separators > 309 {
            // A canonical integer with 310+ digits is at least 10^309.
            return Ok(TokenKind::Number(f64::INFINITY));
        }
        if bytes > MAX_DECIMAL_LITERAL_BYTES {
            return Err(Error::limit(span, Limit::DecimalLiteralBytes));
        }
        Ok(TokenKind::Number(self.decimal_value(span, separators)?))
    }

    fn decimal_value(&mut self, span: Span, separators: usize) -> Result<f64, Error> {
        let raw = &self.source[span.start..span.end];
        let conversion_bytes = raw.len() - separators;
        if separators != 0 {
            // Meter the full normalization pass before allocating; each source
            // byte is visited once and at most one ASCII byte is copied per visit.
            self.charge(raw.len(), span)?;
        }
        self.charge(conversion_bytes, span)?;
        let mut normalized = String::new();
        let input = if separators == 0 {
            raw
        } else {
            normalized
                .try_reserve_exact(conversion_bytes)
                .map_err(|_| Error::limit(span, Limit::Allocation))?;
            for byte in raw.bytes().filter(|&byte| byte != b'_') {
                normalized.push(char::from(byte));
            }
            normalized.as_str()
        };
        input
            .parse::<f64>()
            .map_err(|_| Error::malformed(span, "could not convert decimal literal"))
    }
}

#[derive(Clone, Copy)]
enum Pending {
    Op(Operator),
    ShortCircuit {
        op: Operator,
        jump: usize,
        join_depth: usize,
    },
    StatementTest,
    Group,
    AssignmentGroup(ExpressionAssignment),
    Assignment(ExpressionAssignment),
    Consequent {
        branch: usize,
        base_depth: usize,
    },
    Alternate {
        jump: usize,
        join_depth: usize,
    },
}

#[derive(Clone, Copy)]
struct ExpressionAssignment {
    access: usize,
    tail: AssignmentTail,
    rhs_base_depth: usize,
}

#[derive(Clone, Copy)]
enum AssignmentTail {
    Simple,
    Eager(Operator),
    ShortCircuit(usize),
}

// All unresolved jump targets and binding operands are patched before returning a Script.
const UNPATCHED: usize = usize::MAX;

// Root is index 0. Each braced body and `for` declaration header gets its own scope.
struct Scope {
    parent: Option<usize>,
    last_binding: Option<usize>,
}

struct Binding {
    name: Span,
    kind: BindingKind,
    initialized_at: usize,
    previous: Option<usize>,
}

#[derive(Clone, Copy)]
struct Access {
    name: Span,
    scope: usize,
    instruction: usize,
    write: Option<WriteAccess>,
}

#[derive(Clone, Copy)]
struct WriteAccess {
    operator_span: Span,
    // A read-modify-write patches this load to the same slot as the store.
    load: Option<usize>,
}

#[derive(Clone, Copy)]
enum ExpressionEnd {
    Script,
    ExpressionStatement,
    // A bare assignment RHS ends at the first outer comma or semicolon.
    StatementStore,
    // Subsequent comma operands use expression rules but still require ';'.
    StatementStoreSuffix,
    Declarator(DeclaratorEnd),
    StatementTest,
    ForTest,
    ForClause(ForClauseEnd),
    // An outer comma ends the first assignment RHS or name update.
    ForStore(ForClauseEnd),
}

#[derive(Clone, Copy)]
enum ForClauseEnd {
    Init,
    Update,
}

#[derive(Clone, Copy)]
enum DeclaratorEnd {
    Statement(&'static str),
    ForHeader,
}

const FOR_SEMICOLON: &str = "expected ';' between for clauses";
const FOR_UPDATE_CLOSE: &str = "expected ')' after the for update";
const EXPRESSION_STATEMENT_SEMICOLON: &str = "expression statements require explicit semicolons";
const ASSIGNMENT_STATEMENT_SEMICOLON: &str = "assignments require explicit semicolons";

// One frame per active standalone block or control statement, reused for `else {}`.
// Every test starts and every block enters and joins at operand depth zero.
#[derive(Clone, Copy)]
struct StatementFrame {
    kind: StatementKind,
    // All loops are targets; only labeled blocks and the first `if` of a
    // labeled chain are targets. Changing if arms does not change this data.
    target: Option<JumpTarget>,
}

#[derive(Clone, Copy)]
enum StatementKind {
    Block,
    IfThen {
        branch: usize,
    },
    IfElse {
        join: usize,
    },
    // The then-arm scope has closed, but this if is still active until its
    // nested `else if` finishes.
    IfAwaitJoin {
        join: usize,
    },
    Loop {
        layout: LoopLayout,
        enclosing_loop: Option<usize>,
    },
}

#[derive(Clone, Copy)]
struct JumpTarget {
    label_start: usize,
    enclosing_target: Option<usize>,
    // Head of a list threaded through unpatched break instructions.
    break_head: usize,
}

#[derive(Clone, Copy)]
enum LoopLayout {
    PreTest {
        continue_target: usize,
        exit: usize,
    },
    PostTest {
        body_start: usize,
        // The trailing test has not been emitted. Thread pending continues
        // through their forward jumps, separately from the breaks.
        continue_head: usize,
    },
}

struct BlockFrame {
    statement: StatementFrame,
    // The open body's scope, or the enclosing scope for IfAwaitJoin.
    scope: usize,
}

struct Parser<'s> {
    lexer: Lexer<'s>,
    // Cache the next lexer result (including EOF) while disambiguating a
    // name, a root semicolon, or an if's then-arm.
    lookahead: Option<Option<Token>>,
    max_nesting_depth: usize,
    nesting_depth: usize,
    pending: Vec<(Pending, Span)>,
    statement_frames: Vec<BlockFrame>,
    // Indices into statement_frames. Unlabeled jumps need only the loop chain.
    innermost_loop: Option<usize>,
    // Includes all loops, labeled standalone blocks, and labeled if chains.
    innermost_target: Option<usize>,
    // Contiguous spans per target, plus any prefix currently being parsed.
    // Closing a target truncates this Copy-only stack back to its start.
    labels: Vec<Span>,
    // Only active while parsing a `for` declaration header and opening its body.
    // Once the body frame is pushed, its own scope links to this one.
    for_header_scope: Option<usize>,
    instructions: Vec<Instruction>,
    // Parse-only scope, declaration (in slot order), and access tables.
    scopes: Vec<Scope>,
    bindings: Vec<Binding>,
    accesses: Vec<Access>,
    stack_depth: usize,
    max_stack_depth: usize,
}

impl Parser<'_> {
    fn parse(mut self) -> Result<Script, Error> {
        self.add_scope(None, Span::at(0))?;
        let mut saw_statement = false;
        loop {
            if !self.statement_frames.is_empty() {
                self.parse_block_item()?;
                continue;
            }
            let first = self.next_token()?.ok_or_else(|| {
                Error::unsupported(
                    Span::at(self.lexer.offset),
                    if saw_statement {
                        "a final expression is required after statements"
                    } else {
                        "empty scripts are outside the arithmetic subset"
                    },
                )
            })?;
            self.check_statement_boundary(first.span)?;
            // Reaching this loop's EOF check again means a statement was parsed,
            // even if it emitted nothing. The final expression exits the loop.
            saw_statement = true;
            let Some(first) = self.parse_non_expression_statement(first)? else {
                continue;
            };
            let (end, delimiter) = self.parse_expression(first, ExpressionEnd::Script)?;
            if delimiter.is_none() {
                break;
            }
            // A root semicolon terminates the final expression only at EOF.
            // Otherwise discard the completed value and resume with the cached token.
            let next = self.next_token()?;
            if next.is_none() {
                break;
            }
            self.lookahead = Some(next);
            self.emit(Instruction::Discard, end)?;
        }
        self.resolve_accesses()?;
        Ok(Script {
            instructions: self.instructions,
            max_stack_depth: self.max_stack_depth,
            binding_slots: self.bindings.len(),
        })
    }

    fn next_token(&mut self) -> Result<Option<Token>, Error> {
        match self.lookahead.take() {
            Some(next) => Ok(next),
            None => self.lexer.next(),
        }
    }

    fn require_token(&mut self, message: &'static str) -> Result<Token, Error> {
        self.next_token()?
            .ok_or_else(|| Error::malformed(Span::at(self.lexer.offset), message))
    }

    fn check_statement_boundary(&self, span: Span) -> Result<(), Error> {
        if self.stack_depth != 0 || !self.pending.is_empty() || self.nesting_depth != 0 {
            return Err(Error::malformed(
                span,
                "statement boundaries must have no pending expression or operands",
            ));
        }
        Ok(())
    }

    fn current_scope(&self) -> usize {
        self.for_header_scope
            .or_else(|| self.statement_frames.last().map(|frame| frame.scope))
            .unwrap_or(0)
    }

    fn add_scope(&mut self, parent: Option<usize>, span: Span) -> Result<usize, Error> {
        let id = self.scopes.len();
        try_push(
            &mut self.scopes,
            Scope {
                parent,
                last_binding: None,
            },
            span,
        )?;
        Ok(id)
    }

    fn open_statement(&mut self, first: Token, label_start: usize) -> Result<(), Error> {
        if self.statement_frames.len() == MAX_STATEMENT_FRAMES {
            return Err(Error {
                message: "statement nesting ceiling exceeded",
                ..Error::limit(
                    self.labels.get(label_start).copied().unwrap_or(first.span),
                    Limit::NestingDepth,
                )
            });
        }
        self.check_label_prefix(label_start)?;
        let (frame, missing_brace) = match first.kind {
            TokenKind::OpenBrace => {
                let frame = self.optional_target_frame(StatementKind::Block, label_start);
                return self.open_block(frame, first.span);
            }
            TokenKind::If => {
                let branch = self.parse_statement_test("expected '(' after 'if'")?;
                (
                    self.optional_target_frame(StatementKind::IfThen { branch }, label_start),
                    "expected '{' to open an if branch",
                )
            }
            TokenKind::While => {
                let test = self.instructions.len();
                let exit = self.parse_statement_test("expected '(' after 'while'")?;
                (
                    self.loop_frame(
                        LoopLayout::PreTest {
                            continue_target: test,
                            exit,
                        },
                        label_start,
                    ),
                    "expected '{' to open the while body",
                )
            }
            TokenKind::For => return self.open_for(label_start),
            TokenKind::Do => (
                self.loop_frame(
                    LoopLayout::PostTest {
                        body_start: self.instructions.len(),
                        continue_head: UNPATCHED,
                    },
                    label_start,
                ),
                "expected '{' to open the do body",
            ),
            _ => unreachable!("not a block or control statement"),
        };
        let brace = self.require_block_open(missing_brace)?;
        self.open_block(frame, brace)
    }

    fn open_block(&mut self, frame: StatementFrame, brace: Span) -> Result<(), Error> {
        self.lexer.charge(1, brace)?;
        let index = self.statement_frames.len();
        let scope = self.add_scope(Some(self.current_scope()), brace)?;
        try_push(
            &mut self.statement_frames,
            BlockFrame {
                statement: frame,
                scope,
            },
            brace,
        )?;
        if frame.target.is_some() {
            self.innermost_target = Some(index);
            if matches!(frame.kind, StatementKind::Loop { .. }) {
                self.innermost_loop = Some(index);
            }
        }
        Ok(())
    }

    fn close_block(&mut self, close: Span) -> Result<(), Error> {
        let index = self
            .statement_frames
            .len()
            .checked_sub(1)
            .ok_or_else(|| Error::malformed(close, "unmatched closing brace"))?;
        let kind = self.statement_frames[index].statement.kind;
        match kind {
            StatementKind::Block | StatementKind::Loop { .. } => {}
            StatementKind::IfThen { branch } => {
                // Only this freshly closed then-arm can take the next `else`.
                // In particular, a `}` belongs to the parent, so leave it cached.
                let next = self.next_token()?;
                if let Some(Token {
                    kind: TokenKind::Else,
                    span,
                    ..
                }) = next
                {
                    let join = self.instructions.len();
                    self.emit(Instruction::JumpStatement(UNPATCHED), span)?;
                    self.patch_if_jump(branch, span)?;
                    let missing = "expected '{' or 'if' after 'else'";
                    let arm = self.require_token(missing)?;
                    let parent = self.statement_frames[..index]
                        .last()
                        .map_or(0, |frame| frame.scope);
                    if matches!(arm.kind, TokenKind::If) {
                        // The next test sees the enclosing scope, but this if
                        // still occupies a frame when opening the nested if.
                        let frame = &mut self.statement_frames[index];
                        frame.statement.kind = StatementKind::IfAwaitJoin { join };
                        frame.scope = parent;
                        // The outer frame keeps any label prefix; the next
                        // arm opens without a target of its own.
                        return self.open_statement(arm, self.labels.len());
                    }
                    let brace = Self::block_open(arm, missing)?;
                    // Same control frame, disjoint lexical scope.
                    let scope = self.add_scope(Some(parent), brace)?;
                    let frame = &mut self.statement_frames[index];
                    frame.statement.kind = StatementKind::IfElse { join };
                    frame.scope = scope;
                    return Ok(());
                }
                self.lookahead = Some(next);
                self.patch_if_jump(branch, close)?;
                return self.close_if_chain(close);
            }
            StatementKind::IfElse { join } => {
                self.patch_if_jump(join, close)?;
                return self.close_if_chain(close);
            }
            StatementKind::IfAwaitJoin { .. } => {
                unreachable!("a brace always closes an open body");
            }
        }
        // A trailing test resolves names in the surrounding scope, not
        // the just-closed body, even when entered by a continue.
        let break_head = self.pop_frame();
        if let StatementKind::Loop {
            layout,
            enclosing_loop,
        } = kind
        {
            self.innermost_loop = enclosing_loop;
            let (back_target, exit, end) = match layout {
                LoopLayout::PreTest {
                    continue_target,
                    exit,
                } => (continue_target, exit, close),
                LoopLayout::PostTest {
                    body_start,
                    continue_head,
                } => {
                    self.patch_pending_jumps(continue_head, close)?;
                    let (exit, end) = self.parse_do_tail()?;
                    (body_start, exit, end)
                }
            };
            self.emit(Instruction::LoopBack(back_target), end)?;
            self.patch_jump(exit, end)?;
        }
        // A block needs no exit instruction; join at the current frontier.
        self.patch_pending_jumps(break_head, close)
    }

    fn close_if_chain(&mut self, close: Span) -> Result<(), Error> {
        // Only suspended `else if` ancestors close here, not any enclosing
        // braced body. All arms and named breaks share one empty-stack join.
        loop {
            let break_head = self.pop_frame();
            self.patch_pending_jumps(break_head, close)?;
            let Some(BlockFrame { statement, .. }) = self.statement_frames.last() else {
                return Ok(());
            };
            let StatementKind::IfAwaitJoin { join } = statement.kind else {
                return Ok(());
            };
            self.patch_if_jump(join, close)?;
        }
    }

    fn pop_frame(&mut self) -> usize {
        let frame = self
            .statement_frames
            .pop()
            .expect("an active statement frame");
        let Some(target) = frame.statement.target else {
            return UNPATCHED;
        };
        self.innermost_target = target.enclosing_target;
        self.labels.truncate(target.label_start);
        target.break_head
    }

    fn patch_if_jump(&mut self, at: usize, span: Span) -> Result<(), Error> {
        self.lexer.charge(1, span)?;
        self.patch_jump(at, span)
    }

    fn loop_frame(&self, layout: LoopLayout, label_start: usize) -> StatementFrame {
        self.target_frame(
            StatementKind::Loop {
                layout,
                enclosing_loop: self.innermost_loop,
            },
            label_start,
        )
    }

    fn optional_target_frame(&self, kind: StatementKind, label_start: usize) -> StatementFrame {
        if label_start == self.labels.len() {
            StatementFrame { kind, target: None }
        } else {
            self.target_frame(kind, label_start)
        }
    }

    fn target_frame(&self, kind: StatementKind, label_start: usize) -> StatementFrame {
        StatementFrame {
            kind,
            target: Some(JumpTarget {
                label_start,
                enclosing_target: self.innermost_target,
                break_head: UNPATCHED,
            }),
        }
    }

    fn parse_do_tail(&mut self) -> Result<(usize, Span), Error> {
        let missing_while = "expected 'while' after the do body";
        let keyword = self.require_token(missing_while)?;
        if !matches!(keyword.kind, TokenKind::While) {
            return Err(Error::malformed(keyword.span, missing_while));
        }
        let exit = self.parse_statement_test("expected '(' after 'while'")?;
        let next = self.next_token()?;
        match next {
            Some(Token {
                kind: TokenKind::Semicolon,
                span,
                ..
            }) => Ok((exit, span)),
            // JavaScript permits ASI here even without a line terminator.
            _ => Err(Error::unsupported(
                next.map_or(Span::at(self.lexer.offset), |token| token.span),
                "do-while requires an explicit trailing semicolon",
            )),
        }
    }

    fn open_for(&mut self, label_start: usize) -> Result<(), Error> {
        let result = self.parse_for_header(label_start).and_then(|frame| {
            let brace = self.require_block_open("expected '{' to open the for body")?;
            self.open_block(frame, brace)
        });
        self.for_header_scope = None;
        result
    }

    fn parse_for_header(&mut self, label_start: usize) -> Result<StatementFrame, Error> {
        let missing_open = "expected '(' after 'for'";
        let open = self.require_token(missing_open)?;
        if !matches!(open.kind, TokenKind::Open) {
            return Err(Error::malformed(open.span, missing_open));
        }
        // The header parentheses count toward nesting but do not group a single
        // expression. Each clause runs the same iterative expression parser.
        self.enter_nesting(open.span)?;
        self.lexer.charge(1, open.span)?;

        let init = self.require_token("expected a for initializer or ';'")?;
        match init.kind {
            TokenKind::Close => return Err(Error::malformed(init.span, FOR_SEMICOLON)),
            TokenKind::Semicolon => {}
            TokenKind::Declaration(kind) => {
                self.lexer.charge(1, init.span)?;
                let scope = self.add_scope(Some(self.current_scope()), init.span)?;
                self.for_header_scope = Some(scope);
                self.parse_declaration_list(kind, DeclaratorEnd::ForHeader)?;
            }
            _ => {
                self.parse_for_clause(init, ForClauseEnd::Init)?;
            }
        }

        // Loop back edges skip the initializer. Reentering the statement
        // executes its InitBinding instructions again, in the same fixed slots.
        let test = self.instructions.len();
        let first = self.require_token("expected a for test or ';'")?;
        let end = if matches!(first.kind, TokenKind::Semicolon) {
            self.emit(Instruction::Boolean(true), first.span)?;
            first.span
        } else {
            self.parse_expression(first, ExpressionEnd::ForTest)?.0
        };
        let exit = self.emit_statement_test(end)?;

        let first = self.require_token("expected a for update or ')'")?;
        if matches!(first.kind, TokenKind::Semicolon) {
            return Err(Error::malformed(
                first.span,
                "unexpected third ';' in for header",
            ));
        }
        let (continue_target, close) = if matches!(first.kind, TokenKind::Close) {
            (test, first.span)
        } else {
            // A truthy test skips over the update to the body. Fallthrough at
            // the end of the body and continue both jump back to the update,
            // whose own back edge leads to the test. Break skips both.
            let skip_update = self.instructions.len();
            self.emit(Instruction::JumpStatement(UNPATCHED), first.span)?;
            let update = self.instructions.len();
            let close = self.parse_for_clause(first, ForClauseEnd::Update)?;
            self.emit(Instruction::LoopBack(test), close)?;
            self.patch_jump(skip_update, close)?;
            (update, close)
        };
        self.nesting_depth -= 1;
        self.check_statement_boundary(close)?;
        Ok(self.loop_frame(
            LoopLayout::PreTest {
                continue_target,
                exit,
            },
            label_start,
        ))
    }

    fn parse_for_clause(&mut self, first: Token, end: ForClauseEnd) -> Result<Span, Error> {
        // Keep the consuming store path for a leading assignment or update.
        // Either can then continue with a sequence suffix at a comma.
        let store_end = ExpressionEnd::ForStore(end);
        match first.kind {
            TokenKind::Update(op) => return self.parse_prefix_update(op, first.span, store_end),
            TokenKind::Identifier => {
                let next = self.next_token()?;
                if let Some(span) = self.parse_name_store(first.span, next, store_end)? {
                    return Ok(span);
                }
            }
            _ => {}
        }
        self.parse_discarded_expression(first, ExpressionEnd::ForClause(end))
    }

    // Both statements and for clauses have already read one token after the name.
    // If it doesn't commit to a store, leave it cached for the expression parser.
    fn parse_name_store(
        &mut self,
        name: Span,
        next: Option<Token>,
        end: ExpressionEnd,
    ) -> Result<Option<Span>, Error> {
        match next {
            Some(Token {
                kind: TokenKind::Assignment(op),
                span,
                ..
            }) => self.parse_assignment(name, op, span, end).map(Some),
            Some(Token {
                kind: TokenKind::Update(op),
                span,
                after_line_break,
            }) => {
                check_postfix_line_break(span, after_line_break)?;
                self.parse_update(name, op, span, end).map(Some)
            }
            _ => {
                self.lookahead = Some(next);
                Ok(None)
            }
        }
    }

    fn parse_discarded_expression(
        &mut self,
        first: Token,
        end: ExpressionEnd,
    ) -> Result<Span, Error> {
        let (span, _) = self.parse_expression(first, end)?;
        self.emit(Instruction::Discard, span)?;
        Ok(span)
    }

    fn parse_statement_test(&mut self, missing_open: &'static str) -> Result<usize, Error> {
        let open = self.require_token(missing_open)?;
        if !matches!(open.kind, TokenKind::Open) {
            return Err(Error::malformed(open.span, missing_open));
        }
        let (end, _) = self.parse_expression(open, ExpressionEnd::StatementTest)?;
        self.emit_statement_test(end)
    }

    fn emit_statement_test(&mut self, end: Span) -> Result<usize, Error> {
        let branch = self.instructions.len();
        // The test leaves one value, which the jump consumes on both paths.
        self.emit(Instruction::JumpIfFalseStatement(UNPATCHED), end)?;
        Ok(branch)
    }

    fn require_block_open(&mut self, missing_open: &'static str) -> Result<Span, Error> {
        Self::block_open(self.require_token(missing_open)?, missing_open)
    }

    fn block_open(open: Token, missing_open: &'static str) -> Result<Span, Error> {
        match open.kind {
            TokenKind::OpenBrace => Ok(open.span),
            TokenKind::If => Err(Error::unsupported(
                open.span,
                "if branches and loop bodies require braces",
            )),
            _ => Err(Error::malformed(open.span, missing_open)),
        }
    }

    fn parse_block_item(&mut self) -> Result<(), Error> {
        let first = self.require_token("expected a statement or '}' in the block")?;
        self.check_statement_boundary(first.span)?;
        if matches!(first.kind, TokenKind::CloseBrace) {
            return self.close_block(first.span);
        }
        if let Some(first) = self.parse_non_expression_statement(first)? {
            self.parse_discarded_expression(first, ExpressionEnd::ExpressionStatement)?;
        }
        Ok(())
    }

    // If the token does not start a dedicated statement, return it as the
    // start of an expression. Bare braces, labels and stores take priority.
    fn parse_non_expression_statement(&mut self, first: Token) -> Result<Option<Token>, Error> {
        match first.kind {
            TokenKind::Semicolon => {}
            TokenKind::Declaration(kind) => self.parse_declaration(kind)?,
            TokenKind::OpenBrace
            | TokenKind::If
            | TokenKind::While
            | TokenKind::For
            | TokenKind::Do => self.open_statement(first, self.labels.len())?,
            TokenKind::Break | TokenKind::Continue => self.parse_statement_jump(&first)?,
            TokenKind::Update(op) => {
                self.parse_prefix_update(op, first.span, ExpressionEnd::StatementStore)?;
            }
            TokenKind::Identifier => {
                let next = self.next_token()?;
                if matches!(
                    next.as_ref().map(|token| token.kind),
                    Some(TokenKind::Colon)
                ) {
                    self.open_labeled_statement(first.span)?;
                } else if self
                    .parse_name_store(first.span, next, ExpressionEnd::StatementStore)?
                    .is_none()
                {
                    return Ok(Some(first));
                }
            }
            _ => return Ok(Some(first)),
        }
        Ok(None)
    }

    fn open_labeled_statement(&mut self, mut label: Span) -> Result<(), Error> {
        let label_start = self.labels.len();
        let missing = "expected a block, if, or loop after the label";
        loop {
            // Each stored span follows an already metered name and colon.
            try_push(&mut self.labels, label, label)?;
            let first = self.require_token(missing)?;
            match first.kind {
                TokenKind::OpenBrace
                | TokenKind::If
                | TokenKind::While
                | TokenKind::For
                | TokenKind::Do => {
                    return self.open_statement(first, label_start);
                }
                TokenKind::CloseBrace => return Err(Error::malformed(first.span, missing)),
                TokenKind::Identifier => {
                    if let Some(Token {
                        kind: TokenKind::Colon,
                        ..
                    }) = self.next_token()?
                    {
                        label = first.span;
                        continue;
                    }
                }
                _ => {}
            }
            return Err(Error::unsupported(
                first.span,
                "a label must directly precede a braced block, if, or loop",
            ));
        }
    }

    fn check_label_prefix(&mut self, start: usize) -> Result<(), Error> {
        for index in start..self.labels.len() {
            let name = self.labels[index];
            // Preserve the first label's old cost. Charge each additional
            // prefix entry's validation as well as the candidates it visits.
            if index != start {
                self.lexer.charge(1, name)?;
            }
            if self.has_label(name, start, index)? || self.find_active_label(name, start)?.is_some()
            {
                return Err(Error::malformed(name, "duplicate active label"));
            }
        }
        Ok(())
    }

    fn has_label(&mut self, name: Span, start: usize, end: usize) -> Result<bool, Error> {
        for index in (start..end).rev() {
            self.lexer.charge(1, name)?;
            if self.names_equal(name, self.labels[index])? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn find_active_label(
        &mut self,
        name: Span,
        mut label_end: usize,
    ) -> Result<Option<usize>, Error> {
        // Only active spans precede label_end: any pending prefix follows it.
        // The first alias visit pays for its frame; unlabeled loops still
        // charge one frame visit.
        let mut next = self.innermost_target;
        while let Some(index) = next {
            let Some(JumpTarget {
                label_start,
                enclosing_target,
                ..
            }) = self.statement_frames[index].statement.target
            else {
                unreachable!("active target chain must refer only to target frames");
            };
            if label_start == label_end {
                self.lexer.charge(1, name)?;
            } else if self.has_label(name, label_start, label_end)? {
                return Ok(Some(index));
            }
            label_end = label_start;
            next = enclosing_target;
        }
        Ok(None)
    }

    fn parse_statement_jump(&mut self, keyword: &Token) -> Result<(), Error> {
        let is_continue = matches!(keyword.kind, TokenKind::Continue);
        let (index, next) = match self.next_token()? {
            Some(Token {
                kind: TokenKind::Identifier,
                span,
                after_line_break,
            }) => {
                if after_line_break {
                    return Err(Error::unsupported(
                        span,
                        "a line terminator cannot precede a jump target",
                    ));
                }
                let index = self
                    .find_active_label(span, self.labels.len())?
                    .ok_or_else(|| Error::malformed(span, "jump target is not an active label"))?;
                if is_continue
                    && !matches!(
                        self.statement_frames[index].statement.kind,
                        StatementKind::Loop { .. }
                    )
                {
                    return Err(Error::malformed(span, "continue target must label a loop"));
                }
                (index, self.next_token()?)
            }
            Some(Token {
                kind: TokenKind::Declaration(BindingKind::Let),
                span,
                after_line_break: false,
            }) => {
                return Err(Error::unsupported(
                    span,
                    "let cannot be used as a label name",
                ));
            }
            // Unlabeled jumps go directly to the stored innermost loop index.
            next => (
                self.innermost_loop.ok_or_else(|| {
                    Error::malformed(keyword.span, "break and continue require an enclosing loop")
                })?,
                next,
            ),
        };
        let missing = "break and continue require explicit semicolons";
        let Some(next) = next else {
            return Err(Error::unsupported(Span::at(self.lexer.offset), missing));
        };
        if !matches!(next.kind, TokenKind::Semicolon) {
            // An apparent ASI boundary still cannot terminate a jump here.
            return Err(
                if next.after_line_break || matches!(next.kind, TokenKind::CloseBrace) {
                    Error::unsupported(next.span, missing)
                } else {
                    Error::malformed(next.span, "expected ';' after jump")
                },
            );
        }
        let statement = &mut self.statement_frames[index].statement;
        let head = if is_continue {
            let StatementKind::Loop { layout, .. } = &mut statement.kind else {
                unreachable!("continue target must be a loop");
            };
            match layout {
                LoopLayout::PreTest {
                    continue_target, ..
                } => {
                    let target = *continue_target;
                    return self.emit(Instruction::LoopBack(target), next.span);
                }
                LoopLayout::PostTest { continue_head, .. } => continue_head,
            }
        } else {
            let Some(target) = &mut statement.target else {
                unreachable!("jump target index must refer to a target frame");
            };
            &mut target.break_head
        };
        let previous = std::mem::replace(head, self.instructions.len());
        self.emit(Instruction::JumpStatement(previous), next.span)
    }

    fn patch_pending_jumps(&mut self, mut head: usize, close: Span) -> Result<(), Error> {
        let target = self.instructions.len();
        while head != UNPATCHED {
            // Link each pending jump at emission and visit it once at the target's close.
            self.lexer.charge(1, close)?;
            let Some(Instruction::JumpStatement(previous)) = self.instructions.get_mut(head) else {
                return Err(Error::malformed(close, "invalid pending statement jump"));
            };
            if *previous != UNPATCHED && *previous >= head {
                return Err(Error::malformed(
                    close,
                    "invalid pending statement jump link",
                ));
            }
            head = *previous;
            *previous = target;
        }
        Ok(())
    }

    fn parse_declaration(&mut self, kind: BindingKind) -> Result<(), Error> {
        let missing_semicolon = match kind {
            BindingKind::Const => "const declarations require explicit semicolons",
            BindingKind::Let => "let declarations require explicit semicolons",
        };
        self.parse_declaration_list(kind, DeclaratorEnd::Statement(missing_semicolon))
    }

    fn parse_declaration_list(
        &mut self,
        kind: BindingKind,
        end: DeclaratorEnd,
    ) -> Result<(), Error> {
        while self.parse_declarator(kind, end)? {}
        Ok(())
    }

    // Emit and record one binding before reading the next; true means a comma
    // was consumed and another binding name is required.
    fn parse_declarator(&mut self, kind: BindingKind, mode: DeclaratorEnd) -> Result<bool, Error> {
        let name = self.require_token("expected a binding name")?;
        match name.kind {
            TokenKind::Identifier => {}
            TokenKind::Declaration(_) => {
                return Err(Error::unsupported(
                    name.span,
                    "const and let are reserved names",
                ));
            }
            TokenKind::Boolean(_) => {
                return Err(Error::unsupported(
                    name.span,
                    "Boolean literals cannot be used as binding names",
                ));
            }
            TokenKind::Null => {
                return Err(Error::unsupported(
                    name.span,
                    "null cannot be used as a binding name",
                ));
            }
            TokenKind::Operator(Operator::Void) => {
                return Err(Error::unsupported(
                    name.span,
                    "void cannot be used as a binding name",
                ));
            }
            TokenKind::If
            | TokenKind::Else
            | TokenKind::While
            | TokenKind::For
            | TokenKind::Do
            | TokenKind::Break
            | TokenKind::Continue => {
                return Err(Error::unsupported(
                    name.span,
                    "control-flow keywords are reserved names",
                ));
            }
            TokenKind::OpenBrace => {
                return Err(Error::unsupported(
                    name.span,
                    "destructuring is outside the subset",
                ));
            }
            _ => return Err(Error::malformed(name.span, "expected a binding name")),
        }
        let scope = self.current_scope();
        // The classic Script global scope cannot lexically redeclare its
        // non-configurable `undefined` property. Nested scopes can shadow it.
        if self.scopes[scope].parent.is_none() && self.is_undefined_name(name.span) {
            return Err(Error::malformed(
                name.span,
                "undefined cannot be declared in the Script scope",
            ));
        }
        if self.find_in_scope(scope, name.span)?.is_some() {
            return Err(Error::malformed(name.span, "duplicate binding declaration"));
        }
        let expected_delimiter = match kind {
            BindingKind::Const => "expected '=' after the binding name",
            BindingKind::Let => "expected '=', ',' or ';' after the binding name",
        };
        let delimiter = self.require_token(expected_delimiter)?;
        let (end, separator) = match delimiter.kind {
            TokenKind::Comma | TokenKind::Semicolon if kind == BindingKind::Let => {
                // A metered primitive push, not a name read or a prefilled slot.
                self.emit(Instruction::Undefined, delimiter.span)?;
                (delimiter.span, Some(delimiter.kind))
            }
            TokenKind::Assignment(None) => {
                let first = self.require_token("expected a binding initializer")?;
                self.parse_expression(first, ExpressionEnd::Declarator(mode))?
            }
            TokenKind::Assignment(Some(_)) => {
                return Err(Error::unsupported(
                    delimiter.span,
                    "compound assignments cannot initialize declarations",
                ));
            }
            _ => {
                return Err(Error::malformed(delimiter.span, expected_delimiter));
            }
        };
        let slot = self.bindings.len();
        self.emit(Instruction::InitBinding { slot, kind }, end)?;
        let binding = Binding {
            name: name.span,
            kind,
            initialized_at: end.end,
            previous: self.scopes[scope].last_binding,
        };
        try_push(&mut self.bindings, binding, name.span)?;
        self.scopes[scope].last_binding = Some(slot);
        Ok(matches!(separator, Some(TokenKind::Comma)))
    }

    fn parse_prefix_update(
        &mut self,
        op: Update,
        operator: Span,
        end: ExpressionEnd,
    ) -> Result<Span, Error> {
        let name = self.parse_update_target(operator)?;
        self.parse_update(name, op, operator, end)
    }

    fn parse_update_target(&mut self, operator: Span) -> Result<Span, Error> {
        self.next_token()?
            .filter(|t| matches!(t.kind, TokenKind::Identifier))
            .map(|t| t.span)
            .ok_or_else(|| Error::unsupported(operator, UPDATE_TARGET_REQUIRED))
    }

    fn parse_update(
        &mut self,
        name: Span,
        op: Update,
        operator: Span,
        end: ExpressionEnd,
    ) -> Result<Span, Error> {
        let next = self.next_token()?;
        let span = next
            .as_ref()
            .map_or(Span::at(self.lexer.offset), |t| t.span);
        let kind = next.as_ref().map(|t| t.kind);
        // Only a complete, explicitly delimited update is a supported write.
        // Unlike an assignment statement, a statement-leading update owns no comma.
        match (end, kind) {
            (
                ExpressionEnd::StatementStore | ExpressionEnd::ForStore(ForClauseEnd::Init),
                Some(TokenKind::Semicolon),
            )
            | (ExpressionEnd::ForStore(ForClauseEnd::Update), Some(TokenKind::Close))
            | (ExpressionEnd::ForStore(_), Some(TokenKind::Comma)) => {}
            (ExpressionEnd::StatementStore, None | Some(TokenKind::CloseBrace)) => {
                return Err(Error::unsupported(
                    span,
                    "increments and decrements require explicit semicolons",
                ));
            }
            (ExpressionEnd::StatementStore, _) if next.is_some_and(|t| t.after_line_break) => {
                return Err(Error::unsupported(
                    span,
                    "increments and decrements require explicit semicolons",
                ));
            }
            (
                ExpressionEnd::ForStore(ForClauseEnd::Init),
                None | Some(TokenKind::Close | TokenKind::OpenBrace | TokenKind::CloseBrace),
            ) => {
                return Err(Error::malformed(span, FOR_SEMICOLON));
            }
            (
                ExpressionEnd::ForStore(ForClauseEnd::Update),
                None | Some(TokenKind::Semicolon | TokenKind::OpenBrace | TokenKind::CloseBrace),
            ) => {
                return Err(Error::malformed(span, FOR_UPDATE_CLOSE));
            }
            _ => {
                return Err(Error::unsupported(
                    operator,
                    "increment and decrement expressions are outside the subset at the start of a statement or for clause",
                ));
            }
        }
        let access = self.begin_write(name, operator, true)?;
        self.emit(Instruction::Number(1.0), operator)?;
        self.emit(
            match op {
                Update::Increment => Instruction::Add,
                Update::Decrement => Instruction::Subtract,
            },
            operator,
        )?;
        self.finish_write(access, span, false)?;
        self.parse_store_suffix(end, span, kind)
    }

    // The caller has already charged the name's emission work, just as for a
    // read. A completed update is one operand, not a pending unary operator.
    fn emit_expression_update(
        &mut self,
        name: Span,
        operator: Span,
        op: Update,
        result: UpdateResult,
    ) -> Result<(), Error> {
        self.begin_write(name, operator, false)?;
        self.emit_charged(
            Instruction::UpdateBinding {
                slot: UNPATCHED,
                op,
                result,
            },
            name,
        )
    }

    fn parse_assignment(
        &mut self,
        name: Span,
        op: Option<Operator>,
        operator_span: Span,
        end: ExpressionEnd,
    ) -> Result<Span, Error> {
        let access = self.begin_write(name, operator_span, op.is_some())?;
        // The assignment test is not part of the RHS expression grammar.
        let (skip, binary) = match op.map(Operator::instruction) {
            Some(Instruction::ShortCircuit { condition, .. }) => {
                let branch = self.instructions.len();
                self.emit(
                    Instruction::ShortCircuitStatement {
                        target: UNPATCHED,
                        condition,
                    },
                    operator_span,
                )?;
                (Some(branch), None)
            }
            binary => (None, binary),
        };
        let first = self.require_token("expected an assignment value")?;
        let (rhs_end, delimiter) = self.parse_expression(first, end)?;
        if let Some(binary) = binary {
            self.emit(binary, operator_span)?;
        }
        self.finish_write(access, rhs_end, false)?;
        if let Some(skip) = skip {
            self.patch_jump(skip, rhs_end)?;
        }
        self.parse_store_suffix(end, rhs_end, delimiter)
    }

    fn parse_store_suffix(
        &mut self,
        end: ExpressionEnd,
        span: Span,
        delimiter: Option<TokenKind>,
    ) -> Result<Span, Error> {
        let suffix_end = match (end, delimiter) {
            (ExpressionEnd::StatementStore, Some(TokenKind::Comma)) => {
                ExpressionEnd::StatementStoreSuffix
            }
            (ExpressionEnd::ForStore(clause), Some(TokenKind::Comma)) => {
                ExpressionEnd::ForClause(clause)
            }
            _ => return Ok(span),
        };
        // The comma was already consumed, and the store (or its skip edge)
        // leaves no value to discard. Parse just the remaining operands.
        let first = self.require_token("expected an operand before end of source")?;
        self.parse_discarded_expression(first, suffix_end)
    }

    fn begin_write(&mut self, name: Span, operator_span: Span, load: bool) -> Result<usize, Error> {
        // Record one target before any RHS reads to preserve diagnostic order.
        let write = WriteAccess {
            operator_span,
            load: load.then_some(self.instructions.len()),
        };
        let access = self.record_access(name, Some(write))?;
        if load {
            self.emit(Instruction::LoadBinding(UNPATCHED), name)?;
        }
        Ok(access)
    }

    fn finish_write(&mut self, access: usize, end: Span, keep: bool) -> Result<(), Error> {
        self.accesses[access].instruction = self.instructions.len();
        self.emit(
            if keep {
                Instruction::StoreBindingKeep(UNPATCHED)
            } else {
                Instruction::StoreBinding(UNPATCHED)
            },
            end,
        )
    }

    fn begin_expression_assignment(
        &mut self,
        name: Span,
        op: Option<Operator>,
        operator: Span,
        grouped: bool,
    ) -> Result<(), Error> {
        let access = self.begin_write(name, operator, op.is_some())?;
        // The assignment operator is not part of the RHS expression grammar.
        let tail = match op {
            None => AssignmentTail::Simple,
            Some(op) => match op.instruction() {
                branch @ Instruction::ShortCircuit { .. } => {
                    let skip = self.instructions.len();
                    self.emit(branch, operator)?;
                    AssignmentTail::ShortCircuit(skip)
                }
                _ => AssignmentTail::Eager(op),
            },
        };
        let assignment = ExpressionAssignment {
            access,
            tail,
            // An eager compound keeps its loaded target below the RHS. A
            // logical compound has popped it on the fallthrough path.
            rhs_base_depth: self.stack_depth,
        };
        if grouped {
            let Some(group @ (Pending::Group, _)) = self.pending.last_mut() else {
                return Err(Error::malformed(operator, "expected an assignment group"));
            };
            *group = (Pending::AssignmentGroup(assignment), operator);
            Ok(())
        } else {
            self.open_pending(Pending::Assignment(assignment), operator)
        }
    }

    fn finish_group_assignment(&mut self, end: Span) -> Result<(), Error> {
        if let Some((pending, operator)) = self.pending.last_mut()
            && let Pending::AssignmentGroup(assignment) = *pending
        {
            // The assignment ends here, but only ')' can close the group.
            // Reuse the marker and its nesting slot for the rest of the sequence.
            *pending = Pending::Group;
            let operator = *operator;
            self.finish_expression_assignment(assignment, operator, end)?;
        }
        Ok(())
    }

    fn finish_expression_assignment(
        &mut self,
        ExpressionAssignment {
            access,
            tail,
            rhs_base_depth,
        }: ExpressionAssignment,
        operator: Span,
        end: Span,
    ) -> Result<(), Error> {
        if self.stack_depth.checked_sub(rhs_base_depth) != Some(1) {
            return Err(Error::malformed(
                end,
                "assignment value must leave one result",
            ));
        }
        if let AssignmentTail::Eager(binary) = tail {
            self.emit(binary.instruction(), operator)?;
        }
        self.finish_write(access, end, true)?;
        if let AssignmentTail::ShortCircuit(skip) = tail {
            // Both paths leave one value above rhs_base_depth:
            // the old value on skip, or the RHS after the store. Join here,
            // before the enclosing arm jump, assignment tail or discard.
            self.patch_jump(skip, end)?;
        }
        Ok(())
    }

    fn record_access(&mut self, name: Span, write: Option<WriteAccess>) -> Result<usize, Error> {
        let index = self.accesses.len();
        let scope = self.current_scope();
        try_push(
            &mut self.accesses,
            Access {
                name,
                scope,
                instruction: self.instructions.len(),
                write,
            },
            name,
        )?;
        Ok(index)
    }

    fn names_equal(&mut self, name: Span, candidate: Span) -> Result<bool, Error> {
        let source = self.lexer.source;
        let text = &source[name.start..name.end];
        let other = &source[candidate.start..candidate.end];
        if text.len() != other.len() {
            return Ok(false);
        }
        self.lexer.charge(text.len(), name)?;
        Ok(text == other)
    }

    fn find_in_scope(&mut self, scope: usize, span: Span) -> Result<Option<usize>, Error> {
        let mut next = self.scopes[scope].last_binding;
        while let Some(slot) = next {
            self.lexer.charge(1, span)?;
            let Binding { name, previous, .. } = self.bindings[slot];
            if self.names_equal(span, name)? {
                return Ok(Some(slot));
            }
            next = previous;
        }
        Ok(None)
    }

    fn is_undefined_name(&self, name: Span) -> bool {
        &self.lexer.source[name.start..name.end] == "undefined"
    }

    fn resolve_access(&mut self, access: Access) -> Result<Option<usize>, Error> {
        let mut scope = access.scope;
        loop {
            // Resolve by lexical proximity before checking initialization or mutability.
            // In particular, do not skip a later declaration to find an earlier one.
            if let Some(slot) = self.find_in_scope(scope, access.name)? {
                let binding = &self.bindings[slot];
                if access.name.start < binding.initialized_at {
                    return Err(Error::unsupported(
                        access.name,
                        "access before lexical binding initialization is outside the subset",
                    ));
                }
                if let Some(write) = access.write
                    && binding.kind == BindingKind::Const
                {
                    return Err(Error::unsupported(
                        write.operator_span,
                        "assignment to const is outside the subset",
                    ));
                }
                return Ok(Some(slot));
            }
            let Some(parent) = self.scopes[scope].parent else {
                // Only an unbound read gets the immutable default. A declaration
                // in any visible scope, even in its TDZ, wins over this fallback.
                if access.write.is_none() && self.is_undefined_name(access.name) {
                    return Ok(None);
                }
                return Err(Error::unsupported(
                    access.name,
                    "unbound name is outside the subset",
                ));
            };
            self.lexer.charge(1, access.name)?;
            scope = parent;
        }
    }

    fn resolve_accesses(&mut self) -> Result<(), Error> {
        // Resolve each name site once, even when it emits both a load and a store.
        for index in 0..self.accesses.len() {
            let access = self.accesses[index];
            let slot = self.resolve_access(access)?;
            match (
                access.write,
                slot,
                self.instructions.get_mut(access.instruction),
            ) {
                (None, _, Some(op @ Instruction::LoadBinding(UNPATCHED))) => {
                    *op = slot.map_or(Instruction::Undefined, Instruction::LoadBinding);
                }
                (
                    Some(WriteAccess { load: None, .. }),
                    Some(slot),
                    Some(Instruction::UpdateBinding {
                        slot: binding @ UNPATCHED,
                        ..
                    }),
                ) => *binding = slot,
                (
                    Some(write),
                    Some(slot),
                    Some(
                        Instruction::StoreBinding(binding @ UNPATCHED)
                        | Instruction::StoreBindingKeep(binding @ UNPATCHED),
                    ),
                ) => {
                    *binding = slot;
                    if let Some(load) = write.load {
                        let Some(op @ Instruction::LoadBinding(UNPATCHED)) =
                            self.instructions.get_mut(load)
                        else {
                            return Err(Error::malformed(
                                access.name,
                                "invalid pending write target load",
                            ));
                        };
                        *op = Instruction::LoadBinding(slot);
                    }
                }
                _ => {
                    return Err(Error::malformed(
                        access.name,
                        "invalid pending binding access",
                    ));
                }
            }
        }
        Ok(())
    }

    fn parse_expression(
        &mut self,
        first: Token,
        end_kind: ExpressionEnd,
    ) -> Result<(Span, Option<TokenKind>), Error> {
        // Compound assignment keeps its loaded LHS below the RHS operands.
        let base_depth = self.stack_depth;
        let statement_test = matches!(end_kind, ExpressionEnd::StatementTest);
        let for_update = matches!(
            end_kind,
            ExpressionEnd::ForClause(ForClauseEnd::Update)
                | ExpressionEnd::ForStore(ForClauseEnd::Update)
        );
        let for_delimiter = match end_kind {
            ExpressionEnd::ForClause(ForClauseEnd::Init)
            | ExpressionEnd::ForStore(ForClauseEnd::Init)
            | ExpressionEnd::ForTest
            | ExpressionEnd::Declarator(DeclaratorEnd::ForHeader) => Some(FOR_SEMICOLON),
            _ if for_update => Some(FOR_UPDATE_CLOSE),
            _ => None,
        };
        let missing_semicolon = match end_kind {
            ExpressionEnd::ExpressionStatement => Some(EXPRESSION_STATEMENT_SEMICOLON),
            ExpressionEnd::StatementStore | ExpressionEnd::StatementStoreSuffix => {
                Some(ASSIGNMENT_STATEMENT_SEMICOLON)
            }
            ExpressionEnd::Declarator(DeclaratorEnd::Statement(message)) => Some(message),
            _ => None,
        };
        let mut needs_operand = true;
        // Statement tests receive their already checked opening parenthesis.
        // Meter it like a group, but let it own the test's close and comma
        // sequence without reusing the test marker for an assignment.
        let (mut next, mut after_open) = if statement_test {
            self.open_pending(Pending::StatementTest, first.span)?;
            (self.next_token()?, true)
        } else {
            (Some(first), false)
        };
        // Bare assignment prefixes can start a test, RHS, ternary arm, or
        // noninitial sequence operand, but not an operator operand. Store
        // suffixes start after the first comma. ForClause may also start where
        // parse_for_clause has ruled out a leading store.
        let mut allow_assignment = statement_test
            || matches!(
                end_kind,
                ExpressionEnd::ForTest
                    | ExpressionEnd::Declarator(_)
                    | ExpressionEnd::StatementStore
                    | ExpressionEnd::StatementStoreSuffix
                    | ExpressionEnd::ForStore(_)
                    | ExpressionEnd::ForClause(_)
            );
        let (end, delimiter) = loop {
            let Some(token) = next else {
                let eof = Span::at(self.lexer.offset);
                if needs_operand {
                    return Err(Error::malformed(
                        eof,
                        "expected an operand before end of source",
                    ));
                }
                break (eof, None);
            };
            let Token {
                kind,
                span,
                after_line_break,
            } = token;
            let group_start =
                after_open && matches!(self.pending.last(), Some((Pending::Group, _)));
            match (needs_operand, kind) {
                (true, TokenKind::Update(op)) => {
                    let name = self.parse_update_target(span)?;
                    self.lexer.charge(1, name)?;
                    self.emit_expression_update(name, span, op, UpdateResult::New)?;
                    needs_operand = false;
                }
                (false, TokenKind::Update(_)) => {
                    return Err(Error::unsupported(span, UPDATE_TARGET_REQUIRED));
                }
                (_, TokenKind::Assignment(_)) => {
                    return Err(Error::unsupported(
                        span,
                        "assignment expressions require a simple name followed by an assignment operator at the start of an expression group",
                    ));
                }
                (_, TokenKind::Declaration(_)) => {
                    if !needs_operand
                        && after_line_break
                        && let Some(message) = missing_semicolon
                    {
                        self.reduce_expression(span)?;
                        if self.pending.is_empty() {
                            return Err(Error::unsupported(span, message));
                        }
                    }
                    return Err(Error::unsupported(
                        span,
                        "declarations are not expression tokens",
                    ));
                }
                (
                    _,
                    TokenKind::If
                    | TokenKind::Else
                    | TokenKind::While
                    | TokenKind::For
                    | TokenKind::Do
                    | TokenKind::Break
                    | TokenKind::Continue,
                ) => {
                    return Err(Error::unsupported(
                        span,
                        "control-flow keywords are not expression tokens",
                    ));
                }
                (false, TokenKind::CloseBrace) if let Some(message) = missing_semicolon => {
                    return Err(Error::unsupported(span, message));
                }
                (false, TokenKind::OpenBrace) if statement_test => {
                    return Err(Error::malformed(
                        span,
                        "expected ')' after the statement test",
                    ));
                }
                (false, TokenKind::OpenBrace | TokenKind::CloseBrace)
                    if let Some(message) = for_delimiter =>
                {
                    return Err(Error::malformed(span, message));
                }
                (_, TokenKind::OpenBrace | TokenKind::CloseBrace) => {
                    return Err(Error::unsupported(span, "braces are not expression tokens"));
                }
                (true, TokenKind::Number(value)) => {
                    self.emit(Instruction::Number(value), span)?;
                    needs_operand = false;
                }
                (true, TokenKind::Boolean(value)) => {
                    self.emit(Instruction::Boolean(value), span)?;
                    needs_operand = false;
                }
                (true, TokenKind::Null) => {
                    self.emit(Instruction::Null, span)?;
                    needs_operand = false;
                }
                (true, TokenKind::Identifier) => {
                    // Charge the load, assignment marker or update at the name
                    // before looking ahead, preserving the read's work order.
                    self.lexer.charge(1, span)?;
                    match self.next_token()? {
                        Some(Token {
                            kind: TokenKind::Assignment(op),
                            span: operator,
                            ..
                        }) if group_start || allow_assignment => {
                            self.begin_expression_assignment(span, op, operator, group_start)?;
                            after_open = false;
                            allow_assignment = true;
                            next = self.next_token()?;
                            continue;
                        }
                        Some(Token {
                            kind: TokenKind::Update(op),
                            span: operator,
                            after_line_break,
                        }) => {
                            check_postfix_line_break(operator, after_line_break)?;
                            self.emit_expression_update(span, operator, op, UpdateResult::Old)?;
                        }
                        following => {
                            self.lookahead = Some(following);
                            self.record_access(span, None)?;
                            self.emit_charged(Instruction::LoadBinding(UNPATCHED), span)?;
                        }
                    }
                    needs_operand = false;
                }
                (true, TokenKind::Open) => {
                    self.open_pending(Pending::Group, span)?;
                }
                (true, TokenKind::Operator(op)) => {
                    let prefix = match op {
                        Operator::Add => Operator::Positive,
                        Operator::Subtract => Operator::Negate,
                        Operator::Not => Operator::Not,
                        Operator::BitwiseNot => Operator::BitwiseNot,
                        Operator::Void => Operator::Void,
                        Operator::Divide => {
                            // In operand position a slash could begin a regexp, not division.
                            return Err(Error::unsupported(
                                span,
                                "regular expression literals are outside the arithmetic subset",
                            ));
                        }
                        _ => return Err(Error::malformed(span, "expected an operand")),
                    };
                    // Don't pop pending operators: another prefix applies before this one.
                    self.push_pending(Pending::Op(prefix), span)?;
                }
                (true, TokenKind::Close) if after_open => {
                    if matches!(self.pending.last(), Some((Pending::StatementTest, _))) {
                        return Err(Error::malformed(
                            span,
                            "expected a statement test expression",
                        ));
                    }
                    // Empty parens can start an arrow function; don't misreport that as malformed.
                    self.lexer.skip_trivia()?;
                    let rest = self.lexer.rest();
                    if let Some(prefix) = ["=>", "<!--", "-->"]
                        .into_iter()
                        .find(|p| rest.starts_with(p))
                    {
                        let start = self.lexer.offset;
                        return Err(Error::unsupported(
                            Span {
                                start,
                                end: start + prefix.len(),
                            },
                            "arrow syntax or HTML comments are outside the arithmetic subset",
                        ));
                    }
                    return Err(Error::malformed(span, "empty parenthesized expression"));
                }
                (false, TokenKind::Comma) => {
                    // Comma is looser than ?: and =. Finish pending subexpressions
                    // before deciding which surrounding production owns the comma.
                    self.reduce_expression(span)?;
                    self.finish_group_assignment(span)?;
                    match self.pending.last() {
                        Some((Pending::Consequent { .. }, _)) => {
                            return Err(Error::malformed(span, "expected ':' before ','"));
                        }
                        None if matches!(
                            end_kind,
                            ExpressionEnd::Declarator(_)
                                | ExpressionEnd::StatementStore
                                | ExpressionEnd::ForStore(_)
                        ) =>
                        {
                            break (span, Some(kind));
                        }
                        Some((Pending::Group | Pending::StatementTest, _)) => {}
                        None if matches!(
                            end_kind,
                            ExpressionEnd::Script
                                | ExpressionEnd::ExpressionStatement
                                | ExpressionEnd::StatementStoreSuffix
                                | ExpressionEnd::ForTest
                                | ExpressionEnd::ForClause(_)
                        ) => {}
                        _ => {
                            return Err(Error::unsupported(
                                span,
                                "a sequence in an assignment RHS requires parentheses",
                            ));
                        }
                    }
                    // Discard the left result before the next operand, with no pending
                    // operator or additional nesting for a flat sequence.
                    self.emit(Instruction::Discard, span)?;
                    needs_operand = true;
                }
                (true, _) => return Err(Error::malformed(span, "expected an operand")),
                (false, TokenKind::Operator(Operator::Not)) => {
                    return Err(Error::unsupported(
                        span,
                        "'!' is only supported as a prefix operator",
                    ));
                }
                (false, TokenKind::Operator(Operator::BitwiseNot)) => {
                    return Err(Error::unsupported(
                        span,
                        "'~' is only supported as a prefix operator",
                    ));
                }
                (false, TokenKind::Question) => {
                    // Lower precedence than every operator here, but do not close an
                    // enclosing conditional: this one starts within that arm.
                    self.reduce_operators(0)?;
                    let branch = self.instructions.len();
                    self.emit(Instruction::JumpIfFalse(UNPATCHED), span)?;
                    self.open_pending(
                        Pending::Consequent {
                            branch,
                            base_depth: self.stack_depth,
                        },
                        span,
                    )?;
                    needs_operand = true;
                }
                (false, TokenKind::Colon) => {
                    self.start_alternate(span)?;
                    needs_operand = true;
                }
                (false, TokenKind::Close) => {
                    self.reduce_expression(span)?;
                    self.finish_group_assignment(span)?;
                    match self.pending.pop() {
                        Some((Pending::StatementTest, _)) => {
                            self.nesting_depth -= 1;
                            // Do not consume the opening brace or do-while semicolon.
                            break (span, Some(kind));
                        }
                        Some((Pending::Group, _)) => self.nesting_depth -= 1,
                        Some((Pending::Consequent { .. }, _)) => {
                            return Err(Error::malformed(
                                span,
                                "expected ':' before closing parenthesis",
                            ));
                        }
                        None if for_update => {
                            break (span, Some(kind));
                        }
                        None if let Some(message) = for_delimiter => {
                            return Err(Error::malformed(span, message));
                        }
                        _ => return Err(Error::malformed(span, "unmatched closing parenthesis")),
                    }
                }
                (false, TokenKind::Open) => {
                    return Err(Error::unsupported(
                        span,
                        "call expressions are outside the arithmetic subset",
                    ));
                }
                (
                    false,
                    TokenKind::Number(_)
                    | TokenKind::Boolean(_)
                    | TokenKind::Null
                    | TokenKind::Identifier
                    | TokenKind::Operator(Operator::Void),
                ) => {
                    if after_line_break && for_delimiter.is_none() {
                        self.reduce_expression(span)?;
                        if self.pending.is_empty() {
                            return Err(Error::unsupported(
                                span,
                                missing_semicolon.unwrap_or(EXPRESSION_STATEMENT_SEMICOLON),
                            ));
                        }
                    }
                    return Err(Error::malformed(span, "expected a binary operator"));
                }
                (false, TokenKind::Operator(op)) => {
                    self.push_infix(op, span)?;
                    needs_operand = true;
                }
                (false, TokenKind::Semicolon) if for_update => {
                    return Err(Error::malformed(span, FOR_UPDATE_CLOSE));
                }
                (false, TokenKind::Semicolon) => break (span, Some(kind)),
            }
            after_open = matches!(kind, TokenKind::Open);
            // Only a comma that emitted Discard reaches here; separators
            // and commas rejected by the surrounding production do not.
            allow_assignment = matches!(
                kind,
                TokenKind::Question | TokenKind::Colon | TokenKind::Comma
            );
            next = self.next_token()?;
        };
        let at_eof = delimiter.is_none();
        self.reduce_expression(end)?;
        if let Some((pending, _)) = self.pending.last() {
            let message = match pending {
                Pending::Group | Pending::StatementTest | Pending::AssignmentGroup(_) if at_eof => {
                    "expected a closing parenthesis before end of source"
                }
                Pending::Group | Pending::StatementTest | Pending::AssignmentGroup(_) => {
                    "semicolon inside a parenthesized expression"
                }
                _ => "expected ':' to complete conditional expression",
            };
            return Err(Error::malformed(end, message));
        }
        if at_eof && let Some(message) = missing_semicolon {
            return Err(Error::unsupported(end, message));
        }
        if at_eof && let Some(message) = for_delimiter {
            return Err(Error::malformed(end, message));
        }
        if self.stack_depth.checked_sub(base_depth) != Some(1) {
            return Err(Error::malformed(
                end,
                "expected exactly one complete expression",
            ));
        }
        Ok((end, delimiter))
    }

    fn push_infix(&mut self, op: Operator, span: Span) -> Result<(), Error> {
        let pending = if let branch @ Instruction::ShortCircuit { .. } = op.instruction() {
            // Reduce the tighter operators (starting at bitwise OR precedence)
            // to expose the closest pending short circuit without reducing it.
            // All earlier short circuits in this region have already agreed on
            // the family. A Group or conditional arm marker ends the region.
            self.reduce_operators(Operator::BitwiseOr.precedence())?;
            if let Some((Pending::ShortCircuit { op: previous, .. }, _)) = self.pending.last()
                && matches!(previous, Operator::Coalesce) != matches!(op, Operator::Coalesce)
            {
                return Err(Error::malformed(
                    span,
                    "'??' cannot be mixed with '&&' or '||' without parentheses",
                ));
            }
            // Equal precedence reduces first; a preceding branch joins before
            // this operator consumes its left operand.
            self.reduce_operators(op.precedence())?;
            let jump = self.instructions.len();
            let join_depth = self.stack_depth; // LHS stays on the taken path.
            self.emit(branch, span)?; // Track the fallthrough path, which pops the LHS.
            Pending::ShortCircuit {
                op,
                jump,
                join_depth,
            }
        } else if matches!(op, Operator::Exponentiate) {
            // Only prefix operators bind more tightly. A prefix on this level
            // is still pending, while a prefix inside a closed group is not.
            if let Some((Pending::Op(previous), _)) = self.pending.last()
                && previous.precedence() > op.precedence()
            {
                return Err(Error::malformed(
                    span,
                    "a unary base of exponentiation requires parentheses",
                ));
            }
            // Keep equal precedence pending: `**` associates to the right.
            Pending::Op(op)
        } else {
            self.reduce_operators(op.precedence())?;
            Pending::Op(op)
        };
        self.push_pending(pending, span)
    }

    fn open_pending(&mut self, marker: Pending, span: Span) -> Result<(), Error> {
        self.enter_nesting(span)?;
        self.push_pending(marker, span)
    }

    fn enter_nesting(&mut self, span: Span) -> Result<(), Error> {
        if self.nesting_depth == self.max_nesting_depth {
            return Err(Error::limit(span, Limit::NestingDepth));
        }
        self.nesting_depth += 1;
        Ok(())
    }

    fn push_pending(&mut self, entry: Pending, span: Span) -> Result<(), Error> {
        self.lexer.charge(1, span)?;
        try_push(&mut self.pending, (entry, span), span)
    }

    fn reduce_operators(&mut self, min_precedence: u8) -> Result<(), Error> {
        while let Some(&(pending, span)) = self.pending.last() {
            match pending {
                Pending::Op(op) if op.precedence() >= min_precedence => {
                    self.emit(op.instruction(), span)?;
                }
                Pending::ShortCircuit {
                    op,
                    jump,
                    join_depth,
                } if op.precedence() >= min_precedence => {
                    if self.stack_depth != join_depth {
                        return Err(Error::malformed(
                            span,
                            "short-circuit operands must each leave one result",
                        ));
                    }
                    self.patch_jump(jump, span)?;
                }
                _ => break,
            }
            self.pending.pop();
        }
        Ok(())
    }

    // At a delimiter, finish conditionals and ungrouped assignments inside out.
    // '?' drained operators at its level, and an assignment began at
    // a test, RHS, arm or sequence operand prefix, so neither marker has
    // operators immediately beneath it. An arm's assignments finish before
    // its conditional joins; an assignment whose RHS is that conditional
    // finishes after the join. Groups, the statement test's outer delimiter,
    // and consequents still awaiting ':' stop reduction.
    fn reduce_expression(&mut self, end: Span) -> Result<(), Error> {
        self.reduce_operators(0)?;
        while let Some(&(pending, span)) = self.pending.last() {
            match pending {
                Pending::Alternate { jump, join_depth } => {
                    if self.stack_depth != join_depth {
                        return Err(Error::malformed(
                            span,
                            "conditional branches must each leave one result",
                        ));
                    }
                    self.patch_jump(jump, span)?;
                    self.nesting_depth -= 1;
                }
                Pending::Assignment(assignment) => {
                    self.finish_expression_assignment(assignment, span, end)?;
                    self.nesting_depth -= 1;
                }
                _ => break,
            }
            self.pending.pop();
        }
        Ok(())
    }

    fn start_alternate(&mut self, span: Span) -> Result<(), Error> {
        self.reduce_expression(span)?;
        let Some((Pending::Consequent { branch, base_depth }, _)) = self.pending.pop() else {
            return Err(Error::malformed(span, "':' without a matching '?'"));
        };
        let join_depth = self.stack_depth;
        if join_depth.checked_sub(1) != Some(base_depth) {
            return Err(Error::malformed(
                span,
                "conditional branches must each leave one result",
            ));
        }
        let jump = self.instructions.len();
        self.emit(Instruction::Jump(UNPATCHED), span)?;
        self.patch_jump(branch, span)?;
        self.stack_depth = base_depth;
        self.push_pending(Pending::Alternate { jump, join_depth }, span)
    }

    fn patch_jump(&mut self, at: usize, span: Span) -> Result<(), Error> {
        // Target the current emission frontier. It becomes the next instruction
        // (possibly another jump), or the one-past-end exit of the final Script.
        // Since `at` must index an already emitted jump, the target is forward.
        let target = self.instructions.len();
        let slot = match self.instructions.get_mut(at) {
            Some(
                Instruction::JumpIfFalse(slot)
                | Instruction::ShortCircuit { target: slot, .. }
                | Instruction::Jump(slot),
            ) if *slot == UNPATCHED => slot,
            Some(
                Instruction::JumpIfFalseStatement(slot)
                | Instruction::JumpStatement(slot)
                | Instruction::ShortCircuitStatement { target: slot, .. },
            ) if *slot == UNPATCHED && self.stack_depth == 0 => slot,
            _ => return Err(Error::malformed(span, "invalid forward jump")),
        };
        *slot = target;
        Ok(())
    }

    fn emit(&mut self, instruction: Instruction, span: Span) -> Result<(), Error> {
        self.lexer.charge(1, span)?;
        self.emit_charged(instruction, span)
    }

    fn emit_charged(&mut self, instruction: Instruction, span: Span) -> Result<(), Error> {
        let (operands, results) = match instruction {
            Instruction::Number(value) if value.is_sign_negative() || value.is_nan() => {
                return Err(Error::malformed(span, "invalid Number literal value"));
            }
            Instruction::Number(_)
            | Instruction::Boolean(_)
            | Instruction::Null
            | Instruction::Undefined
            | Instruction::LoadBinding(_)
            | Instruction::UpdateBinding { .. } => (0, 1),
            Instruction::JumpIfFalseStatement(_) | Instruction::ShortCircuitStatement { .. }
                if self.stack_depth != 1 =>
            {
                return Err(Error::malformed(
                    span,
                    "expected one statement test operand",
                ));
            }
            Instruction::JumpStatement(_) if self.stack_depth != 0 => {
                return Err(Error::malformed(
                    span,
                    "statement jump requires an empty operand stack",
                ));
            }
            Instruction::LoopBack(target)
                if target >= self.instructions.len() || self.stack_depth != 0 =>
            {
                return Err(Error::malformed(span, "invalid loop back edge"));
            }
            Instruction::Discard
            | Instruction::InitBinding { .. }
            | Instruction::StoreBinding(_)
            | Instruction::JumpIfFalse(_)
            | Instruction::JumpIfFalseStatement(_)
            | Instruction::ShortCircuit { .. }
            | Instruction::ShortCircuitStatement { .. } => (1, 0),
            Instruction::Jump(_) | Instruction::JumpStatement(_) | Instruction::LoopBack(_) => {
                (0, 0)
            }
            Instruction::Positive
            | Instruction::StoreBindingKeep(_)
            | Instruction::Negate
            | Instruction::Not
            | Instruction::BitwiseNot
            | Instruction::Void => (1, 1),
            Instruction::Add
            | Instruction::Subtract
            | Instruction::Multiply
            | Instruction::Divide
            | Instruction::Remainder
            | Instruction::Exponentiate
            | Instruction::BitwiseAnd
            | Instruction::BitwiseXor
            | Instruction::BitwiseOr
            | Instruction::ShiftLeft
            | Instruction::ShiftRight
            | Instruction::ShiftRightUnsigned
            | Instruction::LooseEqual
            | Instruction::LooseNotEqual
            | Instruction::StrictEqual
            | Instruction::StrictNotEqual
            | Instruction::LessThan
            | Instruction::LessThanOrEqual
            | Instruction::GreaterThan
            | Instruction::GreaterThanOrEqual => (2, 1),
        };
        self.stack_depth = self
            .stack_depth
            .checked_sub(operands)
            .ok_or_else(|| Error::malformed(span, "operation is missing an operand"))?
            .checked_add(results)
            .ok_or_else(|| Error::limit(span, Limit::Allocation))?;
        self.max_stack_depth = self.max_stack_depth.max(self.stack_depth);
        try_push(&mut self.instructions, instruction, span)
    }
}

fn try_push<T>(vec: &mut Vec<T>, item: T, span: Span) -> Result<(), Error> {
    vec.try_reserve(1)
        .map_err(|_| Error::limit(span, Limit::Allocation))?;
    vec.push(item);
    Ok(())
}
