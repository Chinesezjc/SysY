use std::collections::{HashMap, HashSet};

use crate::OutputMode;
use crate::ast::{BinaryOp, Block, BlockItem, CompUnit, Decl, Expr, GlobalItem, Stmt, Type, UnaryOp};
use crate::error::{CompilerError, CompilerResult};

const LIB_FUNCS: &[(&str, Type, &[Type])] = &[
    ("getint", Type::Int, &[]),
    ("getch", Type::Int, &[]),
    ("getarray", Type::Int, &[Type::Int]),
    ("putint", Type::Void, &[Type::Int]),
    ("putch", Type::Void, &[Type::Int]),
    ("putarray", Type::Void, &[Type::Int, Type::Int]),
    ("starttime", Type::Void, &[]),
    ("stoptime", Type::Void, &[]),
];

pub fn generate(program: &CompUnit, mode: OutputMode) -> CompilerResult<String> {
    match mode {
        OutputMode::Koopa => KoopaGen::new().gen_program(program),
        OutputMode::Riscv => RiscvGen::new().gen_program(program),
    }
}

fn is_lib_func(name: &str) -> bool {
    LIB_FUNCS.iter().any(|(n, _, _)| *n == name)
}

fn lib_func_ret_type(name: &str) -> Option<Type> {
    LIB_FUNCS.iter().find(|(n, _, _)| *n == name).map(|(_, t, _)| *t)
}

// ── Koopa IR ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum Symbol {
    Const(i32),
    Var(String),
    Array(String),
}

struct KoopaGen {
    scopes: Vec<HashMap<String, Symbol>>,
    globals: HashMap<String, Symbol>,
    name_count: HashMap<String, usize>,
    tmp: usize,
    label: usize,
    sc_count: usize,
    loop_stack: Vec<(String, String)>,
    current_func_ret_type: Type,
    body: String,
    decls: String,
    lib_funcs_emitted: HashSet<String>,
    global_decls: String,
    func_ret_types: HashMap<String, Type>,
}

impl KoopaGen {
    fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
            globals: HashMap::new(),
            name_count: HashMap::new(),
            tmp: 0,
            label: 0,
            sc_count: 0,
            loop_stack: Vec::new(),
            current_func_ret_type: Type::Int,
            body: String::new(),
            decls: String::new(),
            lib_funcs_emitted: HashSet::new(),
            global_decls: String::new(),
            func_ret_types: HashMap::new(),
        }
    }

    fn enter_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn exit_scope(&mut self) {
        self.scopes.pop();
    }

    fn lookup(&self, name: &str) -> Option<&Symbol> {
        for scope in self.scopes.iter().rev() {
            if let Some(sym) = scope.get(name) {
                return Some(sym);
            }
        }
        self.globals.get(name)
    }

    fn current_scope_contains(&self, name: &str) -> bool {
        self.scopes.last().map_or(false, |s| s.contains_key(name))
    }

    fn mangle(&mut self, name: &str) -> String {
        let count = self.name_count.entry(name.to_string()).or_insert(0);
        let mangled = if *count == 0 {
            name.to_string()
        } else {
            format!("{}_{}", name, count)
        };
        *count += 1;
        mangled
    }

    fn alloc_tmp(&mut self) -> String {
        let t = format!("%{}", self.tmp);
        self.tmp += 1;
        t
    }

    fn alloc_sc(&mut self) -> String {
        let t = format!("@sc_{}", self.sc_count);
        self.sc_count += 1;
        t
    }

    fn new_label(&mut self) -> String {
        let l = format!("%label_{}", self.label);
        self.label += 1;
        l
    }

    fn emit(&mut self, s: &str) {
        self.body.push_str("  ");
        self.body.push_str(s);
        self.body.push('\n');
    }

    fn emit_label(&mut self, label: &str) {
        self.body.push_str(label);
        self.body.push_str(":\n");
    }

    fn emit_decl(&mut self, s: &str) {
        self.decls.push_str(s);
        self.decls.push('\n');
    }

    fn emit_lib_decl(&mut self, name: &str) {
        if self.lib_funcs_emitted.contains(name) {
            return;
        }
        self.lib_funcs_emitted.insert(name.to_string());
        if let Some((_, _, param_types)) = LIB_FUNCS.iter().find(|(n, _, _)| *n == name) {
            let params: Vec<String> = param_types
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let t_str = match t {
                        Type::Int => "i32",
                        Type::Void => "void",
                    };
                    format!("@p{i}: {t_str}")
                })
                .collect();
            let params_str = if params.is_empty() {
                String::new()
            } else {
                params.join(", ")
            };
            self.emit_decl(&format!("decl @{name}({params_str})"));
        }
    }

    fn type_str(t: Type) -> &'static str {
        match t {
            Type::Int => "i32",
            Type::Void => "void",
        }
    }

    fn gen_program(mut self, program: &CompUnit) -> CompilerResult<String> {
        let mut out = String::new();

        // First pass: collect global declarations and function types
        for item in &program.items {
            match item {
                GlobalItem::FuncDef(f) => {
                    self.func_ret_types.insert(f.name.clone(), f.ret_type);
                }
                GlobalItem::FuncDecl(f) => {
                    self.func_ret_types.insert(f.name.clone(), f.ret_type);
                }
                GlobalItem::Decl(decl) => {
                    match decl {
                        Decl::Const(defs) => {
                            for def in defs {
                                let val = self.eval_const(&def.init)?;
                                self.globals.insert(def.name.clone(), Symbol::Const(val));
                            }
                        }
                        Decl::Var(defs) => {
                            for def in defs {
                                if def.dims.is_empty() {
                                    let init_val = def.init.as_ref()
                                        .map(|e| self.eval_const(e))
                                        .transpose()?
                                        .unwrap_or(0);
                                    self.global_decls.push_str(&format!(
                                        "global @{} = alloc i32, {}\n",
                                        def.name, init_val
                                    ));
                                    self.globals.insert(def.name.clone(), Symbol::Var(def.name.clone()));
                                } else {
                                    let dims_str: Vec<String> = def.dims.iter()
                                        .rev()
                                        .map(|d| format!("i32, {}", self.eval_const(d).unwrap_or(1)))
                                        .collect();
                                    let array_type = format!("[{}]", dims_str.join(", "));
                                    self.global_decls.push_str(&format!(
                                        "global @{} = alloc {}\n", def.name, array_type
                                    ));
                                    self.globals.insert(def.name.clone(), Symbol::Array(def.name.clone()));
                                }
                            }
                        }
                    }
                }
            }
        }

        // Second pass: generate functions
        for item in &program.items {
            match item {
                GlobalItem::FuncDef(func) => {
                    self.name_count.clear();
                    self.scopes = vec![HashMap::new()];
                    self.body.clear();
                    self.current_func_ret_type = func.ret_type;
                    self.tmp = 0;
                    self.label = 0;
                    self.sc_count = 0;
                    self.loop_stack.clear();

                    // Emit lib decls for called functions
                    self.gen_block_for_lib_decls(&func.body)?;

                    // Add function params to scope
                    for param in &func.params {
                        let koopa_name = self.mangle(&param.name);
                        self.emit(&format!("@{} = alloca i32", koopa_name));
                        self.emit(&format!(
                            "store @{}, @{}",
                            param.name, koopa_name
                        ));
                        self.scopes
                            .last_mut()
                            .unwrap()
                            .insert(param.name.clone(), Symbol::Var(koopa_name));
                    }

                    self.gen_block(&func.body)?;

                    let params_str: Vec<String> = func
                        .params
                        .iter()
                        .map(|p| format!("@{}: i32", p.name))
                        .collect();
                    let params_sig = if params_str.is_empty() {
                        String::new()
                    } else {
                        params_str.join(", ")
                    };

                    let ret_str = Self::type_str(func.ret_type);
                    let header = format!("fun @{}", func.name);
                    let header = if params_sig.is_empty() {
                        format!("{header}(): {ret_str} {{\n%entry:")
                    } else {
                        format!("{header}({params_sig}): {ret_str} {{\n%entry:")
                    };

                    let func_str = format!(
                        "{}{}{}\n{}}}\n",
                        self.global_decls, self.decls, header, self.body
                    );
                    out.push_str(&func_str);
                    self.global_decls.clear();
                    self.decls.clear();
                }
                GlobalItem::Decl(_) | GlobalItem::FuncDecl(_) => {}
            }
        }
        // Any remaining global decls
        if !self.global_decls.is_empty() {
            out.push_str(&self.global_decls);
        }
        Ok(out)
    }

    fn gen_block_for_lib_decls(&mut self, block: &Block) -> CompilerResult<()> {
        for item in &block.items {
            match item {
                BlockItem::Stmt(stmt) => {
                    self.find_calls_in_stmt(stmt)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn find_calls_in_stmt(&mut self, stmt: &Stmt) -> CompilerResult<()> {
        match stmt {
            Stmt::Return(Some(expr)) | Stmt::Expr(expr) => {
                self.find_calls_in_expr(expr);
            }
            Stmt::Assign { expr, .. } => {
                self.find_calls_in_expr(expr);
            }
            Stmt::Block(block) => {
                self.gen_block_for_lib_decls(block)?;
            }
            Stmt::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.find_calls_in_expr(cond);
                self.find_calls_in_stmt(then_branch)?;
                if let Some(else_s) = else_branch {
                    self.find_calls_in_stmt(else_s)?;
                }
            }
            Stmt::While { cond, body } => {
                self.find_calls_in_expr(cond);
                self.find_calls_in_stmt(body)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn find_calls_in_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Call { name, args } => {
                if is_lib_func(name) {
                    self.emit_lib_decl(name);
                }
                for arg in args {
                    self.find_calls_in_expr(arg);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.find_calls_in_expr(lhs);
                self.find_calls_in_expr(rhs);
            }
            Expr::Unary { expr, .. } => {
                self.find_calls_in_expr(expr);
            }
            Expr::Index { array, index } => {
                self.find_calls_in_expr(array);
                self.find_calls_in_expr(index);
            }
            _ => {}
        }
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
                    if self.current_scope_contains(&def.name) {
                        return Err(CompilerError::new(format!(
                            "redeclaration of '{}'",
                            def.name
                        )));
                    }
                    let val = self.eval_const(&def.init)?;
                    self.scopes
                        .last_mut()
                        .unwrap()
                        .insert(def.name.clone(), Symbol::Const(val));
                }
            }
            Decl::Var(defs) => {
                for def in defs {
                    if self.current_scope_contains(&def.name) {
                        return Err(CompilerError::new(format!(
                            "redeclaration of '{}'",
                            def.name
                        )));
                    }
                    let koopa_name = self.mangle(&def.name);
                    if def.dims.is_empty() {
                        self.emit(&format!("@{} = alloca i32", koopa_name));
                        if let Some(init) = &def.init {
                            let val = self.gen_expr(init)?;
                            self.emit(&format!("store {val}, @{koopa_name}"));
                        }
                        self.scopes
                            .last_mut()
                            .unwrap()
                            .insert(def.name.clone(), Symbol::Var(koopa_name));
                    } else {
                        let dims_str: Vec<String> = def
                            .dims
                            .iter()
                            .rev()
                            .map(|d| format!("i32, {}", self.eval_const(d).unwrap_or(1)))
                            .collect();
                        let array_type = format!("[{}]", dims_str.join(", "));
                        self.emit(&format!(
                            "@{} = alloc {}",
                            koopa_name, array_type
                        ));
                        self.scopes
                            .last_mut()
                            .unwrap()
                            .insert(def.name.clone(), Symbol::Array(koopa_name));
                    }
                }
            }
        }
        Ok(())
    }

    fn gen_stmt(&mut self, stmt: &Stmt) -> CompilerResult<()> {
        match stmt {
            Stmt::Return(expr) => {
                if let Some(e) = expr {
                    let val = self.gen_expr(e)?;
                    self.emit(&format!("ret {val}"));
                } else {
                    self.emit("ret");
                }
            }
            Stmt::Assign { name, index, expr } => {
                if index.is_empty() {
                    let koopa_name = match self.lookup(name) {
                        Some(Symbol::Var(v)) => v.clone(),
                        Some(Symbol::Const(_)) => {
                            return Err(CompilerError::new(format!(
                                "cannot assign to constant '{name}'"
                            )));
                        }
                        None => {
                            return Err(CompilerError::new(format!(
                                "undefined variable '{name}'"
                            )));
                        }
                        _ => {
                            return Err(CompilerError::new(format!(
                                "cannot assign to array '{name}' without index"
                            )));
                        }
                    };
                    let val = self.gen_expr(expr)?;
                    self.emit(&format!("store {val}, @{koopa_name}"));
                } else {
                    let koopa_name = match self.lookup(name) {
                        Some(Symbol::Array(n)) => n.clone(),
                        _ => {
                            return Err(CompilerError::new(format!(
                                "'{name}' is not an array"
                            )));
                        }
                    };
                    let val = self.gen_expr(expr)?;
                    // Build the pointer chain
                    let mut ptr = format!("@{}", koopa_name);
                    for idx in index {
                        let idx_val = self.gen_expr(idx)?;
                        let p0 = self.alloc_tmp();
                        self.emit(&format!("{p0} = getelemptr {ptr}, 0"));
                        let p1 = self.alloc_tmp();
                        self.emit(&format!("{p1} = getptr {p0}, {idx_val}"));
                        ptr = p1;
                    }
                    self.emit(&format!("store {val}, {ptr}"));
                }
            }
            Stmt::Expr(expr) => {
                self.gen_expr(expr)?;
            }
            Stmt::Block(block) => {
                self.enter_scope();
                self.gen_block(block)?;
                self.exit_scope();
            }
            Stmt::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let cond_val = self.gen_expr(cond)?;
                let then_label = self.new_label();
                let else_label = self.new_label();
                let end_label = self.new_label();
                if else_branch.is_some() {
                    self.emit(&format!("br {cond_val}, {then_label}, {else_label}"));
                } else {
                    self.emit(&format!("br {cond_val}, {then_label}, {end_label}"));
                }
                self.emit_label(&then_label);
                self.gen_stmt(then_branch)?;
                self.emit(&format!("jump {end_label}"));
                if let Some(else_s) = else_branch {
                    self.emit_label(&else_label);
                    self.gen_stmt(else_s)?;
                    self.emit(&format!("jump {end_label}"));
                }
                self.emit_label(&end_label);
            }
            Stmt::While { cond, body } => {
                let entry_label = self.new_label();
                let body_label = self.new_label();
                let end_label = self.new_label();
                self.emit(&format!("jump {entry_label}"));
                self.emit_label(&entry_label);
                let cond_val = self.gen_expr(cond)?;
                self.emit(&format!("br {cond_val}, {body_label}, {end_label}"));
                self.loop_stack
                    .push((entry_label.clone(), end_label.clone()));
                self.emit_label(&body_label);
                self.gen_stmt(body)?;
                self.emit(&format!("jump {entry_label}"));
                self.loop_stack.pop();
                self.emit_label(&end_label);
            }
            Stmt::Break => {
                let (_, break_label) = self
                    .loop_stack
                    .last()
                    .ok_or_else(|| CompilerError::new("'break' outside of loop"))?;
                let label = break_label.clone();
                self.emit(&format!("jump {label}"));
            }
            Stmt::Continue => {
                let (continue_label, _) = self
                    .loop_stack
                    .last()
                    .ok_or_else(|| CompilerError::new("'continue' outside of loop"))?;
                let label = continue_label.clone();
                self.emit(&format!("jump {label}"));
            }
        }
        Ok(())
    }

    fn eval_const(&self, expr: &Expr) -> CompilerResult<i32> {
        match expr {
            Expr::Int(value) => Ok(*value),
            Expr::LVal(name) => match self.lookup(name) {
                Some(Symbol::Const(v)) => Ok(*v),
                _ => Err(CompilerError::new(format!(
                    "'{name}' is not a compile-time constant"
                ))),
            },
            Expr::Unary { op, expr } => {
                let value = self.eval_const(expr)?;
                match op {
                    UnaryOp::Plus => Ok(value),
                    UnaryOp::Minus => Ok(value.wrapping_neg()),
                    UnaryOp::Not => Ok((value == 0) as i32),
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                let lhs = self.eval_const(lhs)?;
                let rhs = self.eval_const(rhs)?;
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
            Expr::Call { .. } => Err(CompilerError::new(
                "function call is not a compile-time constant",
            )),
            Expr::Index { .. } => Err(CompilerError::new(
                "array access is not a compile-time constant",
            )),
        }
    }

    fn gen_expr(&mut self, expr: &Expr) -> CompilerResult<String> {
        match expr {
            Expr::Int(n) => Ok(n.to_string()),
            Expr::LVal(name) => {
                match self.lookup(name) {
                    Some(Symbol::Const(v)) => Ok(v.to_string()),
                    Some(Symbol::Var(koopa_name)) => {
                        let koopa_name = koopa_name.clone();
                        let tmp = self.alloc_tmp();
                        self.emit(&format!("{tmp} = load @{koopa_name}"));
                        Ok(tmp)
                    }
                    Some(Symbol::Array(n)) => {
                        let n = n.clone();
                        let tmp = self.alloc_tmp();
                        self.emit(&format!("{tmp} = getelemptr @{n}, 0"));
                        Ok(tmp)
                    }
                    None => Err(CompilerError::new(format!(
                        "undefined identifier '{name}'"
                    ))),
                }
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
            Expr::Binary { op, lhs, rhs } => match op {
                BinaryOp::And => {
                    let sc = self.alloc_sc();
                    self.emit(&format!("{sc} = alloca i32"));
                    let lv = self.gen_expr(lhs)?;
                    let rhs_label = self.new_label();
                    let false_label = self.new_label();
                    let end_label = self.new_label();
                    self.emit(&format!("br {lv}, {rhs_label}, {false_label}"));
                    self.emit_label(&rhs_label);
                    let rv = self.gen_expr(rhs)?;
                    let t = self.alloc_tmp();
                    self.emit(&format!("{t} = ne {rv}, 0"));
                    self.emit(&format!("store {t}, {sc}"));
                    self.emit(&format!("jump {end_label}"));
                    self.emit_label(&false_label);
                    self.emit(&format!("store 0, {sc}"));
                    self.emit(&format!("jump {end_label}"));
                    self.emit_label(&end_label);
                    let result = self.alloc_tmp();
                    self.emit(&format!("{result} = load {sc}"));
                    Ok(result)
                }
                BinaryOp::Or => {
                    let sc = self.alloc_sc();
                    self.emit(&format!("{sc} = alloca i32"));
                    let lv = self.gen_expr(lhs)?;
                    let true_label = self.new_label();
                    let rhs_label = self.new_label();
                    let end_label = self.new_label();
                    self.emit(&format!("br {lv}, {true_label}, {rhs_label}"));
                    self.emit_label(&true_label);
                    self.emit(&format!("store 1, {sc}"));
                    self.emit(&format!("jump {end_label}"));
                    self.emit_label(&rhs_label);
                    let rv = self.gen_expr(rhs)?;
                    let t = self.alloc_tmp();
                    self.emit(&format!("{t} = ne {rv}, 0"));
                    self.emit(&format!("store {t}, {sc}"));
                    self.emit(&format!("jump {end_label}"));
                    self.emit_label(&end_label);
                    let result = self.alloc_tmp();
                    self.emit(&format!("{result} = load {sc}"));
                    Ok(result)
                }
                _ => {
                    let lv = self.gen_expr(lhs)?;
                    let rv = self.gen_expr(rhs)?;
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
            },
            Expr::Index { array, index } => {
                let idx_val = self.gen_expr(index)?;
                let base = self.gen_expr_base(array)?;
                let p0 = self.alloc_tmp();
                self.emit(&format!("{p0} = getelemptr {base}, 0"));
                let p1 = self.alloc_tmp();
                self.emit(&format!("{p1} = getptr {p0}, {idx_val}"));
                let result = self.alloc_tmp();
                self.emit(&format!("{result} = load {p1}"));
                Ok(result)
            }
            Expr::Call { name, args } => {
                if is_lib_func(name) {
                    self.emit_lib_decl(name);
                }
                let evaled_args: Vec<String> = args
                    .iter()
                    .map(|a| self.gen_expr(a))
                    .collect::<CompilerResult<_>>()?;
                let args_str = evaled_args.join(", ");
                let is_void = lib_func_ret_type(name) == Some(Type::Void)
                    || self.func_ret_types.get(name) == Some(&Type::Void);
                if is_void {
                    if args_str.is_empty() {
                        self.emit(&format!("call @{name}()"));
                    } else {
                        self.emit(&format!("call @{name}({args_str})"));
                    }
                    Ok(String::new())
                } else {
                    let tmp = self.alloc_tmp();
                    if args_str.is_empty() {
                        self.emit(&format!("{tmp} = call @{name}()"));
                    } else {
                        self.emit(&format!("{tmp} = call @{name}({args_str})"));
                    }
                    Ok(tmp)
                }
            }
        }
    }

    fn gen_expr_base(&mut self, expr: &Expr) -> CompilerResult<String> {
        match expr {
            Expr::LVal(name) => match self.lookup(name) {
                Some(Symbol::Var(n)) | Some(Symbol::Array(n)) => Ok(format!("@{}", n)),
                _ => Err(CompilerError::new(format!("'{name}' is not an lvalue"))),
            },
            Expr::Index { array, index } => {
                let idx_val = self.gen_expr(index)?;
                let base = self.gen_expr_base(array)?;
                let p0 = self.alloc_tmp();
                self.emit(&format!("{p0} = getelemptr {base}, 0"));
                let p1 = self.alloc_tmp();
                self.emit(&format!("{p1} = getptr {p0}, {idx_val}"));
                Ok(p1)
            }
            _ => Err(CompilerError::new("not an lvalue")),
        }
    }
}

// ── RISC-V ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum RvSymbol {
    Const(i32),
    Var { offset: i32 },
    Array { offset: i32, dims: Vec<i32> },
}

struct RiscvGen {
    scopes: Vec<HashMap<String, RvSymbol>>,
    var_offsets: HashMap<String, i32>,
    mangled_names: HashMap<String, Vec<String>>,
    read_pos: HashMap<String, usize>,
    frame_size: i32,
    extra_sp: i32,
    label: usize,
    loop_stack: Vec<(String, String)>,
    current_ret_type: Type,
    out: String,
}

impl RiscvGen {
    fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
            var_offsets: HashMap::new(),
            mangled_names: HashMap::new(),
            read_pos: HashMap::new(),
            frame_size: 0,
            extra_sp: 0,
            label: 0,
            loop_stack: Vec::new(),
            current_ret_type: Type::Int,
            out: String::new(),
        }
    }

    fn new_label(&mut self) -> String {
        let l = format!(".L{}", self.label);
        self.label += 1;
        l
    }

    fn emit_label(&mut self, label: &str) {
        self.out.push_str(label);
        self.out.push_str(":\n");
    }

    fn emit_directive(&mut self, s: &str) {
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn enter_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn exit_scope(&mut self) {
        self.scopes.pop();
    }

    fn lookup(&self, name: &str) -> Option<&RvSymbol> {
        for scope in self.scopes.iter().rev() {
            if let Some(sym) = scope.get(name) {
                return Some(sym);
            }
        }
        None
    }

    fn current_scope_contains(&self, name: &str) -> bool {
        self.scopes.last().map_or(false, |s| s.contains_key(name))
    }

    fn next_mangled(&mut self, source_name: &str) -> (String, i32) {
        let pos = self.read_pos.entry(source_name.to_string()).or_insert(0);
        let mangled = &self.mangled_names[source_name][*pos];
        *pos += 1;
        let offset = self.var_offsets[mangled];
        (mangled.clone(), offset)
    }

    fn emit(&mut self, s: &str) {
        self.out.push_str("  ");
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn gen_program(mut self, program: &CompUnit) -> CompilerResult<String> {
        let mut out = String::new();
        out.push_str("  .text\n");

        for item in &program.items {
            match item {
                GlobalItem::FuncDef(func) => {
                    self.scopes = vec![HashMap::new()];
                    self.var_offsets.clear();
                    self.mangled_names.clear();
                    self.read_pos.clear();
                    self.out.clear();
                    self.loop_stack.clear();
                    self.current_ret_type = func.ret_type;
                    self.label = 0;

                    // First pass: collect variables
                    let mut slot = 0i32;
                    let mut name_count: HashMap<String, usize> = HashMap::new();

                    // Add params to the count
                    for param in &func.params {
                        let count = name_count.entry(param.name.clone()).or_insert(0);
                        let mangled = if *count == 0 {
                            param.name.clone()
                        } else {
                            format!("{}_{}", param.name, count)
                        };
                        *count += 1;
                        self.var_offsets.insert(mangled.clone(), slot * 4);
                        self.mangled_names
                            .entry(param.name.clone())
                            .or_default()
                            .push(mangled);
                        slot += 1;
                    }

                    let mut const_vals: HashMap<String, i32> = HashMap::new();
                    self.collect_vars(&func.body, &mut slot, &mut name_count, &mut const_vals);

                    let num_vars = slot;
                    self.frame_size = align16(num_vars * 4);

                    let func_label = &func.name;
                    self.emit_directive(&format!("  .globl {func_label}"));
                    self.emit_label(func_label);

                    // Prologue: save ra, allocate frame
                    let total_frame = self.frame_size + 4; // +4 for ra
                    let aligned_frame = align16(total_frame);
                    let ra_offset = aligned_frame - 4;
                    self.emit(&format!("addi sp, sp, -{}", aligned_frame));
                    self.emit(&format!("sw ra, {ra_offset}(sp)"));

                    // Store params into their stack slots
                    for (i, param) in func.params.iter().enumerate() {
                        let (_, offset) = self.next_mangled(&param.name);
                        // Adjust offset: frame is now at sp + 4 (since we also pushed ra)
                        // Actually the offsets were computed from 0, starting at sp
                        // But we pushed ra above the vars, so we need to add 4 to each offset
                        let adjusted_offset = offset + 4;
                        let reg = match i {
                            0 => "a0",
                            1 => "a1",
                            2 => "a2",
                            3 => "a3",
                            4 => "a4",
                            5 => "a5",
                            6 => "a6",
                            7 => "a7",
                            _ => {
                                // Stack args — load from caller's frame
                                // For simplicity, skip >8 args
                                continue;
                            }
                        };
                        self.emit(&format!("sw {reg}, {adjusted_offset}(sp)"));
                        self.scopes.last_mut().unwrap().insert(
                            param.name.clone(),
                            RvSymbol::Var {
                                offset: adjusted_offset,
                            },
                        );
                    }

                    // Reset read_pos for local vars
                    // (params already consumed their mangled names)
                    // We don't reset — continue reading from the same position

                    self.gen_block(&func.body, aligned_frame)?;

                    out.push_str(&self.out);
                }
                _ => {}
            }
        }
        Ok(out)
    }

    fn collect_eval_const(expr: &Expr, const_vals: &HashMap<String, i32>) -> i32 {
        match expr {
            Expr::Int(n) => *n,
            Expr::LVal(name) => const_vals.get(name).copied().unwrap_or(1),
            Expr::Binary { op: BinaryOp::Mul, lhs, rhs } => {
                Self::collect_eval_const(lhs, const_vals) * Self::collect_eval_const(rhs, const_vals)
            }
            _ => 1, // fallback for non-const dimensions
        }
    }

    fn collect_vars(
        &mut self,
        block: &Block,
        slot: &mut i32,
        name_count: &mut HashMap<String, usize>,
        const_vals: &mut HashMap<String, i32>,
    ) {
        for item in &block.items {
            match item {
                BlockItem::Decl(Decl::Const(defs)) => {
                    for def in defs {
                        let val = Self::collect_eval_const(&def.init, const_vals);
                        const_vals.insert(def.name.clone(), val);
                    }
                }
                BlockItem::Decl(Decl::Var(defs)) => {
                    for def in defs {
                        let count = name_count.entry(def.name.clone()).or_insert(0);
                        let mangled = if *count == 0 {
                            def.name.clone()
                        } else {
                            format!("{}_{}", def.name, count)
                        };
                        *count += 1;
                        self.var_offsets.insert(mangled.clone(), *slot * 4);
                        self.mangled_names
                            .entry(def.name.clone())
                            .or_default()
                            .push(mangled);
                        let elems: i32 = def.dims.iter()
                            .map(|d| Self::collect_eval_const(d, const_vals))
                            .product();
                        *slot += if elems > 0 { elems } else { 1 };
                    }
                }
                BlockItem::Stmt(stmt) => {
                    self.collect_stmt_vars(stmt, slot, name_count, const_vals);
                }
            }
        }
    }

    fn collect_stmt_vars(
        &mut self,
        stmt: &Stmt,
        slot: &mut i32,
        name_count: &mut HashMap<String, usize>,
        const_vals: &mut HashMap<String, i32>,
    ) {
        match stmt {
            Stmt::Block(inner) => {
                self.collect_vars(inner, slot, name_count, const_vals);
            }
            Stmt::If {
                then_branch,
                else_branch,
                ..
            } => {
                self.collect_stmt_vars(then_branch, slot, name_count, const_vals);
                if let Some(else_s) = else_branch {
                    self.collect_stmt_vars(else_s, slot, name_count, const_vals);
                }
            }
            Stmt::While { body, .. } => {
                self.collect_stmt_vars(body, slot, name_count, const_vals);
            }
            _ => {}
        }
    }

    fn gen_block(&mut self, block: &Block, frame: i32) -> CompilerResult<()> {
        for item in &block.items {
            match item {
                BlockItem::Decl(d) => self.gen_decl(d, frame)?,
                BlockItem::Stmt(s) => self.gen_stmt(s, frame)?,
            }
        }
        Ok(())
    }

    fn gen_decl(&mut self, decl: &Decl, frame: i32) -> CompilerResult<()> {
        match decl {
            Decl::Const(defs) => {
                for def in defs {
                    if self.current_scope_contains(&def.name) {
                        return Err(CompilerError::new(format!(
                            "redeclaration of '{}'",
                            def.name
                        )));
                    }
                    let val = self.eval_const(&def.init)?;
                    self.scopes
                        .last_mut()
                        .unwrap()
                        .insert(def.name.clone(), RvSymbol::Const(val));
                }
            }
            Decl::Var(defs) => {
                for def in defs {
                    if self.current_scope_contains(&def.name) {
                        return Err(CompilerError::new(format!(
                            "redeclaration of '{}'",
                            def.name
                        )));
                    }
                    let (_, offset) = self.next_mangled(&def.name);
                    let adjusted_offset = offset + 4; // +4 for ra
                    if def.dims.is_empty() {
                        if let Some(init) = &def.init {
                            self.gen_expr(init, frame)?;
                            self.emit(&format!("sw a0, {adjusted_offset}(sp)"));
                        }
                        self.scopes.last_mut().unwrap().insert(
                            def.name.clone(),
                            RvSymbol::Var {
                                offset: adjusted_offset,
                            },
                        );
                    } else {
                        let dims: Vec<i32> = def.dims.iter()
                            .map(|d| self.eval_const(d))
                            .collect::<CompilerResult<_>>()?;
                        self.scopes.last_mut().unwrap().insert(
                            def.name.clone(),
                            RvSymbol::Array {
                                offset: adjusted_offset,
                                dims,
                            },
                        );
                    }
                }
            }
        }
        Ok(())
    }

    fn gen_stmt(&mut self, stmt: &Stmt, frame: i32) -> CompilerResult<()> {
        match stmt {
            Stmt::Return(expr) => {
                if let Some(e) = expr {
                    self.gen_expr(e, frame)?;
                }
                // Epilogue
                let ra_offset = frame - 4;
                self.emit(&format!("lw ra, {ra_offset}(sp)"));
                self.emit(&format!("addi sp, sp, {frame}"));
                self.emit("ret");
            }
            Stmt::Assign { name, index, expr } => {
                if index.is_empty() {
                    let offset = match self.lookup(name) {
                        Some(RvSymbol::Var { offset }) => *offset,
                        Some(RvSymbol::Const(_)) => {
                            return Err(CompilerError::new(format!(
                                "cannot assign to constant '{name}'"
                            )));
                        }
                        None => {
                            return Err(CompilerError::new(format!(
                                "undefined variable '{name}'"
                            )));
                        }
                        _ => {
                            return Err(CompilerError::new(format!(
                                "cannot assign to array '{name}' without index"
                            )));
                        }
                    };
                    self.gen_expr(expr, frame)?;
                    self.emit(&format!("sw a0, {offset}(sp)"));
                } else {
                    let (arr_offset, arr_dims) = match self.lookup(name) {
                        Some(RvSymbol::Array { offset, dims }) => (*offset, dims.clone()),
                        _ => {
                            return Err(CompilerError::new(format!(
                                "'{name}' is not an array"
                            )));
                        }
                    };
                    // Evaluate RHS first, save to t1
                    self.gen_expr(expr, frame)?;
                    self.emit("mv t1, a0");
                    // Compute flat element index: sum(index_i * stride_i)
                    for (i, idx) in index.iter().enumerate() {
                        self.gen_expr(idx, frame)?; // a0 = index value
                        let stride: i32 = arr_dims.iter().skip(i + 1).product();
                        if stride != 1 {
                            self.emit(&format!("li t0, {}", stride));
                            self.emit("mul a0, a0, t0");
                        }
                        if i == 0 {
                            self.emit("mv t2, a0");
                        } else {
                            self.emit("add t2, t2, a0");
                        }
                    }
                    self.emit("slli t2, t2, 2");
                    self.emit(&format!("addi t2, t2, {}", arr_offset + self.extra_sp));
                    self.emit("add t2, sp, t2");
                    self.emit("sw t1, 0(t2)");
                }
            }
            Stmt::Expr(expr) => {
                self.gen_expr(expr, frame)?;
            }
            Stmt::Block(block) => {
                self.enter_scope();
                self.gen_block(block, frame)?;
                self.exit_scope();
            }
            Stmt::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.gen_expr(cond, frame)?;
                let then_label = self.new_label();
                let else_label = self.new_label();
                let end_label = self.new_label();
                if else_branch.is_some() {
                    self.emit(&format!("beqz a0, {else_label}"));
                } else {
                    self.emit(&format!("beqz a0, {end_label}"));
                }
                self.emit_label(&then_label);
                self.gen_stmt(then_branch, frame)?;
                self.emit(&format!("j {end_label}"));
                if let Some(else_s) = else_branch {
                    self.emit_label(&else_label);
                    self.gen_stmt(else_s, frame)?;
                    self.emit(&format!("j {end_label}"));
                }
                self.emit_label(&end_label);
            }
            Stmt::While { cond, body } => {
                let entry_label = self.new_label();
                let body_label = self.new_label();
                let end_label = self.new_label();
                self.emit(&format!("j {entry_label}"));
                self.emit_label(&entry_label);
                self.gen_expr(cond, frame)?;
                self.emit(&format!("beqz a0, {end_label}"));
                self.loop_stack
                    .push((entry_label.clone(), end_label.clone()));
                self.emit_label(&body_label);
                self.gen_stmt(body, frame)?;
                self.emit(&format!("j {entry_label}"));
                self.loop_stack.pop();
                self.emit_label(&end_label);
            }
            Stmt::Break => {
                let (_, break_label) = self
                    .loop_stack
                    .last()
                    .ok_or_else(|| CompilerError::new("'break' outside of loop"))?;
                let label = break_label.clone();
                self.emit(&format!("j {label}"));
            }
            Stmt::Continue => {
                let (continue_label, _) = self
                    .loop_stack
                    .last()
                    .ok_or_else(|| CompilerError::new("'continue' outside of loop"))?;
                let label = continue_label.clone();
                self.emit(&format!("j {label}"));
            }
        }
        Ok(())
    }

    fn eval_const(&self, expr: &Expr) -> CompilerResult<i32> {
        match expr {
            Expr::Int(value) => Ok(*value),
            Expr::LVal(name) => match self.lookup(name) {
                Some(RvSymbol::Const(v)) => Ok(*v),
                _ => Err(CompilerError::new(format!(
                    "'{name}' is not a compile-time constant"
                ))),
            },
            Expr::Unary { op, expr } => {
                let value = self.eval_const(expr)?;
                match op {
                    UnaryOp::Plus => Ok(value),
                    UnaryOp::Minus => Ok(value.wrapping_neg()),
                    UnaryOp::Not => Ok((value == 0) as i32),
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                let lhs = self.eval_const(lhs)?;
                let rhs = self.eval_const(rhs)?;
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
            Expr::Call { .. } => Err(CompilerError::new(
                "function call is not a compile-time constant",
            )),
            Expr::Index { .. } => Err(CompilerError::new(
                "array access is not a compile-time constant",
            )),
        }
    }

    fn gen_expr(&mut self, expr: &Expr, frame: i32) -> CompilerResult<()> {
        match expr {
            Expr::Int(n) => {
                self.emit(&format!("li a0, {n}"));
            }
            Expr::LVal(name) => match self.lookup(name) {
                Some(RvSymbol::Const(v)) => {
                    self.emit(&format!("li a0, {v}"));
                }
                Some(RvSymbol::Var { offset }) => {
                    let addr = *offset + self.extra_sp;
                    self.emit(&format!("lw a0, {addr}(sp)"));
                }
                Some(RvSymbol::Array { offset, .. }) => {
                    self.emit(&format!("addi a0, sp, {}", offset + self.extra_sp));
                }
                None => {
                    return Err(CompilerError::new(format!(
                        "undefined identifier '{name}'"
                    )));
                }
            },
            Expr::Unary { op, expr } => {
                self.gen_expr(expr, frame)?;
                match op {
                    UnaryOp::Plus => {}
                    UnaryOp::Minus => self.emit("neg a0, a0"),
                    UnaryOp::Not => self.emit("seqz a0, a0"),
                }
            }
            Expr::Binary { op, lhs, rhs } => match op {
                BinaryOp::And => {
                    self.gen_expr(lhs, frame)?;
                    let false_label = self.new_label();
                    let end_label = self.new_label();
                    self.emit(&format!("beqz a0, {false_label}"));
                    self.gen_expr(rhs, frame)?;
                    self.emit("snez a0, a0");
                    self.emit("addi sp, sp, -4");
                    self.emit("sw a0, 0(sp)");
                    self.extra_sp += 4;
                    self.emit(&format!("j {end_label}"));
                    self.emit_label(&false_label);
                    self.emit("addi sp, sp, -4");
                    self.emit("sw zero, 0(sp)");
                    self.extra_sp += 4;
                    self.emit_label(&end_label);
                    self.emit("lw a0, 0(sp)");
                    self.emit("addi sp, sp, 4");
                    self.extra_sp -= 4;
                }
                BinaryOp::Or => {
                    self.gen_expr(lhs, frame)?;
                    let true_label = self.new_label();
                    let end_label = self.new_label();
                    self.emit(&format!("bnez a0, {true_label}"));
                    self.gen_expr(rhs, frame)?;
                    self.emit("snez a0, a0");
                    self.emit("addi sp, sp, -4");
                    self.emit("sw a0, 0(sp)");
                    self.extra_sp += 4;
                    self.emit(&format!("j {end_label}"));
                    self.emit_label(&true_label);
                    self.emit("addi sp, sp, -4");
                    self.emit("li a0, 1");
                    self.emit("sw a0, 0(sp)");
                    self.extra_sp += 4;
                    self.emit_label(&end_label);
                    self.emit("lw a0, 0(sp)");
                    self.emit("addi sp, sp, 4");
                    self.extra_sp -= 4;
                }
                _ => {
                    self.gen_expr(lhs, frame)?;
                    self.emit("addi sp, sp, -4");
                    self.emit("sw a0, 0(sp)");
                    self.extra_sp += 4;
                    self.gen_expr(rhs, frame)?;
                    self.emit("lw t0, 0(sp)");
                    self.emit("addi sp, sp, 4");
                    self.extra_sp -= 4;
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
                        BinaryOp::And | BinaryOp::Or => unreachable!(),
                    }
                }
            },
            Expr::Index { array, index } => {
                // Walk the Index chain to find base LVal
                let mut indices: Vec<&Expr> = vec![index.as_ref()];
                let mut base: &Expr = array.as_ref();
                while let Expr::Index { array: inner_arr, index: inner_idx } = base {
                    indices.push(inner_idx.as_ref());
                    base = inner_arr.as_ref();
                }
                indices.reverse();
                if let Expr::LVal(name) = base {
                    let (arr_offset, arr_dims) = match self.lookup(name) {
                        Some(RvSymbol::Array { offset, dims }) => (*offset, dims.clone()),
                        _ => return Err(CompilerError::new(format!("'{name}' is not an array"))),
                    };
                    // Compute flat element index: sum(index_i * stride_i)
                    for (i, idx) in indices.iter().enumerate() {
                        self.gen_expr(idx, frame)?; // a0 = index value
                        let stride: i32 = arr_dims.iter().skip(i + 1).product();
                        if stride != 1 {
                            self.emit(&format!("li t1, {}", stride));
                            self.emit("mul a0, a0, t1");
                        }
                        if i == 0 {
                            self.emit("mv t2, a0");
                        } else {
                            self.emit("add t2, t2, a0");
                        }
                    }
                    self.emit("slli t2, t2, 2");
                    self.emit(&format!("addi t2, t2, {}", arr_offset + self.extra_sp));
                    self.emit("add t2, sp, t2");
                    self.emit("lw a0, 0(t2)");
                } else {
                    return Err(CompilerError::new("invalid array access"));
                }
            }
            Expr::Call { name, args } => {
                for (_i, arg) in args.iter().enumerate() {
                    self.gen_expr(arg, frame)?;
                    self.emit("addi sp, sp, -4");
                    self.emit("sw a0, 0(sp)");
                    self.extra_sp += 4;
                }
                for i in (0..args.len()).rev() {
                    let reg = match i {
                        0 => "a0",
                        1 => "a1",
                        2 => "a2",
                        3 => "a3",
                        4 => "a4",
                        5 => "a5",
                        6 => "a6",
                        7 => "a7",
                        _ => "a7",
                    };
                    self.emit(&format!("lw {reg}, 0(sp)"));
                    self.emit("addi sp, sp, 4");
                    self.extra_sp -= 4;
                }
                self.emit(&format!("call {name}"));
            }
        }
        Ok(())
    }
}

fn align16(n: i32) -> i32 {
    (n + 15) & !15
}
