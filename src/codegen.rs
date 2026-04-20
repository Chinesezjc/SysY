use std::collections::{HashMap, HashSet};

use crate::OutputMode;
use crate::ast::{BinaryOp, Block, BlockItem, CompUnit, Decl, Expr, Stmt, UnaryOp};
use crate::error::{CompilerError, CompilerResult};

pub fn generate(program: &CompUnit, mode: OutputMode) -> CompilerResult<String> {
    match mode {
        OutputMode::Koopa => KoopaGen::new().gen_program(program),
        OutputMode::Riscv => RiscvGen::new().gen_program(program),
    }
}

// ── Koopa IR ────────────────────────────────────────────────────────────────

struct KoopaGen {
    consts: HashMap<String, i32>,
    vars: HashSet<String>,
    tmp: usize,
    body: String,
}

impl KoopaGen {
    fn new() -> Self {
        Self {
            consts: HashMap::new(),
            vars: HashSet::new(),
            tmp: 0,
            body: String::new(),
        }
    }

    fn alloc_tmp(&mut self) -> String {
        let t = format!("%{}", self.tmp);
        self.tmp += 1;
        t
    }

    fn emit(&mut self, s: &str) {
        self.body.push_str("  ");
        self.body.push_str(s);
        self.body.push('\n');
    }

    fn gen_program(mut self, program: &CompUnit) -> CompilerResult<String> {
        self.gen_block(&program.func.body)?;
        let name = &program.func.name;
        Ok(format!("fun @{name}(): i32 {{\n%entry:\n{}}}\n", self.body))
    }

    fn gen_block(&mut self, block: &Block) -> CompilerResult<()> {
        for item in &block.items {
            match item {
                BlockItem::Decl(d) => self.gen_decl(d)?,
                BlockItem::Stmt(s) => self.gen_stmt(s)?,
            }
        }
        Ok(())
    }

    fn gen_decl(&mut self, decl: &Decl) -> CompilerResult<()> {
        match decl {
            Decl::Const(defs) => {
                for def in defs {
                    if self.consts.contains_key(&def.name) || self.vars.contains(&def.name) {
                        return Err(CompilerError::new(format!(
                            "redeclaration of '{}'",
                            def.name
                        )));
                    }
                    let val = def.init.eval(&self.consts)?;
                    self.consts.insert(def.name.clone(), val);
                }
            }
            Decl::Var(defs) => {
                for def in defs {
                    if self.consts.contains_key(&def.name) || self.vars.contains(&def.name) {
                        return Err(CompilerError::new(format!(
                            "redeclaration of '{}'",
                            def.name
                        )));
                    }
                    self.vars.insert(def.name.clone());
                    self.emit(&format!("@{} = alloca i32", def.name));
                    if let Some(init) = &def.init {
                        let val = self.gen_expr(init)?;
                        self.emit(&format!("store {val}, @{}", def.name));
                    }
                }
            }
        }
        Ok(())
    }

    fn gen_stmt(&mut self, stmt: &Stmt) -> CompilerResult<()> {
        match stmt {
            Stmt::Return(expr) => {
                let val = self.gen_expr(expr)?;
                self.emit(&format!("ret {val}"));
            }
            Stmt::Assign { name, expr } => {
                if self.consts.contains_key(name) {
                    return Err(CompilerError::new(format!(
                        "cannot assign to constant '{name}'"
                    )));
                }
                if !self.vars.contains(name) {
                    return Err(CompilerError::new(format!("undefined variable '{name}'")));
                }
                let val = self.gen_expr(expr)?;
                self.emit(&format!("store {val}, @{name}"));
            }
        }
        Ok(())
    }

    fn gen_expr(&mut self, expr: &Expr) -> CompilerResult<String> {
        match expr {
            Expr::Int(n) => Ok(n.to_string()),
            Expr::LVal(name) => {
                if let Some(&v) = self.consts.get(name) {
                    return Ok(v.to_string());
                }
                if self.vars.contains(name) {
                    let tmp = self.alloc_tmp();
                    self.emit(&format!("{tmp} = load @{name}"));
                    return Ok(tmp);
                }
                Err(CompilerError::new(format!("undefined identifier '{name}'")))
            }
            Expr::Unary { op, expr } => {
                let val = self.gen_expr(expr)?;
                match op {
                    UnaryOp::Plus => Ok(val),
                    UnaryOp::Minus => {
                        let tmp = self.alloc_tmp();
                        self.emit(&format!("{tmp} = sub 0, {val}"));
                        Ok(tmp)
                    }
                    UnaryOp::Not => {
                        let tmp = self.alloc_tmp();
                        self.emit(&format!("{tmp} = eq {val}, 0"));
                        Ok(tmp)
                    }
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                let lv = self.gen_expr(lhs)?;
                let rv = self.gen_expr(rhs)?;
                match op {
                    BinaryOp::And => {
                        let t0 = self.alloc_tmp();
                        let t1 = self.alloc_tmp();
                        let t2 = self.alloc_tmp();
                        self.emit(&format!("{t0} = ne {lv}, 0"));
                        self.emit(&format!("{t1} = ne {rv}, 0"));
                        self.emit(&format!("{t2} = and {t0}, {t1}"));
                        Ok(t2)
                    }
                    BinaryOp::Or => {
                        let t0 = self.alloc_tmp();
                        let t1 = self.alloc_tmp();
                        let t2 = self.alloc_tmp();
                        self.emit(&format!("{t0} = ne {lv}, 0"));
                        self.emit(&format!("{t1} = ne {rv}, 0"));
                        self.emit(&format!("{t2} = or {t0}, {t1}"));
                        Ok(t2)
                    }
                    _ => {
                        let op_str = match op {
                            BinaryOp::Mul => "mul",
                            BinaryOp::Div => "div",
                            BinaryOp::Rem => "mod",
                            BinaryOp::Add => "add",
                            BinaryOp::Sub => "sub",
                            BinaryOp::Lt => "lt",
                            BinaryOp::Gt => "gt",
                            BinaryOp::Le => "le",
                            BinaryOp::Ge => "ge",
                            BinaryOp::Eq => "eq",
                            BinaryOp::Ne => "ne",
                            BinaryOp::And | BinaryOp::Or => unreachable!(),
                        };
                        let tmp = self.alloc_tmp();
                        self.emit(&format!("{tmp} = {op_str} {lv}, {rv}"));
                        Ok(tmp)
                    }
                }
            }
        }
    }
}

// ── RISC-V ──────────────────────────────────────────────────────────────────

struct RiscvGen {
    consts: HashMap<String, i32>,
    var_offsets: HashMap<String, i32>,
    frame_size: i32,
    extra_sp: i32,
    out: String,
}

impl RiscvGen {
    fn new() -> Self {
        Self {
            consts: HashMap::new(),
            var_offsets: HashMap::new(),
            frame_size: 0,
            extra_sp: 0,
            out: String::new(),
        }
    }

    fn emit(&mut self, s: &str) {
        self.out.push_str("  ");
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn gen_program(mut self, program: &CompUnit) -> CompilerResult<String> {
        // First pass: collect variable declarations and assign stack offsets.
        let mut slot = 0i32;
        for item in &program.func.body.items {
            if let BlockItem::Decl(Decl::Var(defs)) = item {
                for def in defs {
                    self.var_offsets.insert(def.name.clone(), slot * 4);
                    slot += 1;
                }
            }
        }
        let num_vars = slot;
        self.frame_size = align16(num_vars * 4);

        // Second pass: generate code.
        let name = &program.func.name;
        self.out.push_str(&format!("  .text\n  .globl {name}\n{name}:\n"));
        if self.frame_size > 0 {
            self.emit(&format!("addi sp, sp, -{}", self.frame_size));
        }
        self.gen_block(&program.func.body)?;
        Ok(self.out)
    }

    fn gen_block(&mut self, block: &Block) -> CompilerResult<()> {
        for item in &block.items {
            match item {
                BlockItem::Decl(d) => self.gen_decl(d)?,
                BlockItem::Stmt(s) => self.gen_stmt(s)?,
            }
        }
        Ok(())
    }

    fn gen_decl(&mut self, decl: &Decl) -> CompilerResult<()> {
        match decl {
            Decl::Const(defs) => {
                for def in defs {
                    let val = def.init.eval(&self.consts)?;
                    self.consts.insert(def.name.clone(), val);
                }
            }
            Decl::Var(defs) => {
                for def in defs {
                    if let Some(init) = &def.init {
                        self.gen_expr(init)?;
                        let offset = self.var_offsets[&def.name] + self.extra_sp;
                        self.emit(&format!("sw a0, {offset}(sp)"));
                    }
                }
            }
        }
        Ok(())
    }

    fn gen_stmt(&mut self, stmt: &Stmt) -> CompilerResult<()> {
        match stmt {
            Stmt::Return(expr) => {
                self.gen_expr(expr)?;
                if self.frame_size > 0 {
                    self.emit(&format!("addi sp, sp, {}", self.frame_size));
                }
                self.emit("ret");
            }
            Stmt::Assign { name, expr } => {
                if self.consts.contains_key(name) {
                    return Err(CompilerError::new(format!(
                        "cannot assign to constant '{name}'"
                    )));
                }
                self.gen_expr(expr)?;
                let offset = self.var_offsets[name] + self.extra_sp;
                self.emit(&format!("sw a0, {offset}(sp)"));
            }
        }
        Ok(())
    }

    fn gen_expr(&mut self, expr: &Expr) -> CompilerResult<()> {
        match expr {
            Expr::Int(n) => {
                self.emit(&format!("li a0, {n}"));
            }
            Expr::LVal(name) => {
                if let Some(&v) = self.consts.get(name) {
                    self.emit(&format!("li a0, {v}"));
                } else if let Some(&base_offset) = self.var_offsets.get(name) {
                    let offset = base_offset + self.extra_sp;
                    self.emit(&format!("lw a0, {offset}(sp)"));
                } else {
                    return Err(CompilerError::new(format!(
                        "undefined identifier '{name}'"
                    )));
                }
            }
            Expr::Unary { op, expr } => {
                self.gen_expr(expr)?;
                match op {
                    UnaryOp::Plus => {}
                    UnaryOp::Minus => self.emit("neg a0, a0"),
                    UnaryOp::Not => self.emit("seqz a0, a0"),
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                self.gen_expr(lhs)?;
                // Push lhs result.
                self.emit("addi sp, sp, -4");
                self.emit("sw a0, 0(sp)");
                self.extra_sp += 4;
                self.gen_expr(rhs)?;
                // Pop lhs to t0.
                self.emit("lw t0, 0(sp)");
                self.emit("addi sp, sp, 4");
                self.extra_sp -= 4;
                // t0 = lhs, a0 = rhs → result in a0
                match op {
                    BinaryOp::Add => self.emit("add a0, t0, a0"),
                    BinaryOp::Sub => self.emit("sub a0, t0, a0"),
                    BinaryOp::Mul => self.emit("mul a0, t0, a0"),
                    BinaryOp::Div => self.emit("div a0, t0, a0"),
                    BinaryOp::Rem => self.emit("rem a0, t0, a0"),
                    BinaryOp::Lt => self.emit("slt a0, t0, a0"),
                    BinaryOp::Gt => self.emit("sgt a0, t0, a0"),
                    BinaryOp::Le => {
                        self.emit("sgt a0, t0, a0");
                        self.emit("xori a0, a0, 1");
                    }
                    BinaryOp::Ge => {
                        self.emit("slt a0, t0, a0");
                        self.emit("xori a0, a0, 1");
                    }
                    BinaryOp::Eq => {
                        self.emit("sub a0, t0, a0");
                        self.emit("seqz a0, a0");
                    }
                    BinaryOp::Ne => {
                        self.emit("sub a0, t0, a0");
                        self.emit("snez a0, a0");
                    }
                    BinaryOp::And => {
                        self.emit("snez t0, t0");
                        self.emit("snez a0, a0");
                        self.emit("and a0, t0, a0");
                    }
                    BinaryOp::Or => {
                        self.emit("or a0, t0, a0");
                        self.emit("snez a0, a0");
                    }
                }
            }
        }
        Ok(())
    }
}

fn align16(n: i32) -> i32 {
    (n + 15) & !15
}
