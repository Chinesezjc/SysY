#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompUnit {
    pub items: Vec<GlobalItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlobalItem {
    FuncDef(FuncDef),
    FuncDecl(FuncDecl),
    Decl(Decl),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncDecl {
    pub ret_type: Type,
    pub name: String,
    pub params: Vec<FuncParam>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncDef {
    pub ret_type: Type,
    pub name: String,
    pub params: Vec<FuncParam>,
    pub body: Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    Int,
    Void,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncParam {
    pub name: String,
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
    pub dims: Vec<Expr>,
    pub init: Expr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarDef {
    pub name: String,
    pub dims: Vec<Expr>,
    pub init: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    Return(Option<Expr>),
    Assign { name: String, index: Vec<Expr>, expr: Expr },
    Expr(Expr),
    Block(Block),
    If {
        cond: Expr,
        then_branch: Box<Stmt>,
        else_branch: Option<Box<Stmt>>,
    },
    While {
        cond: Expr,
        body: Box<Stmt>,
    },
    Break,
    Continue,
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
    Call {
        name: String,
        args: Vec<Expr>,
    },
    Index {
        array: Box<Expr>,
        index: Box<Expr>,
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
