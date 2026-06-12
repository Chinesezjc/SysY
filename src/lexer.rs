use crate::error::{CompilerError, CompilerResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub position: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    KwInt,
    KwReturn,
    KwConst,
    KwIf,
    KwElse,
    KwWhile,
    KwBreak,
    KwContinue,
    KwVoid,
    LBracket,
    RBracket,
    Ident(String),
    IntLiteral(i32),
    LParen,
    RParen,
    LBrace,
    RBrace,
    Semicolon,
    Comma,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Bang,
    Less,
    Greater,
    LessEq,
    GreaterEq,
    EqEq,
    Eq,
    NotEq,
    AndAnd,
    OrOr,
    Eof,
}

pub fn tokenize(input: &str) -> CompilerResult<Vec<Token>> {
    Lexer::new(input).tokenize()
}

struct Lexer<'a> {
    input: &'a str,
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Lexer<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            bytes: input.as_bytes(),
            position: 0,
        }
    }

    fn tokenize(mut self) -> CompilerResult<Vec<Token>> {
        let mut tokens = Vec::new();
        loop {
            let token = self.next_token()?;
            let is_eof = matches!(token.kind, TokenKind::Eof);
            tokens.push(token);
            if is_eof {
                break;
            }
        }
        Ok(tokens)
    }

    fn next_token(&mut self) -> CompilerResult<Token> {
        self.skip_trivia()?;
        let position = self.position;
        let kind = match self.peek() {
            None => TokenKind::Eof,
            Some(b'(') => {
                self.bump();
                TokenKind::LParen
            }
            Some(b')') => {
                self.bump();
                TokenKind::RParen
            }
            Some(b'{') => {
                self.bump();
                TokenKind::LBrace
            }
            Some(b'}') => {
                self.bump();
                TokenKind::RBrace
            }
            Some(b'[') => {
                self.bump();
                TokenKind::LBracket
            }
            Some(b']') => {
                self.bump();
                TokenKind::RBracket
            }
            Some(b';') => {
                self.bump();
                TokenKind::Semicolon
            }
            Some(b',') => {
                self.bump();
                TokenKind::Comma
            }
            Some(b'+') => {
                self.bump();
                TokenKind::Plus
            }
            Some(b'-') => {
                self.bump();
                TokenKind::Minus
            }
            Some(b'*') => {
                self.bump();
                TokenKind::Star
            }
            Some(b'/') => {
                self.bump();
                TokenKind::Slash
            }
            Some(b'%') => {
                self.bump();
                TokenKind::Percent
            }
            Some(b'!') => {
                self.bump();
                if self.consume_if(b'=') {
                    TokenKind::NotEq
                } else {
                    TokenKind::Bang
                }
            }
            Some(b'<') => {
                self.bump();
                if self.consume_if(b'=') {
                    TokenKind::LessEq
                } else {
                    TokenKind::Less
                }
            }
            Some(b'>') => {
                self.bump();
                if self.consume_if(b'=') {
                    TokenKind::GreaterEq
                } else {
                    TokenKind::Greater
                }
            }
            Some(b'=') => {
                self.bump();
                if self.consume_if(b'=') {
                    TokenKind::EqEq
                } else {
                    TokenKind::Eq
                }
            }
            Some(b'&') => {
                self.bump();
                if self.consume_if(b'&') {
                    TokenKind::AndAnd
                } else {
                    return Err(CompilerError::at(position, "unexpected '&'"));
                }
            }
            Some(b'|') => {
                self.bump();
                if self.consume_if(b'|') {
                    TokenKind::OrOr
                } else {
                    return Err(CompilerError::at(position, "unexpected '|'"));
                }
            }
            Some(byte) if is_ident_start(byte) => self.lex_ident_or_keyword(position),
            Some(byte) if byte.is_ascii_digit() => self.lex_number(position)?,
            Some(byte) => {
                return Err(CompilerError::at(
                    position,
                    format!("unexpected character '{}'", byte as char),
                ));
            }
        };
        Ok(Token { kind, position })
    }

    fn skip_trivia(&mut self) -> CompilerResult<()> {
        loop {
            while matches!(self.peek(), Some(byte) if byte.is_ascii_whitespace()) {
                self.bump();
            }

            if self.peek() == Some(b'/') && self.peek_next() == Some(b'/') {
                self.bump();
                self.bump();
                while !matches!(self.peek(), None | Some(b'\n')) {
                    self.bump();
                }
                continue;
            }

            if self.peek() == Some(b'/') && self.peek_next() == Some(b'*') {
                let start = self.position;
                self.bump();
                self.bump();
                loop {
                    match (self.peek(), self.peek_next()) {
                        (Some(b'*'), Some(b'/')) => {
                            self.bump();
                            self.bump();
                            break;
                        }
                        (Some(_), _) => {
                            self.bump();
                        }
                        (None, _) => {
                            return Err(CompilerError::at(start, "unterminated block comment"));
                        }
                    }
                }
                continue;
            }

            break;
        }

        Ok(())
    }

    fn lex_ident_or_keyword(&mut self, start: usize) -> TokenKind {
        while matches!(self.peek(), Some(byte) if is_ident_continue(byte)) {
            self.bump();
        }

        match &self.input[start..self.position] {
            "int" => TokenKind::KwInt,
            "return" => TokenKind::KwReturn,
            "const" => TokenKind::KwConst,
            "if" => TokenKind::KwIf,
            "else" => TokenKind::KwElse,
            "while" => TokenKind::KwWhile,
            "break" => TokenKind::KwBreak,
            "continue" => TokenKind::KwContinue,
            "void" => TokenKind::KwVoid,
            ident => TokenKind::Ident(ident.to_string()),
        }
    }

    fn lex_number(&mut self, start: usize) -> CompilerResult<TokenKind> {
        if self.peek() == Some(b'0') {
            self.bump();
            match self.peek() {
                Some(b'x') | Some(b'X') => {
                    self.bump();
                    let digits_start = self.position;
                    while matches!(self.peek(), Some(byte) if byte.is_ascii_hexdigit()) {
                        self.bump();
                    }
                    if digits_start == self.position {
                        return Err(CompilerError::at(start, "expected hexadecimal digits"));
                    }
                    return self.parse_int(start, 16, &self.input[digits_start..self.position]);
                }
                Some(byte) if byte.is_ascii_digit() => {
                    while matches!(self.peek(), Some(byte) if byte.is_ascii_digit()) {
                        if !matches!(self.peek(), Some(b'0'..=b'7')) {
                            return Err(CompilerError::at(start, "invalid octal literal"));
                        }
                        self.bump();
                    }
                    return self.parse_int(start, 8, &self.input[start + 1..self.position]);
                }
                _ => return Ok(TokenKind::IntLiteral(0)),
            }
        }

        while matches!(self.peek(), Some(byte) if byte.is_ascii_digit()) {
            self.bump();
        }
        self.parse_int(start, 10, &self.input[start..self.position])
    }

    fn parse_int(&self, position: usize, radix: u32, digits: &str) -> CompilerResult<TokenKind> {
        if digits.is_empty() {
            return Ok(TokenKind::IntLiteral(0));
        }

        // Use u32 to allow hex literals like 0x80000000 (bit pattern for INT32_MIN)
        let value = u32::from_str_radix(digits, radix)
            .map_err(|_| CompilerError::at(position, "integer literal out of range"))?;
        Ok(TokenKind::IntLiteral(value as i32))
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn peek_next(&self) -> Option<u8> {
        self.bytes.get(self.position + 1).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.position += 1;
        Some(byte)
    }

    fn consume_if(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.bump();
            true
        } else {
            false
        }
    }
}

fn is_ident_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

fn is_ident_continue(byte: u8) -> bool {
    is_ident_start(byte) || byte.is_ascii_digit()
}

#[cfg(test)]
mod tests {
    use super::{TokenKind, tokenize};

    #[test]
    fn lexes_comments_and_literals() {
        let tokens = tokenize("int main(){/*x*/return 0x10 + 07 // y\n;}")
            .unwrap()
            .into_iter()
            .map(|token| token.kind)
            .collect::<Vec<_>>();

        assert_eq!(
            tokens,
            vec![
                TokenKind::KwInt,
                TokenKind::Ident("main".to_string()),
                TokenKind::LParen,
                TokenKind::RParen,
                TokenKind::LBrace,
                TokenKind::KwReturn,
                TokenKind::IntLiteral(16),
                TokenKind::Plus,
                TokenKind::IntLiteral(7),
                TokenKind::Semicolon,
                TokenKind::RBrace,
                TokenKind::Eof,
            ]
        );
    }
}
