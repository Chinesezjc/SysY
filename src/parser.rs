use crate::ast::{
    BinaryOp, Block, BlockItem, CompUnit, ConstDef, Decl, Expr, FuncDef, Stmt, UnaryOp, VarDef,
};
use crate::error::{CompilerError, CompilerResult};
use crate::lexer::{Token, TokenKind};

pub fn parse(tokens: Vec<Token>) -> CompilerResult<CompUnit> {
    Parser::new(tokens).parse_comp_unit()
}

struct Parser {
    tokens: Vec<Token>,
    index: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, index: 0 }
    }

    fn parse_comp_unit(mut self) -> CompilerResult<CompUnit> {
        let func = self.parse_func_def()?;
        self.expect_eof()?;
        Ok(CompUnit { func })
    }

    fn parse_func_def(&mut self) -> CompilerResult<FuncDef> {
        self.expect_keyword_int()?;
        let name = self.expect_ident()?;
        self.expect_l_paren()?;
        self.expect_r_paren()?;
        let body = self.parse_block()?;
        Ok(FuncDef { name, body })
    }

    fn parse_block(&mut self) -> CompilerResult<Block> {
        self.expect_l_brace()?;
        let mut items = Vec::new();
        while !matches!(self.current_kind(), TokenKind::RBrace | TokenKind::Eof) {
            items.push(self.parse_block_item()?);
        }
        self.expect_r_brace()?;
        Ok(Block { items })
    }

    fn parse_block_item(&mut self) -> CompilerResult<BlockItem> {
        match self.current_kind() {
            TokenKind::KwConst => Ok(BlockItem::Decl(self.parse_const_decl()?)),
            TokenKind::KwInt => Ok(BlockItem::Decl(self.parse_var_decl()?)),
            _ => Ok(BlockItem::Stmt(self.parse_stmt()?)),
        }
    }

    fn parse_const_decl(&mut self) -> CompilerResult<Decl> {
        self.advance(); // consume "const"
        self.expect_keyword_int()?;
        let mut defs = vec![self.parse_const_def()?];
        while matches!(self.current_kind(), TokenKind::Comma) {
            self.advance();
            defs.push(self.parse_const_def()?);
        }
        self.expect_semicolon()?;
        Ok(Decl::Const(defs))
    }

    fn parse_const_def(&mut self) -> CompilerResult<ConstDef> {
        let name = self.expect_ident()?;
        self.expect_eq()?;
        let init = self.parse_expr()?;
        Ok(ConstDef { name, init })
    }

    fn parse_var_decl(&mut self) -> CompilerResult<Decl> {
        self.advance(); // consume "int"
        let mut defs = vec![self.parse_var_def()?];
        while matches!(self.current_kind(), TokenKind::Comma) {
            self.advance();
            defs.push(self.parse_var_def()?);
        }
        self.expect_semicolon()?;
        Ok(Decl::Var(defs))
    }

    fn parse_var_def(&mut self) -> CompilerResult<VarDef> {
        let name = self.expect_ident()?;
        let init = if matches!(self.current_kind(), TokenKind::Eq) {
            self.advance();
            Some(self.parse_expr()?)
        } else {
            None
        };
        Ok(VarDef { name, init })
    }

    fn parse_stmt(&mut self) -> CompilerResult<Stmt> {
        match self.current_kind() {
            TokenKind::KwReturn => {
                self.advance();
                let expr = self.parse_expr()?;
                self.expect_semicolon()?;
                Ok(Stmt::Return(expr))
            }
            TokenKind::Ident(_)
                if matches!(self.peek_next_kind(), TokenKind::Eq) =>
            {
                let name = self.expect_ident()?;
                self.advance(); // consume "="
                let expr = self.parse_expr()?;
                self.expect_semicolon()?;
                Ok(Stmt::Assign { name, expr })
            }
            _ => Err(CompilerError::at(
                self.current().position,
                "expected statement",
            )),
        }
    }

    fn parse_expr(&mut self) -> CompilerResult<Expr> {
        self.parse_lor_expr()
    }

    fn parse_lor_expr(&mut self) -> CompilerResult<Expr> {
        let mut expr = self.parse_land_expr()?;
        while matches!(self.current_kind(), TokenKind::OrOr) {
            self.advance();
            let rhs = self.parse_land_expr()?;
            expr = Expr::Binary {
                op: BinaryOp::Or,
                lhs: Box::new(expr),
                rhs: Box::new(rhs),
            };
        }
        Ok(expr)
    }

    fn parse_land_expr(&mut self) -> CompilerResult<Expr> {
        let mut expr = self.parse_eq_expr()?;
        while matches!(self.current_kind(), TokenKind::AndAnd) {
            self.advance();
            let rhs = self.parse_eq_expr()?;
            expr = Expr::Binary {
                op: BinaryOp::And,
                lhs: Box::new(expr),
                rhs: Box::new(rhs),
            };
        }
        Ok(expr)
    }

    fn parse_eq_expr(&mut self) -> CompilerResult<Expr> {
        let mut expr = self.parse_rel_expr()?;
        loop {
            let op = match self.current_kind() {
                TokenKind::EqEq => BinaryOp::Eq,
                TokenKind::NotEq => BinaryOp::Ne,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_rel_expr()?;
            expr = Expr::Binary {
                op,
                lhs: Box::new(expr),
                rhs: Box::new(rhs),
            };
        }
        Ok(expr)
    }

    fn parse_rel_expr(&mut self) -> CompilerResult<Expr> {
        let mut expr = self.parse_add_expr()?;
        loop {
            let op = match self.current_kind() {
                TokenKind::Less => BinaryOp::Lt,
                TokenKind::Greater => BinaryOp::Gt,
                TokenKind::LessEq => BinaryOp::Le,
                TokenKind::GreaterEq => BinaryOp::Ge,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_add_expr()?;
            expr = Expr::Binary {
                op,
                lhs: Box::new(expr),
                rhs: Box::new(rhs),
            };
        }
        Ok(expr)
    }

    fn parse_add_expr(&mut self) -> CompilerResult<Expr> {
        let mut expr = self.parse_mul_expr()?;
        loop {
            let op = match self.current_kind() {
                TokenKind::Plus => BinaryOp::Add,
                TokenKind::Minus => BinaryOp::Sub,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_mul_expr()?;
            expr = Expr::Binary {
                op,
                lhs: Box::new(expr),
                rhs: Box::new(rhs),
            };
        }
        Ok(expr)
    }

    fn parse_mul_expr(&mut self) -> CompilerResult<Expr> {
        let mut expr = self.parse_unary_expr()?;
        loop {
            let op = match self.current_kind() {
                TokenKind::Star => BinaryOp::Mul,
                TokenKind::Slash => BinaryOp::Div,
                TokenKind::Percent => BinaryOp::Rem,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_unary_expr()?;
            expr = Expr::Binary {
                op,
                lhs: Box::new(expr),
                rhs: Box::new(rhs),
            };
        }
        Ok(expr)
    }

    fn parse_unary_expr(&mut self) -> CompilerResult<Expr> {
        match self.current_kind() {
            TokenKind::Plus => {
                self.advance();
                Ok(Expr::Unary {
                    op: UnaryOp::Plus,
                    expr: Box::new(self.parse_unary_expr()?),
                })
            }
            TokenKind::Minus => {
                self.advance();
                Ok(Expr::Unary {
                    op: UnaryOp::Minus,
                    expr: Box::new(self.parse_unary_expr()?),
                })
            }
            TokenKind::Bang => {
                self.advance();
                Ok(Expr::Unary {
                    op: UnaryOp::Not,
                    expr: Box::new(self.parse_unary_expr()?),
                })
            }
            _ => self.parse_primary_expr(),
        }
    }

    fn parse_primary_expr(&mut self) -> CompilerResult<Expr> {
        match self.current_kind() {
            TokenKind::LParen => {
                self.advance();
                let expr = self.parse_expr()?;
                self.expect_r_paren()?;
                Ok(expr)
            }
            TokenKind::IntLiteral(value) => {
                let value = *value;
                self.advance();
                Ok(Expr::Int(value))
            }
            TokenKind::Ident(name) => {
                let name = name.clone();
                self.advance();
                Ok(Expr::LVal(name))
            }
            _ => Err(CompilerError::at(
                self.current().position,
                "expected primary expression",
            )),
        }
    }

    fn expect_keyword_int(&mut self) -> CompilerResult<()> {
        self.expect_simple(TokenKind::KwInt, "expected 'int'")
    }

    fn expect_l_paren(&mut self) -> CompilerResult<()> {
        self.expect_simple(TokenKind::LParen, "expected '('")
    }

    fn expect_r_paren(&mut self) -> CompilerResult<()> {
        self.expect_simple(TokenKind::RParen, "expected ')'")
    }

    fn expect_l_brace(&mut self) -> CompilerResult<()> {
        self.expect_simple(TokenKind::LBrace, "expected '{'")
    }

    fn expect_r_brace(&mut self) -> CompilerResult<()> {
        self.expect_simple(TokenKind::RBrace, "expected '}'")
    }

    fn expect_semicolon(&mut self) -> CompilerResult<()> {
        self.expect_simple(TokenKind::Semicolon, "expected ';'")
    }

    fn expect_eq(&mut self) -> CompilerResult<()> {
        self.expect_simple(TokenKind::Eq, "expected '='")
    }

    fn expect_ident(&mut self) -> CompilerResult<String> {
        match self.current_kind() {
            TokenKind::Ident(ident) => {
                let ident = ident.clone();
                self.advance();
                Ok(ident)
            }
            _ => Err(CompilerError::at(
                self.current().position,
                "expected identifier",
            )),
        }
    }

    fn expect_eof(&self) -> CompilerResult<()> {
        if matches!(self.current_kind(), TokenKind::Eof) {
            Ok(())
        } else {
            Err(CompilerError::at(
                self.current().position,
                "unexpected trailing tokens",
            ))
        }
    }

    fn expect_simple(&mut self, expected: TokenKind, message: &str) -> CompilerResult<()> {
        if self.current_kind() == &expected {
            self.advance();
            Ok(())
        } else {
            Err(CompilerError::at(self.current().position, message))
        }
    }

    fn current(&self) -> &Token {
        &self.tokens[self.index]
    }

    fn current_kind(&self) -> &TokenKind {
        &self.current().kind
    }

    fn peek_next_kind(&self) -> &TokenKind {
        let next = (self.index + 1).min(self.tokens.len() - 1);
        &self.tokens[next].kind
    }

    fn advance(&mut self) {
        if self.index + 1 < self.tokens.len() {
            self.index += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse;
    use crate::lexer::tokenize;

    #[test]
    fn parses_expression_precedence() {
        let tokens = tokenize("int main() { return 1 + 2 * 3 == 7 || 0; }").unwrap();
        let program = parse(tokens).unwrap();
        assert_eq!(program.func.body.items.len(), 1);
    }

    #[test]
    fn parses_const_and_var() {
        let src = "int main() { const int x = 2; int y = x + 1; y = y - 1; return y; }";
        let tokens = tokenize(src).unwrap();
        let program = parse(tokens).unwrap();
        assert_eq!(program.func.body.items.len(), 4);
    }
}
