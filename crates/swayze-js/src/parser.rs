use std::fmt;

// Fixed ceiling for decimal literals with a point or exponent.
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
    /// and pending conditional expressions. Statement frames have a separate fixed cap.
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
    /// Fixed byte ceiling for a decimal literal with a point or exponent.
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
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    Positive,
    Negate,
    Not,
    BitwiseNot,
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
    Jump(usize),
    // Unconditional statement transfers require an empty operand stack.
    JumpStatement(usize),
    // The only backward edge; enter a loop test or update with an empty stack.
    LoopBack(usize),
}

/// Parse a complete Script in the supported subset with default [`Limits`].
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
    Positive,
    Negate,
    Not,
    BitwiseNot,
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
            Self::Positive | Self::Negate | Self::Not | Self::BitwiseNot => 11,
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
            Self::Positive => Instruction::Positive,
            Self::Negate => Instruction::Negate,
            Self::Not => Instruction::Not,
            Self::BitwiseNot => Instruction::BitwiseNot,
        }
    }
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
    Break,
    Continue,
    Equals,
    Operator(Operator),
    Open,
    Close,
    OpenBrace,
    CloseBrace,
    Question,
    Colon,
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
            | "delete" | "do" | "enum" | "export" | "extends"
            | "finally" | "function" | "import" | "in" | "instanceof"
            | "new" | "return" | "super" | "switch" | "this" | "throw"
            | "try" | "typeof" | "var" | "void" | "with" | "await" | "yield"
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
            '&' | '|' | '^' => self.bitwise_or_logical(c, start)?,
            '~' => TokenKind::Operator(Operator::BitwiseNot),
            // Annex B HTML-like comments remain outside the subset.
            '<' if self.rest().starts_with("!--") => {
                return Err(Error::unsupported(
                    first,
                    "HTML comments are outside the subset",
                ));
            }
            '<' | '>' => self.relational_or_shift(c, start)?,
            '(' => TokenKind::Open,
            ')' => TokenKind::Close,
            '{' => TokenKind::OpenBrace,
            '}' => TokenKind::CloseBrace,
            '?' => self.question(start)?,
            ':' => TokenKind::Colon,
            ';' => TokenKind::Semicolon,
            '+' | '-' | '*' | '/' | '%' => self.operator(c, start)?,
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
            "break" => Ok(TokenKind::Break),
            "continue" => Ok(TokenKind::Continue),
            "true" => Ok(TokenKind::Boolean(true)),
            "false" => Ok(TokenKind::Boolean(false)),
            "null" => Ok(TokenKind::Null),
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
                return Ok(TokenKind::Equals);
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

    fn bitwise_or_logical(&mut self, first: char, start: usize) -> Result<TokenKind, Error> {
        let doubled = matches!(first, '&' | '|') && self.peek() == Some(first);
        if doubled {
            self.consume(first)?;
        }
        if self.peek() == Some('=') {
            self.consume('=')?;
            return Err(Error::unsupported(
                Span {
                    start,
                    end: self.offset,
                },
                "compound assignments are outside the subset",
            ));
        }
        Ok(TokenKind::Operator(match (first, doubled) {
            ('&', false) => Operator::BitwiseAnd,
            ('^', false) => Operator::BitwiseXor,
            ('|', false) => Operator::BitwiseOr,
            ('&', true) => Operator::LogicalAnd,
            ('|', true) => Operator::LogicalOr,
            _ => unreachable!("not a bitwise or logical operator"),
        }))
    }

    fn relational_or_shift(&mut self, first: char, start: usize) -> Result<TokenKind, Error> {
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
            if self.peek() == Some('=') {
                self.consume('=')?;
                return Err(Error::unsupported(
                    Span {
                        start,
                        end: self.offset,
                    },
                    "compound assignments are outside the subset",
                ));
            }
            return Ok(TokenKind::Operator(op));
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
            if self.peek() == Some('=') {
                self.consume('=')?;
            } else {
                return Ok(TokenKind::Operator(Operator::Coalesce));
            }
        } else if self.rest().strip_prefix('.').is_some_and(|rest| {
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

    fn operator(&mut self, c: char, start: usize) -> Result<TokenKind, Error> {
        if let Some(next) = self.peek()
            && (next == '=' || (next == c && matches!(c, '+' | '-' | '*')))
        {
            self.consume(next)?;
            if c == '*' && next == '*' && self.rest().starts_with('=') {
                self.consume('=')?;
            }
            return Err(Error::unsupported(
                Span {
                    start,
                    end: self.offset,
                },
                "longer punctuator is outside the arithmetic subset",
            ));
        }
        Ok(TokenKind::Operator(match c {
            '+' => Operator::Add,
            '-' => Operator::Subtract,
            '*' => Operator::Multiply,
            '/' => Operator::Divide,
            '%' => Operator::Remainder,
            _ => unreachable!("not an arithmetic operator"),
        }))
    }

    fn decimal_digits(&mut self) -> Result<(), Error> {
        while let Some(c @ '0'..='9') = self.peek() {
            self.consume(c)?;
        }
        Ok(())
    }

    fn reject_numeric_suffix(&mut self) -> Result<(), Error> {
        // Decline adjacent non-ASCII non-whitespace without classifying Unicode identifiers.
        if let Some(c) = self.peek()
            && (c.is_ascii_alphabetic()
                || matches!(c, '.' | '_' | '$' | '\\')
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

    fn number(&mut self, first: char, start: usize) -> Result<TokenKind, Error> {
        if first == '0' {
            if let Some(c @ '0'..='9') = self.peek() {
                let span = self.consume(c)?;
                return Err(Error::unsupported(
                    span,
                    "leading-zero numeric syntax is outside the subset",
                ));
            }
        } else {
            self.decimal_digits()?;
        }

        let mut integer_only = first != '.';
        if integer_only && self.peek() == Some('.') {
            self.consume('.')?;
            self.decimal_digits()?;
            integer_only = false;
        }
        if let Some(marker @ ('e' | 'E')) = self.peek() {
            self.consume(marker)?;
            if let Some(sign @ ('+' | '-')) = self.peek() {
                self.consume(sign)?;
            }
            let digits_start = self.offset;
            self.decimal_digits()?;
            if self.offset == digits_start {
                // An adjacent identifier or separator is still unsupported, not
                // necessarily an attempt to write an exponent in this grammar.
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
        let value = if integer_only && bytes > 309 {
            // A canonical integer with 310+ digits is at least 10^309.
            f64::INFINITY
        } else {
            if bytes > MAX_DECIMAL_LITERAL_BYTES {
                return Err(Error::limit(span, Limit::DecimalLiteralBytes));
            }
            self.charge(bytes, span)?;
            self.source[start..self.offset]
                .parse::<f64>()
                .map_err(|_| Error::malformed(span, "could not convert decimal literal"))?
        };
        Ok(TokenKind::Number(value))
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
    Group,
    Consequent {
        branch: usize,
        base_depth: usize,
    },
    Alternate {
        jump: usize,
        join_depth: usize,
    },
}

// All unresolved jump targets and binding operands are patched before returning a Script.
const UNPATCHED: usize = usize::MAX;

// Root is index 0; every block (including each else arm) gets its own index.
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
    // Present for a write, for the diagnostic if the resolved binding is const.
    equals: Option<Span>,
}

enum ExpressionEnd {
    Script,
    Semicolon(&'static str),
    StatementTest,
    ForSemicolon,
    ForClose,
}

// One frame per active statement, including across both arms of an if.
// Every test starts and every arm enters and joins at operand depth zero.
#[derive(Clone, Copy)]
enum StatementFrame {
    IfThen {
        branch: usize,
    },
    IfElse {
        join: usize,
    },
    Loop {
        continue_target: usize,
        exit: usize,
        enclosing_loop: Option<usize>,
        // Head of a list threaded through unpatched break instructions.
        break_head: usize,
    },
}

struct BlockFrame {
    statement: StatementFrame,
    scope: usize,
}

struct Parser<'s> {
    lexer: Lexer<'s>,
    // Cache the next lexer result (including EOF) after a top-level name or
    // the first if branch, so disambiguation never re-lexes a token.
    lookahead: Option<Option<Token>>,
    max_nesting_depth: usize,
    nesting_depth: usize,
    pending: Vec<(Pending, Span)>,
    statement_frames: Vec<BlockFrame>,
    // Index into statement_frames; opening or closing an if does not change it.
    innermost_loop: Option<usize>,
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
        loop {
            if !self.statement_frames.is_empty() {
                self.parse_block_item()?;
                continue;
            }
            let first = self.next_token()?.ok_or_else(|| {
                Error::unsupported(
                    Span::at(self.lexer.offset),
                    if self.instructions.is_empty() {
                        "empty scripts are outside the arithmetic subset"
                    } else {
                        "a final expression is required after statements"
                    },
                )
            })?;
            self.check_statement_boundary(first.span)?;
            match first.kind {
                TokenKind::Declaration(kind) => {
                    self.parse_declaration(kind)?;
                    continue;
                }
                TokenKind::If | TokenKind::While | TokenKind::For => {
                    self.open_statement(first)?;
                    continue;
                }
                TokenKind::Break | TokenKind::Continue => {
                    self.parse_loop_jump(&first)?;
                    continue;
                }
                TokenKind::Identifier => {
                    let next = self.next_token()?;
                    if let Some(Token {
                        kind: TokenKind::Equals,
                        span,
                        ..
                    }) = next
                    {
                        self.parse_assignment_statement(first.span, span)?;
                        continue;
                    }
                    self.lookahead = Some(next);
                }
                _ => {}
            }
            self.parse_expression(first, ExpressionEnd::Script)?;
            break;
        }
        if let Some(token) = self.next_token()? {
            if matches!(token.kind, TokenKind::Break | TokenKind::Continue) {
                // A jump here also has no enclosing loop.
                self.parse_loop_jump(&token)?;
            }
            return Err(Error::unsupported(
                token.span,
                "no additional statements are supported after the final expression",
            ));
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
        self.statement_frames.last().map_or(0, |frame| frame.scope)
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

    fn open_statement(&mut self, keyword: Token) -> Result<(), Error> {
        if self.statement_frames.len() == MAX_STATEMENT_FRAMES {
            return Err(Error {
                message: "statement nesting ceiling exceeded",
                ..Error::limit(keyword.span, Limit::NestingDepth)
            });
        }
        let (frame, missing_brace) = match keyword.kind {
            TokenKind::If => {
                let branch = self.parse_statement_test("expected '(' after 'if'")?;
                (
                    StatementFrame::IfThen { branch },
                    "expected '{' to open an if branch",
                )
            }
            TokenKind::While => {
                let test = self.instructions.len();
                let exit = self.parse_statement_test("expected '(' after 'while'")?;
                (
                    self.loop_frame(test, exit),
                    "expected '{' to open the while body",
                )
            }
            TokenKind::For => (
                self.parse_for_header()?,
                "expected '{' to open the for body",
            ),
            _ => unreachable!("not a control statement keyword"),
        };
        let brace = self.require_block_open(missing_brace)?;
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
        if matches!(frame, StatementFrame::Loop { .. }) {
            self.innermost_loop = Some(index);
        }
        Ok(())
    }

    fn close_block(&mut self, close: Span) -> Result<(), Error> {
        let index = self
            .statement_frames
            .len()
            .checked_sub(1)
            .ok_or_else(|| Error::malformed(close, "unmatched closing brace"))?;
        match self.statement_frames[index].statement {
            StatementFrame::IfThen { branch } => {
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
                    self.patch_jump(branch, span)?;
                    let brace = self.require_block_open("expected '{' after 'else'")?;
                    // Same control frame, disjoint lexical scope with the same parent.
                    let parent = self.scopes[self.statement_frames[index].scope].parent;
                    let scope = self.add_scope(parent, brace)?;
                    self.statement_frames[index] = BlockFrame {
                        statement: StatementFrame::IfElse { join },
                        scope,
                    };
                    return Ok(());
                }
                self.lookahead = Some(next);
                self.patch_jump(branch, close)?;
            }
            StatementFrame::IfElse { join } => self.patch_jump(join, close)?,
            StatementFrame::Loop {
                continue_target,
                exit,
                enclosing_loop,
                break_head,
            } => {
                self.emit(Instruction::LoopBack(continue_target), close)?;
                self.patch_jump(exit, close)?;
                self.patch_breaks(break_head, close)?;
                self.innermost_loop = enclosing_loop;
            }
        }
        self.statement_frames.pop();
        Ok(())
    }

    fn loop_frame(&self, continue_target: usize, exit: usize) -> StatementFrame {
        StatementFrame::Loop {
            continue_target,
            exit,
            enclosing_loop: self.innermost_loop,
            break_head: UNPATCHED,
        }
    }

    fn parse_for_header(&mut self) -> Result<StatementFrame, Error> {
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
        if matches!(init.kind, TokenKind::Close) {
            return Err(Error::malformed(
                init.span,
                "expected ';' between for clauses",
            ));
        }
        if !matches!(init.kind, TokenKind::Semicolon) {
            self.parse_for_assignment(init, ExpressionEnd::ForSemicolon)?;
        }

        let test = self.instructions.len();
        let first = self.require_token("expected a for test or ';'")?;
        let end = if matches!(first.kind, TokenKind::Semicolon) {
            self.emit(Instruction::Boolean(true), first.span)?;
            first.span
        } else {
            self.parse_expression(first, ExpressionEnd::ForSemicolon)?
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
            let close = self.parse_for_assignment(first, ExpressionEnd::ForClose)?;
            self.emit(Instruction::LoopBack(test), close)?;
            self.patch_jump(skip_update, close)?;
            (update, close)
        };
        self.nesting_depth -= 1;
        self.check_statement_boundary(close)?;
        Ok(self.loop_frame(continue_target, exit))
    }

    fn parse_for_assignment(&mut self, first: Token, end: ExpressionEnd) -> Result<Span, Error> {
        let unsupported = "for initializers and updates must be simple assignments";
        if !matches!(first.kind, TokenKind::Identifier) {
            return Err(Error::unsupported(first.span, unsupported));
        }
        let equals = self.require_token("expected '=' after the assignment name")?;
        if !matches!(equals.kind, TokenKind::Equals) {
            return Err(Error::unsupported(equals.span, unsupported));
        }
        self.parse_assignment(first.span, equals.span, end)
    }

    fn parse_statement_test(&mut self, missing_open: &'static str) -> Result<usize, Error> {
        let open = self.require_token(missing_open)?;
        if !matches!(open.kind, TokenKind::Open) {
            return Err(Error::malformed(open.span, missing_open));
        }
        let end = self.parse_expression(open, ExpressionEnd::StatementTest)?;
        self.emit_statement_test(end)
    }

    fn emit_statement_test(&mut self, end: Span) -> Result<usize, Error> {
        let branch = self.instructions.len();
        // The test leaves one value, which the jump consumes on both paths.
        self.emit(Instruction::JumpIfFalseStatement(UNPATCHED), end)?;
        Ok(branch)
    }

    fn require_block_open(&mut self, missing_open: &'static str) -> Result<Span, Error> {
        let open = self.require_token(missing_open)?;
        match open.kind {
            TokenKind::OpenBrace => Ok(open.span),
            TokenKind::If => Err(Error::unsupported(
                open.span,
                "if branches and loop bodies require braces; else if is outside the subset",
            )),
            _ => Err(Error::malformed(open.span, missing_open)),
        }
    }

    fn parse_block_item(&mut self) -> Result<(), Error> {
        let first = self.require_token("expected a statement or '}' in the block")?;
        self.check_statement_boundary(first.span)?;
        match first.kind {
            TokenKind::CloseBrace => self.close_block(first.span),
            TokenKind::Declaration(kind) => self.parse_declaration(kind),
            TokenKind::If | TokenKind::While | TokenKind::For => self.open_statement(first),
            TokenKind::Break | TokenKind::Continue => self.parse_loop_jump(&first),
            TokenKind::Identifier => {
                let equals = self.require_token("expected '=' after the assignment name")?;
                if !matches!(equals.kind, TokenKind::Equals) {
                    return Err(Error::unsupported(
                        equals.span,
                        "only simple assignments are supported in these blocks",
                    ));
                }
                self.parse_assignment_statement(first.span, equals.span)
            }
            _ => Err(Error::unsupported(
                first.span,
                "only declarations, assignments and supported control statements are allowed in these blocks",
            )),
        }
    }

    fn parse_loop_jump(&mut self, keyword: &Token) -> Result<(), Error> {
        let next = self.next_token()?;
        if let Some(Token {
            kind: TokenKind::Identifier | TokenKind::Declaration(BindingKind::Let),
            span,
            after_line_break: false,
        }) = next
        {
            return Err(Error::unsupported(
                span,
                "labeled loop jumps are outside the subset",
            ));
        }
        // The stored frame index bypasses any intervening if frames.
        let index = self.innermost_loop.ok_or_else(|| {
            Error::malformed(keyword.span, "break and continue require an enclosing loop")
        })?;
        let missing = "loop jumps require explicit semicolons";
        let Some(next) = next else {
            return Err(Error::unsupported(Span::at(self.lexer.offset), missing));
        };
        if !matches!(next.kind, TokenKind::Semicolon) {
            // An apparent ASI boundary still cannot terminate a jump here.
            return Err(
                if next.after_line_break || matches!(next.kind, TokenKind::CloseBrace) {
                    Error::unsupported(next.span, missing)
                } else {
                    Error::malformed(next.span, "expected ';' after loop jump")
                },
            );
        }
        let StatementFrame::Loop {
            continue_target,
            break_head,
            ..
        } = &mut self.statement_frames[index].statement
        else {
            unreachable!("innermost loop index must refer to a loop frame");
        };
        let instruction = if matches!(keyword.kind, TokenKind::Continue) {
            Instruction::LoopBack(*continue_target)
        } else {
            let previous = std::mem::replace(break_head, self.instructions.len());
            Instruction::JumpStatement(previous)
        };
        self.emit(instruction, next.span)
    }

    fn patch_breaks(&mut self, mut head: usize, close: Span) -> Result<(), Error> {
        let target = self.instructions.len();
        while head != UNPATCHED {
            // Each break is linked at emission and visited once at its loop's close.
            self.lexer.charge(1, close)?;
            let Some(Instruction::JumpStatement(previous)) = self.instructions.get_mut(head) else {
                return Err(Error::malformed(close, "invalid pending break"));
            };
            if *previous != UNPATCHED && *previous >= head {
                return Err(Error::malformed(close, "invalid pending break link"));
            }
            head = *previous;
            *previous = target;
        }
        Ok(())
    }

    fn parse_declaration(&mut self, kind: BindingKind) -> Result<(), Error> {
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
            TokenKind::If
            | TokenKind::Else
            | TokenKind::While
            | TokenKind::For
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
        // non-configurable `undefined` property. Block scopes can shadow it.
        if self.scopes[scope].parent.is_none() && self.is_undefined_name(name.span) {
            return Err(Error::malformed(
                name.span,
                "undefined cannot be declared in the Script scope",
            ));
        }
        if self.find_in_scope(scope, name.span)?.is_some() {
            return Err(Error::malformed(name.span, "duplicate binding declaration"));
        }
        let equals = self.require_token("expected '=' after the binding name")?;
        if !matches!(equals.kind, TokenKind::Equals) {
            return Err(Error::malformed(
                equals.span,
                "expected '=' after the binding name",
            ));
        }
        let first = self.require_token("expected a binding initializer")?;
        let missing_semicolon = match kind {
            BindingKind::Const => "const declarations require explicit semicolons",
            BindingKind::Let => "let declarations require explicit semicolons",
        };
        let end = self.parse_expression(first, ExpressionEnd::Semicolon(missing_semicolon))?;
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
        Ok(())
    }

    fn parse_assignment_statement(&mut self, name: Span, equals: Span) -> Result<(), Error> {
        let end = ExpressionEnd::Semicolon("assignments require explicit semicolons");
        self.parse_assignment(name, equals, end)?;
        Ok(())
    }

    fn parse_assignment(
        &mut self,
        name: Span,
        equals: Span,
        end: ExpressionEnd,
    ) -> Result<Span, Error> {
        // Record the target before the RHS reads to preserve diagnostic order.
        let access = self.record_access(name, Some(equals))?;
        let first = self.require_token("expected an assignment value")?;
        let end = self.parse_expression(first, end)?;
        self.accesses[access].instruction = self.instructions.len();
        self.emit(Instruction::StoreBinding(UNPATCHED), end)?;
        Ok(end)
    }

    fn record_access(&mut self, name: Span, equals: Option<Span>) -> Result<usize, Error> {
        let index = self.accesses.len();
        let scope = self.current_scope();
        try_push(
            &mut self.accesses,
            Access {
                name,
                scope,
                instruction: self.instructions.len(),
                equals,
            },
            name,
        )?;
        Ok(index)
    }

    fn find_in_scope(&mut self, scope: usize, span: Span) -> Result<Option<usize>, Error> {
        let source = self.lexer.source;
        let name = &source[span.start..span.end];
        let mut next = self.scopes[scope].last_binding;
        while let Some(slot) = next {
            self.lexer.charge(1, span)?;
            let binding = &self.bindings[slot];
            let candidate = &source[binding.name.start..binding.name.end];
            if name.len() == candidate.len() {
                self.lexer.charge(name.len(), span)?;
                if name == candidate {
                    return Ok(Some(slot));
                }
            }
            next = binding.previous;
        }
        Ok(None)
    }

    fn is_undefined_name(&self, name: Span) -> bool {
        &self.lexer.source[name.start..name.end] == "undefined"
    }

    fn resolve_access(&mut self, access: Access) -> Result<Instruction, Error> {
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
                if let Some(equals) = access.equals
                    && binding.kind == BindingKind::Const
                {
                    return Err(Error::unsupported(
                        equals,
                        "assignment to const is outside the subset",
                    ));
                }
                return Ok(if access.equals.is_some() {
                    Instruction::StoreBinding(slot)
                } else {
                    Instruction::LoadBinding(slot)
                });
            }
            let Some(parent) = self.scopes[scope].parent else {
                // Only an unbound read gets the immutable default. A declaration
                // in any visible scope, even in its TDZ, wins over this fallback.
                if access.equals.is_none() && self.is_undefined_name(access.name) {
                    return Ok(Instruction::Undefined);
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
        // One site per emitted load or store. The search alone takes extra work.
        for index in 0..self.accesses.len() {
            let access = self.accesses[index];
            let resolved = self.resolve_access(access)?;
            match self.instructions.get_mut(access.instruction) {
                Some(op @ Instruction::LoadBinding(UNPATCHED)) if access.equals.is_none() => {
                    *op = resolved;
                }
                Some(op @ Instruction::StoreBinding(UNPATCHED)) if access.equals.is_some() => {
                    *op = resolved;
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

    fn parse_expression(&mut self, first: Token, end_kind: ExpressionEnd) -> Result<Span, Error> {
        let statement_test = matches!(end_kind, ExpressionEnd::StatementTest);
        let for_delimiter = match end_kind {
            ExpressionEnd::ForSemicolon => Some("expected ';' between for clauses"),
            ExpressionEnd::ForClose => Some("expected ')' after the for update"),
            _ => None,
        };
        let missing_semicolon = match end_kind {
            ExpressionEnd::Semicolon(message) => Some(message),
            _ => None,
        };
        let mut needs_operand = true;
        let mut seen_token = false;
        let mut next = Some(first);
        let (end, at_eof) = loop {
            let Some(token) = next else {
                let eof = Span::at(self.lexer.offset);
                if needs_operand {
                    return Err(Error::malformed(
                        eof,
                        "expected an operand before end of source",
                    ));
                }
                break (eof, true);
            };
            let Token {
                kind,
                span,
                after_line_break,
            } = token;
            match (needs_operand, kind) {
                (_, TokenKind::Equals) => {
                    return Err(Error::unsupported(
                        span,
                        "assignment expressions are outside the subset",
                    ));
                }
                (_, TokenKind::Declaration(_)) => {
                    if !needs_operand
                        && after_line_break
                        && let Some(message) = missing_semicolon
                    {
                        self.reduce_expression()?;
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
                    return Err(Error::unsupported(
                        span,
                        "braces are only supported around if branches and loop bodies",
                    ));
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
                    self.record_access(span, None)?;
                    self.emit(Instruction::LoadBinding(UNPATCHED), span)?;
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
                (true, TokenKind::Semicolon)
                    if matches!(end_kind, ExpressionEnd::Script) && !seen_token =>
                {
                    return Err(Error::unsupported(
                        span,
                        "empty statements are outside the arithmetic subset",
                    ));
                }
                (true, TokenKind::Close)
                    if matches!(self.pending.last(), Some((Pending::Group, _))) =>
                {
                    if statement_test && self.pending.len() == 1 {
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
                (false, TokenKind::Operator(op)) => {
                    self.push_infix(op, span)?;
                    needs_operand = true;
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
                    self.reduce_expression()?;
                    match self.pending.pop() {
                        Some((Pending::Group, _)) => self.nesting_depth -= 1,
                        Some((Pending::Consequent { .. }, _)) => {
                            return Err(Error::malformed(
                                span,
                                "expected ':' before closing parenthesis",
                            ));
                        }
                        None if matches!(end_kind, ExpressionEnd::ForClose) => break (span, false),
                        None if let Some(message) = for_delimiter => {
                            return Err(Error::malformed(span, message));
                        }
                        _ => return Err(Error::malformed(span, "unmatched closing parenthesis")),
                    }
                    // The statement test starts with its required '(' as the first group.
                    // Stop at that group's close without consuming the block's '{'.
                    if statement_test && self.pending.is_empty() {
                        break (span, false);
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
                    | TokenKind::Identifier,
                ) => {
                    if after_line_break && for_delimiter.is_none() {
                        self.reduce_expression()?;
                        if self.pending.is_empty() {
                            return Err(Error::unsupported(
                                span,
                                missing_semicolon
                                    .unwrap_or("only one expression statement is supported"),
                            ));
                        }
                    }
                    return Err(Error::malformed(span, "expected a binary operator"));
                }
                (false, TokenKind::Semicolon) if matches!(end_kind, ExpressionEnd::ForClose) => {
                    return Err(Error::malformed(span, "expected ')' after the for update"));
                }
                (false, TokenKind::Semicolon) => break (span, false),
            }
            seen_token = true;
            next = self.next_token()?;
        };
        self.reduce_expression()?;
        if let Some((pending, _)) = self.pending.last() {
            let message = match pending {
                Pending::Group if at_eof => "expected a closing parenthesis before end of source",
                Pending::Group => "semicolon inside a parenthesized expression",
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
        if self.stack_depth != 1 {
            return Err(Error::malformed(
                end,
                "expected exactly one complete expression",
            ));
        }
        Ok(end)
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

    // At an expression delimiter, reduce through completed conditionals but stop
    // at an opening parenthesis or a conditional whose ':' has not been seen.
    // '?' drained operators at its level, so there are no operator entries
    // underneath the alternate markers until after the next delimiter.
    fn reduce_expression(&mut self) -> Result<(), Error> {
        self.reduce_operators(0)?;
        while let Some(&(Pending::Alternate { jump, join_depth }, span)) = self.pending.last() {
            if self.stack_depth != join_depth {
                return Err(Error::malformed(
                    span,
                    "conditional branches must each leave one result",
                ));
            }
            self.patch_jump(jump, span)?;
            self.pending.pop();
            self.nesting_depth -= 1;
        }
        Ok(())
    }

    fn start_alternate(&mut self, span: Span) -> Result<(), Error> {
        self.reduce_expression()?;
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
                | Instruction::JumpIfFalseStatement(slot)
                | Instruction::ShortCircuit { target: slot, .. }
                | Instruction::Jump(slot)
                | Instruction::JumpStatement(slot),
            ) if *slot == UNPATCHED => slot,
            _ => return Err(Error::malformed(span, "invalid forward jump")),
        };
        *slot = target;
        Ok(())
    }

    fn emit(&mut self, instruction: Instruction, span: Span) -> Result<(), Error> {
        self.lexer.charge(1, span)?;
        let (operands, results) = match instruction {
            Instruction::Number(value) if value.is_sign_negative() || value.is_nan() => {
                return Err(Error::malformed(span, "invalid decimal Number value"));
            }
            Instruction::Number(_)
            | Instruction::Boolean(_)
            | Instruction::Null
            | Instruction::Undefined
            | Instruction::LoadBinding(_) => (0, 1),
            Instruction::JumpIfFalseStatement(_) if self.stack_depth != 1 => {
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
            Instruction::InitBinding { .. }
            | Instruction::StoreBinding(_)
            | Instruction::JumpIfFalse(_)
            | Instruction::JumpIfFalseStatement(_)
            | Instruction::ShortCircuit { .. } => (1, 0),
            Instruction::Jump(_) | Instruction::JumpStatement(_) | Instruction::LoopBack(_) => {
                (0, 0)
            }
            Instruction::Positive
            | Instruction::Negate
            | Instruction::Not
            | Instruction::BitwiseNot => (1, 1),
            Instruction::Add
            | Instruction::Subtract
            | Instruction::Multiply
            | Instruction::Divide
            | Instruction::Remainder
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
