use std::collections::HashMap;

use crate::error::{CompilerError, CompilerResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompUnit {
    pub func: FuncDef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncDef {
    pub name: String,
    pub body: Block,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub items: Vec<BlockItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockItem {
    Decl(Decl),
    Stmt(Stmt),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decl {
    Const(Vec<ConstDef>),
    Var(Vec<VarDef>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstDef {
    pub name: String,
    pub init: Expr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarDef {
    pub name: String,
    pub init: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    Return(Expr),
    Assign { name: String, expr: Expr },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Int(i32),
    LVal(String),
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Plus,
    Minus,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Mul,
    Div,
    Rem,
    Add,
    Sub,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

impl Expr {
    pub fn eval(&self, consts: &HashMap<String, i32>) -> CompilerResult<i32> {
        match self {
            Expr::Int(value) => Ok(*value),
            Expr::LVal(name) => consts
                .get(name)
                .copied()
                .ok_or_else(|| CompilerError::new(format!("'{name}' is not a compile-time constant"))),
            Expr::Unary { op, expr } => {
                let value = expr.eval(consts)?;
                match op {
                    UnaryOp::Plus => Ok(value),
                    UnaryOp::Minus => Ok(value.wrapping_neg()),
                    UnaryOp::Not => Ok((value == 0) as i32),
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                let lhs = lhs.eval(consts)?;
                let rhs = rhs.eval(consts)?;
                match op {
                    BinaryOp::Mul => Ok(lhs.wrapping_mul(rhs)),
                    BinaryOp::Div => lhs
                        .checked_div(rhs)
                        .ok_or_else(|| CompilerError::new("invalid constant division")),
                    BinaryOp::Rem => lhs
                        .checked_rem(rhs)
                        .ok_or_else(|| CompilerError::new("invalid constant remainder")),
                    BinaryOp::Add => Ok(lhs.wrapping_add(rhs)),
                    BinaryOp::Sub => Ok(lhs.wrapping_sub(rhs)),
                    BinaryOp::Lt => Ok((lhs < rhs) as i32),
                    BinaryOp::Gt => Ok((lhs > rhs) as i32),
                    BinaryOp::Le => Ok((lhs <= rhs) as i32),
                    BinaryOp::Ge => Ok((lhs >= rhs) as i32),
                    BinaryOp::Eq => Ok((lhs == rhs) as i32),
                    BinaryOp::Ne => Ok((lhs != rhs) as i32),
                    BinaryOp::And => Ok((lhs != 0 && rhs != 0) as i32),
                    BinaryOp::Or => Ok((lhs != 0 || rhs != 0) as i32),
                }
            }
        }
    }
}
