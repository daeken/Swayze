use super::{Error, Lexer, Limit, Span, is_line_terminator};

const MAX_STRING_LITERAL_BYTES: usize = 4096;
// Two delimiters emit nothing; a scalar, escape or normalized line ending
// emits at most as many UTF-16 units as the number of source bytes it consumes.
pub(crate) const MAX_STRING_LITERAL_CODE_UNITS: usize = MAX_STRING_LITERAL_BYTES - 2;
const MAX_STRING_LITERAL_UNITS: usize = 16_384;
const MAX_CODE_POINT: u32 = 0x10_FFFF;
const _: () = assert!(MAX_STRING_LITERAL_UNITS <= u16::MAX as usize);

/// Index and length in a single Script's UTF-16 pool, never a source span.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StringRange {
    start: u16,
    len: u16,
}

impl StringRange {
    pub(crate) fn get(self, pool: &[u16]) -> Option<&[u16]> {
        let start = usize::from(self.start);
        pool.get(start..start.checked_add(usize::from(self.len))?)
    }

    pub(crate) fn is_empty(self) -> bool {
        self.len == 0
    }
}

impl Lexer<'_> {
    // The opening quote or backtick has already been consumed and charged.
    pub(super) fn string_literal(
        &mut self,
        delimiter: char,
        start: usize,
    ) -> Result<StringRange, Error> {
        let template = delimiter == '`';
        let unit_start = self.string_units.len();
        while let Some(c) = self.peek() {
            let span = self.consume_string(c, start)?;
            if c == delimiter {
                return Ok(StringRange {
                    // The append ceiling bounds both fields, including the start of
                    // an empty literal immediately after a full pool.
                    start: unit_start as u16,
                    len: (self.string_units.len() - unit_start) as u16,
                });
            }
            let value = match c {
                '\n' | '\r' if !template => {
                    return Err(Error::malformed(
                        span,
                        "unescaped CR or LF in string literal",
                    ));
                }
                '\r' => {
                    self.finish_string_crlf(c, start)?;
                    u32::from('\n')
                }
                '$' if template && self.peek() == Some('{') => {
                    self.consume_string('{', start)?;
                    return Err(Error::unsupported(
                        Span {
                            start: span.start,
                            end: self.offset,
                        },
                        "template interpolation is outside the subset",
                    ));
                }
                '\\' => match self.string_escape(start, span.start, template)? {
                    Some(value) => value,
                    None => continue,
                },
                // This includes raw NUL, LS and PS, and a template's LF.
                _ => u32::from(c),
            };
            self.append_string_point(
                value,
                Span {
                    start: span.start,
                    end: self.offset,
                },
            )?;
        }
        Err(Error::malformed(
            Span {
                start,
                end: self.offset,
            },
            if template {
                "unterminated template literal"
            } else {
                "unterminated string literal"
            },
        ))
    }

    fn consume_string(&mut self, c: char, start: usize) -> Result<Span, Error> {
        let end = self.offset + c.len_utf8();
        if end - start > MAX_STRING_LITERAL_BYTES {
            let err = Error::limit(
                Span {
                    start: self.offset,
                    end,
                },
                Limit::StringLiteralBytes,
            );
            return Err(if self.source.as_bytes()[start] == b'`' {
                Error {
                    message: "template literal byte ceiling exceeded (including backticks)",
                    ..err
                }
            } else {
                err
            });
        }
        self.consume(c)
    }

    // CRLF is one line ending, but both bytes must pass the token and work caps.
    fn finish_string_crlf(&mut self, c: char, start: usize) -> Result<(), Error> {
        if c == '\r' && self.peek() == Some('\n') {
            self.consume_string('\n', start)?;
        }
        Ok(())
    }

    fn string_escape(
        &mut self,
        start: usize,
        slash: usize,
        template: bool,
    ) -> Result<Option<u32>, Error> {
        let c = self.peek().ok_or_else(|| {
            Error::malformed(
                Span {
                    start: slash,
                    end: self.offset,
                },
                "unterminated string escape",
            )
        })?;
        self.consume_string(c, start)?;
        if is_line_terminator(c) {
            // Escaped line terminators are continuations and emit no units.
            self.finish_string_crlf(c, start)?;
            return Ok(None);
        }

        let value = match c {
            'b' => 0x08,
            'f' => 0x0C,
            'n' => 0x0A,
            'r' => 0x0D,
            't' => 0x09,
            'v' => 0x0B,
            'x' => self.string_hex_digits(start, slash, 2)?,
            'u' if self.peek() == Some('{') => self.string_braced_point(start, slash)?,
            'u' => self.string_hex_digits(start, slash, 4)?,
            '0' if !self.peek().is_some_and(|next| next.is_ascii_digit()) => 0,
            '0'..='9' => {
                if c == '0'
                    && let Some(digit) = self.peek().filter(char::is_ascii_digit)
                {
                    self.consume_string(digit, start)?;
                }
                let span = Span {
                    start: slash,
                    end: self.offset,
                };
                return Err(if template {
                    // Annex B extends quoted Strings only. Untagged templates
                    // reject these escapes rather than producing a cooked value.
                    Error::malformed(span, "invalid decimal or octal template escape")
                } else {
                    Error::unsupported(
                        span,
                        "Annex B decimal and octal string escapes are outside the subset",
                    )
                });
            }
            // This includes escaped backticks, dollars and identity escapes.
            _ => u32::from(c),
        };
        Ok(Some(value))
    }

    fn string_hex_digits(
        &mut self,
        start: usize,
        slash: usize,
        count: usize,
    ) -> Result<u32, Error> {
        let mut value = 0;
        for _ in 0..count {
            let digit = if let Some(c) = self.peek() {
                self.consume_string(c, start)?;
                c.to_digit(16)
            } else {
                None
            };
            let digit = digit.ok_or_else(|| {
                Error::malformed(
                    Span {
                        start: slash,
                        end: self.offset,
                    },
                    "expected a hexadecimal digit in string escape",
                )
            })?;
            value = value * 16 + digit;
        }
        Ok(value)
    }

    fn string_braced_point(&mut self, start: usize, slash: usize) -> Result<u32, Error> {
        self.consume_string('{', start)?;
        let digits_start = self.offset;
        let mut value = 0;
        while let Some(c) = self.peek() {
            let span = self.consume_string(c, start)?;
            if c == '}' && span.start != digits_start && value <= MAX_CODE_POINT {
                return Ok(value);
            }
            let Some(digit) = c.to_digit(16) else { break };
            // Leading zeros may be arbitrarily long within the byte ceiling.
            // Saturate without overflow but continue scanning to check the cap
            // and consume the closing brace or offending scalar for the span.
            value = (value * 16 + digit).min(MAX_CODE_POINT + 1);
        }
        Err(Error::malformed(
            Span {
                start: slash,
                end: self.offset,
            },
            "invalid braced Unicode string escape",
        ))
    }

    fn append_string_point(&mut self, value: u32, span: Span) -> Result<(), Error> {
        let (units, count) = if value <= 0xFFFF {
            // Escaped surrogates are legal and must not pass through Rust char.
            ([value as u16, 0], 1)
        } else {
            let pair = value - 0x1_0000;
            (
                [
                    (0xD800 + (pair >> 10)) as u16,
                    (0xDC00 + (pair & 0x3FF)) as u16,
                ],
                2,
            )
        };
        let len = self.string_units.len();
        let needed = len + count;
        if needed > MAX_STRING_LITERAL_UNITS {
            return Err(Error::limit(span, Limit::StringLiteralUnits));
        }
        self.charge(count, span)?;
        let cap = self.string_units.capacity();
        if needed > cap {
            // Request geometric growth but never more than the fixed unit ceiling.
            let target = (cap * 2).max(needed).min(MAX_STRING_LITERAL_UNITS);
            self.string_units
                .try_reserve_exact(target - len)
                .map_err(|_| Error::limit(span, Limit::Allocation))?;
        }
        self.string_units.extend_from_slice(&units[..count]);
        Ok(())
    }
}
