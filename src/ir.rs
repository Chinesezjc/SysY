//! In-memory intermediate representation (Koopa IR derived).
//!
//! Uses integer indices for operands and blocks — the string names live in
//! central tables (`IrProgram::local_names`, `block_names`, `func_names`, …)
//! so that `IrOperand` is `Copy` and cloning is cheap.

use std::collections::HashMap;
use std::fmt;

// ── Types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IrType {
    I32,
    Void,
    Ptr(Box<IrType>),
    Array(Box<IrType>, u32), // element type, length
}

impl fmt::Display for IrType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IrType::I32 => write!(f, "i32"),
            IrType::Void => write!(f, "void"),
            IrType::Ptr(t) => write!(f, "*{t}"),
            IrType::Array(t, n) => write!(f, "[{t}, {n}]"),
        }
    }
}

// ── Operands ─────────────────────────────────────────────────────────────────

/// An SSA value / constant / global reference.
///
/// `Local` and `Global` carry an index into `IrProgram::local_names` /
/// `global_names` respectively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IrOperand {
    Int(i32),
    Local(usize),
    Global(usize),
    Undef,
}

impl IrOperand {
    /// Display helper that takes the name tables from `IrProgram`.
    pub fn display(
        self,
        locals: &[String],
        globals: &[String],
    ) -> String {
        match self {
            IrOperand::Int(n) => n.to_string(),
            IrOperand::Local(i) => locals.get(i).cloned().unwrap_or_else(|| format!("%?{i}")),
            IrOperand::Global(i) => globals.get(i).cloned().unwrap_or_else(|| format!("@?{i}")),
            IrOperand::Undef => "undef".to_string(),
        }
    }
}

// ── Instructions ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IrArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

impl fmt::Display for IrArithOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IrArithOp::Add => write!(f, "add"),
            IrArithOp::Sub => write!(f, "sub"),
            IrArithOp::Mul => write!(f, "mul"),
            IrArithOp::Div => write!(f, "div"),
            IrArithOp::Mod => write!(f, "mod"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IrCmpOp {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

impl fmt::Display for IrCmpOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IrCmpOp::Eq => write!(f, "eq"),
            IrCmpOp::Ne => write!(f, "ne"),
            IrCmpOp::Lt => write!(f, "lt"),
            IrCmpOp::Gt => write!(f, "gt"),
            IrCmpOp::Le => write!(f, "le"),
            IrCmpOp::Ge => write!(f, "ge"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum IrInst {
    /// `dest = alloc ty`
    Alloc {
        dest: usize, // local index
        ty: IrType,
    },

    /// `dest = load src`
    Load {
        dest: usize,
        src: IrOperand,
    },

    /// `store value, ptr`
    Store {
        value: IrOperand,
        ptr: IrOperand,
    },

    /// `dest = op lhs, rhs`
    Arith {
        dest: usize,
        op: IrArithOp,
        lhs: IrOperand,
        rhs: IrOperand,
    },

    /// `dest = op lhs, rhs`
    Icmp {
        dest: usize,
        op: IrCmpOp,
        lhs: IrOperand,
        rhs: IrOperand,
    },

    /// `dest = getptr ptr, index`  — pointer arithmetic
    GetPtr {
        dest: usize,
        ptr: IrOperand,
        index: IrOperand,
    },

    /// `dest = getelemptr ptr, index`  — element pointer from array
    GetElemPtr {
        dest: usize,
        ptr: IrOperand,
        index: IrOperand,
    },

    /// `dest = call @func(args)`
    Call {
        dest: Option<usize>, // None for void calls
        func: usize,         // func_names index
        args: Vec<IrOperand>,
    },

    /// `br cond, then_bb, else_bb`  (terminator)
    Br {
        cond: IrOperand,
        then_bb: usize, // block_names index
        else_bb: usize,
    },

    /// `jump target`  (terminator)
    Jump {
        target: usize, // block_names index
    },

    /// `ret` or `ret value`  (terminator)
    Ret {
        value: Option<IrOperand>,
    },

    /// `dest = phi [(val0, bb0), (val1, bb1), ...]`
    Phi {
        dest: usize,
        incoming: Vec<(IrOperand, usize)>, // (value, block index)
    },

    /// Inline assembly — raw string embedded verbatim in RISC-V output.
    Asm(String),
}

impl IrInst {
    /// Returns `true` if this instruction is a block terminator.
    pub fn is_terminator(&self) -> bool {
        matches!(self, IrInst::Br { .. } | IrInst::Jump { .. } | IrInst::Ret { .. })
    }

    /// Returns the destination local index, if any.
    pub fn dest(&self) -> Option<usize> {
        match self {
            IrInst::Alloc { dest, .. }
            | IrInst::Load { dest, .. }
            | IrInst::Arith { dest, .. }
            | IrInst::Icmp { dest, .. }
            | IrInst::GetPtr { dest, .. }
            | IrInst::GetElemPtr { dest, .. }
            | IrInst::Phi { dest, .. } => Some(*dest),
            IrInst::Call { dest, .. } => *dest,
            IrInst::Store { .. }
            | IrInst::Br { .. }
            | IrInst::Jump { .. }
            | IrInst::Ret { .. }
            | IrInst::Asm(_) => None,
        }
    }

    /// Returns mutable reference to destination local index, if any.
    pub fn dest_mut(&mut self) -> Option<&mut usize> {
        match self {
            IrInst::Alloc { dest, .. }
            | IrInst::Load { dest, .. }
            | IrInst::Arith { dest, .. }
            | IrInst::Icmp { dest, .. }
            | IrInst::GetPtr { dest, .. }
            | IrInst::GetElemPtr { dest, .. }
            | IrInst::Phi { dest, .. } => Some(dest),
            IrInst::Call { dest, .. } => dest.as_mut(),
            IrInst::Store { .. }
            | IrInst::Br { .. }
            | IrInst::Jump { .. }
            | IrInst::Ret { .. }
            | IrInst::Asm(_) => None,
        }
    }

    /// Returns all operand references (for use-def chains).
    pub fn operands(&self) -> Vec<&IrOperand> {
        match self {
            IrInst::Load { src, .. } => vec![src],
            IrInst::Store { value, ptr } => vec![value, ptr],
            IrInst::Arith { lhs, rhs, .. } => vec![lhs, rhs],
            IrInst::Icmp { lhs, rhs, .. } => vec![lhs, rhs],
            IrInst::GetPtr { ptr, index, .. } => vec![ptr, index],
            IrInst::GetElemPtr { ptr, index, .. } => vec![ptr, index],
            IrInst::Call { args, .. } => args.iter().collect(),
            IrInst::Br { cond, .. } => vec![cond],
            IrInst::Ret { value } => value.iter().collect(),
            IrInst::Phi { incoming, .. } => incoming.iter().map(|(v, _)| v).collect(),
            IrInst::Alloc { .. } | IrInst::Jump { .. } | IrInst::Asm(_) => vec![],
        }
    }
}

// ── Blocks ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct IrBlock {
    /// Index into `IrProgram::block_names`.
    pub label: usize,
    /// Instructions; the last must be a terminator (Br / Jump / Ret).
    pub instrs: Vec<IrInst>,
    /// Predecessor block indices (populated by CFG pass).
    pub preds: Vec<usize>,
}

// ── Functions ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct IrFunc {
    /// Index into `IrProgram::func_names`.
    pub name: usize,
    /// Parameter (name, type). Names are local indices.
    pub params: Vec<(usize, IrType)>,
    pub ret_type: IrType,
    /// Entry-block allocas: `@name = alloc ty`.  First element of each tuple is
    /// a *global* index (the alloca has a `@name` in Koopa IR).
    pub allocas: Vec<(usize, IrType)>,
    /// Basic blocks.  `blocks[0]` is the entry block.
    pub blocks: Vec<IrBlock>,
}

// ── Declarations ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct IrFuncDecl {
    pub name: usize,
    pub param_types: Vec<IrType>,
    pub ret_type: IrType,
}

// ── Globals ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum IrGlobalInit {
    Zero,
    Values(Vec<i32>),
}

#[derive(Debug, Clone)]
pub struct IrGlobal {
    pub name: usize,
    pub ty: IrType,
    pub init: IrGlobalInit,
}

// ── Program ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct IrProgram {
    // ── String tables ──
    /// `%0`, `%1`, … — SSA temporaries / local variables.
    pub local_names: Vec<String>,
    /// `@arr`, `@size`, … — global variable names.
    pub global_names: Vec<String>,
    /// `%entry`, `%label_0`, … — basic block labels.
    pub block_names: Vec<String>,
    /// `@main`, `@getint`, … — function names.
    pub func_names: Vec<String>,

    // ── Top-level items ──
    pub globals: Vec<IrGlobal>,
    pub func_decls: Vec<IrFuncDecl>,
    pub funcs: Vec<IrFunc>,
}

impl IrProgram {
    pub fn new() -> Self {
        IrProgram {
            local_names: Vec::new(),
            global_names: Vec::new(),
            block_names: Vec::new(),
            func_names: Vec::new(),
            globals: Vec::new(),
            func_decls: Vec::new(),
            funcs: Vec::new(),
        }
    }

    // ── Name interning ───────────────────────────────────────────────────────

    pub fn intern_local(&mut self, name: String) -> usize {
        let idx = self.local_names.len();
        self.local_names.push(name);
        idx
    }

    pub fn intern_global(&mut self, name: String) -> usize {
        let name = if name.starts_with('@') { name } else { format!("@{name}") };
        if let Some(pos) = self.global_names.iter().position(|n| n == &name) {
            return pos;
        }
        let idx = self.global_names.len();
        self.global_names.push(name);
        idx
    }

    pub fn intern_block(&mut self, name: String) -> usize {
        let idx = self.block_names.len();
        self.block_names.push(name);
        idx
    }

    pub fn intern_func(&mut self, name: String) -> usize {
        let idx = self.func_names.len();
        self.func_names.push(name);
        idx
    }

    // ── Queries ──────────────────────────────────────────────────────────────

    pub fn local_name(&self, idx: usize) -> &str {
        &self.local_names[idx]
    }

    pub fn global_name(&self, idx: usize) -> &str {
        &self.global_names[idx]
    }

    pub fn block_name(&self, idx: usize) -> &str {
        &self.block_names[idx]
    }

    pub fn func_name(&self, idx: usize) -> &str {
        &self.func_names[idx]
    }

    pub fn find_func(&self, name_idx: usize) -> Option<&IrFunc> {
        self.funcs.iter().find(|f| f.name == name_idx)
    }

    pub fn find_func_mut(&mut self, name_idx: usize) -> Option<&mut IrFunc> {
        self.funcs.iter_mut().find(|f| f.name == name_idx)
    }

    /// Look up a global index by name string (for interning from AST).
    pub fn global_by_name(&self, name: &str) -> Option<usize> {
        self.global_names.iter().position(|n| n == name)
    }

    /// Look up a func index by name string.
    pub fn func_by_name(&self, name: &str) -> Option<usize> {
        self.func_names.iter().position(|n| n == name)
    }
}

impl Default for IrProgram {
    fn default() -> Self {
        Self::new()
    }
}
