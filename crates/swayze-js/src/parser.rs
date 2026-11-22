use std::fmt;

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
    /// Maximum number of non-whitespace tokens, including punctuation.
    pub max_tokens: usize,
    /// Maximum number of simultaneously open parentheses.
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
    NestingDepth,
    Work,
    Allocation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    /// The recognized syntax cannot be completed here.
    Malformed,
    /// Parsing would require syntax outside the slice. This is not a validity claim.
    Unsupported,
    /// A configured ceiling or available allocation was exhausted.
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
            Limit::NestingDepth => "parenthesis nesting ceiling exceeded",
            Limit::Work => "parse work ceiling exceeded",
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

/// A validated Script in the arithmetic subset. Only the parser constructs it.
pub struct Script {
    instructions: Vec<Instruction>,
    max_stack_depth: usize,
}

impl Script {
    /// The number of literals and operators in the expression.
    pub fn instruction_count(&self) -> usize {
        self.instructions().len()
    }

    /// The peak operand stack size required by the validated postfix order.
    pub fn max_stack_depth(&self) -> usize {
        self.max_stack_depth
    }

    pub(crate) fn instructions(&self) -> &[Instruction] {
        &self.instructions
    }
}

impl fmt::Debug for Script {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Script")
            .field("instruction_count", &self.instruction_count())
            .field("max_stack_depth", &self.max_stack_depth())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Instruction {
    Number(f64),
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    Positive,
    Negate,
}

/// Parse a complete Script in the arithmetic subset with default [`Limits`].
pub fn parse_script(source: &str) -> Result<Script, Error> {
    parse_script_with_limits(source, Limits::default())
}

/// Parse a complete Script in the arithmetic subset with caller-supplied limits.
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
        max_nesting_depth: limits.max_nesting_depth,
        pending: Vec::new(),
        postfix: Vec::new(),
        stack_depth: 0,
        max_stack_depth: 0,
    }
    .parse()
}

#[derive(Clone, Copy, Debug)]
enum Operator {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    Positive,
    Negate,
}

impl Operator {
    fn precedence(self) -> u8 {
        match self {
            Self::Add | Self::Subtract => 1,
            Self::Multiply | Self::Divide | Self::Remainder => 2,
            Self::Positive | Self::Negate => 3,
        }
    }

    fn instruction(self) -> Instruction {
        match self {
            Self::Add => Instruction::Add,
            Self::Subtract => Instruction::Subtract,
            Self::Multiply => Instruction::Multiply,
            Self::Divide => Instruction::Divide,
            Self::Remainder => Instruction::Remainder,
            Self::Positive => Instruction::Positive,
            Self::Negate => Instruction::Negate,
        }
    }
}

#[derive(Clone, Copy)]
enum TokenKind {
    Number(f64),
    Operator(Operator),
    Open,
    Close,
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

impl Lexer<'_> {
    fn rest(&self) -> &str {
        // `offset` starts at zero and only advances by complete UTF-8 scalars.
        &self.source[self.offset..]
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

    fn skip_whitespace(&mut self) -> Result<bool, Error> {
        let mut line_break = false;
        while let Some(c) = self.rest().chars().next() {
            match c {
                '\n' | '\r' | '\u{2028}' | '\u{2029}' => line_break = true,
                // ECMA-262 WhiteSpace (not Rust's `char::is_whitespace`).
                '\t'
                | '\u{000B}'
                | '\u{000C}'
                | ' '
                | '\u{00A0}'
                | '\u{1680}'
                | '\u{2000}'..='\u{200A}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}' => {}
                _ => break,
            }
            self.consume(c)?;
        }
        Ok(line_break)
    }

    fn next(&mut self) -> Result<Option<Token>, Error> {
        let after_line_break = self.skip_whitespace()?;
        let Some(c) = self.rest().chars().next() else {
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
            '(' => TokenKind::Open,
            ')' => TokenKind::Close,
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

    fn operator(&mut self, c: char, start: usize) -> Result<TokenKind, Error> {
        if let Some(next) = self.rest().chars().next() {
            if c == '/' && matches!(next, '/' | '*') {
                self.consume(next)?;
                return Err(Error::unsupported(
                    Span {
                        start,
                        end: self.offset,
                    },
                    "comments are outside the arithmetic subset",
                ));
            }
            if next == '=' || (next == c && matches!(c, '+' | '-' | '*')) {
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

    fn number(&mut self, first: char, start: usize) -> Result<TokenKind, Error> {
        while let Some(c @ '0'..='9') = self.rest().chars().next() {
            let span = self.consume(c)?;
            if first == '0' {
                return Err(Error::unsupported(
                    span,
                    "leading-zero numeric syntax is outside the subset",
                ));
            }
        }
        if let Some(c) = self.rest().chars().next()
            && (c.is_ascii_alphabetic() || matches!(c, '.' | '_' | '$' | '\\'))
        {
            let span = self.consume(c)?;
            return Err(Error::unsupported(
                span,
                "adjacent characters require non-subset numeric or identifier lexing",
            ));
        }

        let span = Span {
            start,
            end: self.offset,
        };
        let digits = span.end - span.start;
        let value = if digits > 309 {
            // A canonical decimal with 310+ digits is at least 10^309.
            f64::INFINITY
        } else {
            self.charge(digits, span)?;
            self.source[start..self.offset]
                .parse::<f64>()
                .map_err(|_| Error::malformed(span, "could not convert decimal literal"))?
        };
        Ok(TokenKind::Number(value))
    }
}

// `None` is a delimiter; otherwise this is a pending prefix or binary operator.
type Pending = (Option<Operator>, Span);

struct Parser<'s> {
    lexer: Lexer<'s>,
    max_nesting_depth: usize,
    pending: Vec<Pending>,
    postfix: Vec<Instruction>,
    stack_depth: usize,
    max_stack_depth: usize,
}

impl Parser<'_> {
    fn parse(mut self) -> Result<Script, Error> {
        let mut nesting = 0;
        let mut needs_operand = true;
        let mut seen_token = false;
        let mut terminated = false;
        while let Some(token) = self.lexer.next()? {
            let Token {
                kind,
                span,
                after_line_break,
            } = token;
            if terminated {
                return Err(Error::unsupported(
                    span,
                    "only one expression statement is supported",
                ));
            }
            match (needs_operand, kind) {
                (true, TokenKind::Number(value)) => {
                    self.emit(Instruction::Number(value), span)?;
                    needs_operand = false;
                }
                (true, TokenKind::Open) => {
                    if nesting == self.max_nesting_depth {
                        return Err(Error::limit(span, Limit::NestingDepth));
                    }
                    nesting += 1;
                    self.push_pending(None, span)?;
                }
                (true, TokenKind::Operator(op)) => {
                    let prefix = match op {
                        Operator::Add => Operator::Positive,
                        Operator::Subtract => Operator::Negate,
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
                    self.push_pending(Some(prefix), span)?;
                }
                (true, TokenKind::Semicolon) if !seen_token => {
                    return Err(Error::unsupported(
                        span,
                        "empty statements are outside the arithmetic subset",
                    ));
                }
                (true, TokenKind::Close) if matches!(self.pending.last(), Some((None, _))) => {
                    // Empty parens can start an arrow function; don't misreport that as malformed.
                    self.lexer.skip_whitespace()?;
                    let rest = self.lexer.rest();
                    if let Some(prefix) = ["=>", "/*", "//", "<!--", "-->"]
                        .into_iter()
                        .find(|p| rest.starts_with(p))
                    {
                        let start = self.lexer.offset;
                        return Err(Error::unsupported(
                            Span {
                                start,
                                end: start + prefix.len(),
                            },
                            "arrow syntax or comments are outside the arithmetic subset",
                        ));
                    }
                    return Err(Error::malformed(span, "empty parenthesized expression"));
                }
                (true, _) => return Err(Error::malformed(span, "expected an operand")),
                (false, TokenKind::Operator(op)) => {
                    while let Some(&(Some(prev), at)) = self.pending.last() {
                        if prev.precedence() < op.precedence() {
                            break;
                        }
                        self.pending.pop();
                        self.emit(prev.instruction(), at)?;
                    }
                    self.push_pending(Some(op), span)?;
                    needs_operand = true;
                }
                (false, TokenKind::Close) => {
                    nesting = nesting
                        .checked_sub(1)
                        .ok_or_else(|| Error::malformed(span, "unmatched closing parenthesis"))?;
                    while let Some((Some(op), at)) = self.pending.pop() {
                        self.emit(op.instruction(), at)?;
                    }
                }
                (false, TokenKind::Open) => {
                    return Err(Error::unsupported(
                        span,
                        "call expressions are outside the arithmetic subset",
                    ));
                }
                (false, TokenKind::Number(_)) if nesting == 0 && after_line_break => {
                    return Err(Error::unsupported(
                        span,
                        "only one expression statement is supported",
                    ));
                }
                (false, TokenKind::Number(_)) => {
                    return Err(Error::malformed(span, "expected a binary operator"));
                }
                (false, TokenKind::Semicolon) if nesting == 0 => terminated = true,
                (false, TokenKind::Semicolon) => {
                    return Err(Error::malformed(
                        span,
                        "semicolon inside a parenthesized expression",
                    ));
                }
            }
            seen_token = true;
        }

        let eof = Span::at(self.lexer.offset);
        if !seen_token {
            return Err(Error::unsupported(
                eof,
                "empty scripts are outside the arithmetic subset",
            ));
        }
        if needs_operand {
            return Err(Error::malformed(
                eof,
                "expected an operand before end of source",
            ));
        }
        if nesting != 0 {
            return Err(Error::malformed(
                eof,
                "expected a closing parenthesis before end of source",
            ));
        }
        while let Some((op, at)) = self.pending.pop() {
            let op = op.ok_or_else(|| Error::malformed(at, "unmatched opening parenthesis"))?;
            self.emit(op.instruction(), at)?;
        }
        if self.stack_depth != 1 {
            return Err(Error::malformed(
                eof,
                "expected exactly one complete expression",
            ));
        }
        Ok(Script {
            instructions: self.postfix,
            max_stack_depth: self.max_stack_depth,
        })
    }

    fn push_pending(&mut self, op: Option<Operator>, span: Span) -> Result<(), Error> {
        self.lexer.charge(1, span)?;
        try_push(&mut self.pending, (op, span), span)
    }

    fn emit(&mut self, instruction: Instruction, span: Span) -> Result<(), Error> {
        self.lexer.charge(1, span)?;
        let operands = match instruction {
            Instruction::Number(value) if value.is_sign_negative() || value.is_nan() => {
                return Err(Error::malformed(span, "invalid decimal Number value"));
            }
            Instruction::Number(_) => 0,
            Instruction::Positive | Instruction::Negate => 1,
            Instruction::Add
            | Instruction::Subtract
            | Instruction::Multiply
            | Instruction::Divide
            | Instruction::Remainder => 2,
        };
        self.stack_depth = self
            .stack_depth
            .checked_sub(operands)
            .ok_or_else(|| Error::malformed(span, "operation is missing an operand"))?
            .checked_add(1)
            .ok_or_else(|| Error::limit(span, Limit::Allocation))?;
        self.max_stack_depth = self.max_stack_depth.max(self.stack_depth);
        try_push(&mut self.postfix, instruction, span)
    }
}

fn try_push<T>(vec: &mut Vec<T>, item: T, span: Span) -> Result<(), Error> {
    vec.try_reserve(1)
        .map_err(|_| Error::limit(span, Limit::Allocation))?;
    vec.push(item);
    Ok(())
}
