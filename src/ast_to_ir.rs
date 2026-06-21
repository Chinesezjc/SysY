//! AST → in-memory IR translation.
//!
//! Mirrors the logic of [`crate::koopa_gen`] but produces an [`IrProgram`]
//! instead of Koopa IR text.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::codegen::{LIB_FUNCS, is_lib_func, lib_func_ret_type};
use crate::error::{CompilerError, CompilerResult};
use crate::ir::*;
use crate::ir_builder::*;

// ── Local symbol table ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum IrSymbol {
    Const(i32),
    Var(usize),                   // global index of alloca
    Array(usize, usize),          // global index + num dims
    PtrArray(usize),              // global index
    NdParam { name: usize, dims: Vec<i32> },
}

// ── AST → IR translator ──────────────────────────────────────────────────────

pub(crate) struct AstToIr {
    pub program: IrProgram,

    // Per-function state (reset for each function)
    scopes: Vec<HashMap<String, IrSymbol>>,
    globals: HashMap<String, IrSymbol>,
    name_count: HashMap<String, usize>,
    param_sig_names: HashMap<String, usize>,
    loop_stack: Vec<(usize, usize)>,
    current_ret_type: IrType,
    lib_funcs_emitted: HashSet<String>,
    func_ret_types: HashMap<String, Type>,
    builder: Option<IrBuilder>,
}

impl AstToIr {
    pub fn new() -> Self {
        AstToIr {
            program: IrProgram::new(),
            scopes: vec![HashMap::new()],
            globals: HashMap::new(),
            name_count: HashMap::new(),
            param_sig_names: HashMap::new(),
            loop_stack: Vec::new(),
            current_ret_type: IrType::I32,
            lib_funcs_emitted: HashSet::new(),
            func_ret_types: HashMap::new(),
            builder: None,
        }
    }

    // ── Scope helpers ────────────────────────────────────────────────────────

    fn enter_scope(&mut self) { self.scopes.push(HashMap::new()); }
    fn exit_scope(&mut self) { self.scopes.pop(); }

    fn lookup(&self, name: &str) -> Option<&IrSymbol> {
        for scope in self.scopes.iter().rev() {
            if let Some(sym) = scope.get(name) { return Some(sym); }
        }
        self.globals.get(name)
    }

    fn current_scope_contains(&self, name: &str) -> bool {
        self.scopes.last().map_or(false, |s| s.contains_key(name))
    }

    fn mangle(&mut self, name: &str) -> String {
        let count = self.name_count.entry(name.to_string()).or_insert(0);
        let mangled = if *count == 0 { name.to_string() } else { format!("{name}_{count}") };
        *count += 1;
        mangled
    }

    fn b(&mut self) -> &mut IrBuilder { self.builder.as_mut().expect("no builder") }

    // ── String table merge ───────────────────────────────────────────────────

    /// Merge builder's string tables into the program, returning the index
    /// offset for each table so caller can rebase indices.
    fn merge_meta(&mut self, meta: BuilderMeta) -> (usize, usize, usize, usize) {
        let local_off = self.program.local_names.len();
        let global_off = self.program.global_names.len();
        let block_off = self.program.block_names.len();
        let func_off = self.program.func_names.len();
        self.program.local_names.extend(meta.local_names);
        self.program.global_names.extend(meta.global_names);
        self.program.block_names.extend(meta.block_names);
        self.program.func_names.extend(meta.func_names);
        (local_off, global_off, block_off, func_off)
    }

    // ── Top-level ────────────────────────────────────────────────────────────

    pub fn gen_program(mut self, program: &CompUnit) -> CompilerResult<IrProgram> {
        // First pass: collect globals and function signatures
        for item in &program.items {
            match item {
                GlobalItem::FuncDef(f) => { self.func_ret_types.insert(f.name.clone(), f.ret_type); }
                GlobalItem::FuncDecl(f) => { self.func_ret_types.insert(f.name.clone(), f.ret_type); }
                GlobalItem::Decl(decl) => self.gen_global_decl(decl)?,
            }
        }

        // Second pass: generate functions
        for item in &program.items {
            if let GlobalItem::FuncDef(func) = item {
                self.gen_func(func)?;
            }
        }

        Ok(self.program)
    }

    fn gen_global_decl(&mut self, decl: &Decl) -> CompilerResult<()> {
        match decl {
            Decl::Const(defs) => {
                for def in defs {
                    if def.dims.is_empty() && !matches!(&def.init, Expr::InitList(_)) {
                        let val = self.eval_const_global(&def.init)?;
                        self.globals.insert(def.name.clone(), IrSymbol::Const(val));
                    } else {
                        let dims: Vec<i32> = def.dims.iter()
                            .map(|d| self.eval_const_global(d))
                            .collect::<CompilerResult<_>>()?;
                        let g_idx = self.program.intern_global(def.name.clone());
                        let array_type = make_array_type(&dims);
                        let init = self.global_init_vals(&dims, &def.init);
                        self.program.globals.push(IrGlobal { name: g_idx, ty: array_type, init });
                        self.globals.insert(def.name.clone(), IrSymbol::Array(g_idx, dims.len()));
                    }
                }
            }
            Decl::Var(defs) => {
                for def in defs {
                    if def.dims.is_empty() {
                        let init_val = def.init.as_ref()
                            .map(|e| self.eval_const_global(e)).transpose()?.unwrap_or(0);
                        let g_idx = self.program.intern_global(def.name.clone());
                        self.program.globals.push(IrGlobal {
                            name: g_idx, ty: IrType::I32,
                            init: IrGlobalInit::Values(vec![init_val]),
                        });
                        self.globals.insert(def.name.clone(), IrSymbol::Var(g_idx));
                    } else {
                        let dims: Vec<i32> = def.dims.iter()
                            .map(|d| self.eval_const_global(d))
                            .collect::<CompilerResult<_>>()?;
                        let g_idx = self.program.intern_global(def.name.clone());
                        let array_type = make_array_type(&dims);
                        let init = if let Some(e) = &def.init {
                            self.global_init_vals(&dims, e)
                        } else { IrGlobalInit::Zero };
                        self.program.globals.push(IrGlobal { name: g_idx, ty: array_type, init });
                        self.globals.insert(def.name.clone(), IrSymbol::Array(g_idx, dims.len()));
                    }
                }
            }
        }
        Ok(())
    }

    fn gen_func(&mut self, func: &FuncDef) -> CompilerResult<()> {
        self.name_count.clear();
        self.scopes = vec![HashMap::new()];
        self.param_sig_names.clear();
        self.loop_stack.clear();

        let ret_type = if func.ret_type == Type::Void { IrType::Void } else { IrType::I32 };
        self.current_ret_type = ret_type.clone();

        // Intern func name in PROGRAM (not builder)
        let func_idx = self.program.intern_func(func.name.clone());

        // Create builder with program-absolute base indices
        let base_local = self.program.local_names.len();
        let base_global = self.program.global_names.len();
        let base_block = self.program.block_names.len();
        let base_func = self.program.func_names.len();

        let mut params: Vec<(usize, IrType)> = Vec::new();
        let mut builder = IrBuilder::new(func_idx, Vec::new(), ret_type,
            base_local, base_global, base_block, base_func);
        self.builder = Some(builder);

        // Process parameters
        for param in &func.params {
            if param.is_array {
                if self.globals.contains_key(&param.name) { self.mangle(&param.name); }
                // Array params: sig_name == koopa_name (single mangle call, matching koopa_gen.rs)
                let koopa_name = self.mangle(&param.name);
                let koopa_idx = self.b().intern_global(koopa_name.clone());
                self.param_sig_names.insert(param.name.clone(), koopa_idx);
                if param.array_dims.is_empty() {
                    self.scopes.last_mut().unwrap().insert(param.name.clone(), IrSymbol::PtrArray(koopa_idx));
                    params.push((koopa_idx, IrType::Ptr(Box::new(IrType::I32))));
                } else {
                    let fixed_dims: Vec<i32> = param.array_dims.iter()
                        .map(|d| self.eval_const_global(d).unwrap_or(1))
                        .collect();
                    self.scopes.last_mut().unwrap().insert(param.name.clone(),
                        IrSymbol::NdParam { name: koopa_idx, dims: fixed_dims.clone() });
                    let inner = make_array_type(&fixed_dims);
                    params.push((koopa_idx, IrType::Ptr(Box::new(inner))));
                }
            } else {
                if self.globals.contains_key(&param.name) { self.mangle(&param.name); }
                let sig_name = self.mangle(&param.name);
                let koopa_name = self.mangle(&param.name);
                let sig_idx = self.b().intern_global(sig_name);
                let koopa_idx = self.b().intern_global(koopa_name);
                self.param_sig_names.insert(param.name.clone(), sig_idx);
                self.b().add_pending_alloca(koopa_idx, IrType::I32);
                self.b().emit_store(IrOperand::Global(sig_idx), IrOperand::Global(koopa_idx));
                self.scopes.last_mut().unwrap().insert(param.name.clone(), IrSymbol::Var(koopa_idx));
                params.push((sig_idx, IrType::I32));
            }
        }

        // First pass: find lib calls in body (like koopa_gen's gen_block_for_lib_decls)
        self.emit_lib_decls_for_body(&func.body)?;

        // Generate body
        self.gen_block(&func.body)?;

        // Ensure terminator
        if !self.b().is_terminated() {
            if self.current_ret_type == IrType::Void {
                self.b().emit_ret(None);
            } else {
                self.b().emit_ret(Some(IrOperand::Int(0)));
            }
        }

        // Build and merge (indices are already program-absolute, no rebase needed)
        let builder = self.builder.take().unwrap();
        let (mut ir_func, meta) = builder.build();
        ir_func.params = params;
        self.merge_meta(meta); // merge name tables into program
        self.program.funcs.push(ir_func);
        Ok(())
    }

    // ── Lib decl helpers ─────────────────────────────────────────────────────

    fn emit_lib_decls_for_body(&mut self, block: &Block) -> CompilerResult<()> {
        for item in &block.items {
            match item {
                BlockItem::Stmt(s) => self.emit_lib_decls_for_stmt(s)?,
                BlockItem::Decl(d) => self.emit_lib_decls_for_decl(d)?,
            }
        }
        Ok(())
    }

    fn emit_lib_decls_for_decl(&mut self, decl: &Decl) -> CompilerResult<()> {
        match decl {
            Decl::Const(defs) => {
                for def in defs { self.emit_lib_decl_for_expr(&def.init)?; }
            }
            Decl::Var(defs) => {
                for def in defs {
                    if let Some(init) = &def.init { self.emit_lib_decl_for_expr(init)?; }
                }
            }
        }
        Ok(())
    }

    fn emit_lib_decls_for_stmt(&mut self, stmt: &Stmt) -> CompilerResult<()> {
        match stmt {
            Stmt::Return(Some(e)) => self.emit_lib_decl_for_expr(e)?,
            Stmt::Assign { expr, .. } => self.emit_lib_decl_for_expr(expr)?,
            Stmt::Expr(e) => self.emit_lib_decl_for_expr(e)?,
            Stmt::Block(b) => self.emit_lib_decls_for_body(b)?,
            Stmt::If { cond, then_branch, else_branch } => {
                self.emit_lib_decl_for_expr(cond)?;
                self.emit_lib_decls_for_stmt(then_branch)?;
                if let Some(els) = else_branch { self.emit_lib_decls_for_stmt(els)?; }
            }
            Stmt::While { cond, body } => {
                self.emit_lib_decl_for_expr(cond)?;
                self.emit_lib_decls_for_stmt(body)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn emit_lib_decl_for_expr(&mut self, expr: &Expr) -> CompilerResult<()> {
        match expr {
            Expr::Call { name, .. } => {
                if is_lib_func(name) && !self.lib_funcs_emitted.contains(name.as_str()) {
                    self.lib_funcs_emitted.insert(name.clone());
                    // Use program directly (builder may not be active)
                    let func_idx = self.program.intern_func(name.clone());
                    let ret = lib_func_ret_type(name).unwrap_or(Type::Int);
                    let ret_type = if ret == Type::Int { IrType::I32 } else { IrType::Void };
                    let param_types = LIB_FUNCS.iter()
                        .find(|(n, _, _)| *n == name)
                        .map(|(_, _, pts)| pts.iter().map(|s| str_to_ir_type(s)).collect())
                        .unwrap_or_default();
                    // Avoid duplicate decls
                    if !self.program.func_decls.iter().any(|d| d.name == func_idx) {
                        self.program.func_decls.push(IrFuncDecl { name: func_idx, param_types, ret_type });
                    }
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.emit_lib_decl_for_expr(lhs.as_ref())?;
                self.emit_lib_decl_for_expr(rhs.as_ref())?;
            }
            Expr::Unary { expr: e, .. } => self.emit_lib_decl_for_expr(e.as_ref())?,
            Expr::Index { array, index } => {
                self.emit_lib_decl_for_expr(array.as_ref())?;
                self.emit_lib_decl_for_expr(index.as_ref())?;
            }
            Expr::InitList(items) => {
                for i in items { self.emit_lib_decl_for_expr(i)?; }
            }
            _ => {}
        }
        Ok(())
    }

    // ── Blocks and declarations ──────────────────────────────────────────────

    fn gen_block(&mut self, block: &Block) -> CompilerResult<()> {
        for item in &block.items {
            if self.b().is_terminated() { break; }
            match item {
                BlockItem::Stmt(s) => self.gen_stmt(s)?,
                BlockItem::Decl(d) => self.gen_decl(d)?,
            }
        }
        Ok(())
    }

    fn gen_decl(&mut self, decl: &Decl) -> CompilerResult<()> {
        match decl {
            Decl::Const(defs) => {
                for def in defs {
                    if self.current_scope_contains(&def.name) {
                        return Err(CompilerError::new(format!("redeclaration of '{}'", def.name)));
                    }
                    if def.dims.is_empty() && !matches!(&def.init, Expr::InitList(_)) {
                        let val = self.eval_const(&def.init)?;
                        self.scopes.last_mut().unwrap().insert(def.name.clone(), IrSymbol::Const(val));
                    } else {
                        if self.globals.contains_key(&def.name) { self.mangle(&def.name); }
                        let koopa_name = self.mangle(&def.name);
                        let dims: Vec<i32> = def.dims.iter()
                            .map(|d| self.eval_const(d))
                            .collect::<CompilerResult<_>>()?;
                        let array_type = make_array_type(&dims);
                        let base_idx = self.b().intern_global(koopa_name);
                        self.b().add_pending_alloca(base_idx, array_type);
                        self.scopes.last_mut().unwrap()
                            .insert(def.name.clone(), IrSymbol::Array(base_idx, dims.len()));
                        self.gen_array_init(base_idx, &dims, &def.init)?;
                    }
                }
            }
            Decl::Var(defs) => {
                for def in defs {
                    if self.current_scope_contains(&def.name) {
                        return Err(CompilerError::new(format!("redeclaration of '{}'", def.name)));
                    }
                    if self.globals.contains_key(&def.name) { self.mangle(&def.name); }
                    let koopa_name = self.mangle(&def.name);
                    if def.dims.is_empty() {
                        let base_idx = self.b().intern_global(koopa_name);
                        self.b().add_pending_alloca(base_idx, IrType::I32);
                        if let Some(init) = &def.init {
                            let val = self.gen_expr(init)?;
                            self.b().emit_store(val, IrOperand::Global(base_idx));
                        }
                        self.scopes.last_mut().unwrap().insert(def.name.clone(), IrSymbol::Var(base_idx));
                    } else {
                        let dims: Vec<i32> = def.dims.iter()
                            .map(|d| self.eval_const(d))
                            .collect::<CompilerResult<_>>()?;
                        let array_type = make_array_type(&dims);
                        let base_idx = self.b().intern_global(koopa_name);
                        self.b().add_pending_alloca(base_idx, array_type);
                        self.scopes.last_mut().unwrap()
                            .insert(def.name.clone(), IrSymbol::Array(base_idx, dims.len()));
                        if let Some(init) = &def.init {
                            self.gen_array_init(base_idx, &dims, init)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    // ── Statements ───────────────────────────────────────────────────────────

    fn gen_stmt(&mut self, stmt: &Stmt) -> CompilerResult<()> {
        match stmt {
            Stmt::Return(expr) => {
                let val = if let Some(e) = expr { Some(self.gen_expr(e)?) } else { None };
                self.b().emit_ret(val);
            }
            Stmt::Assign { name, index, expr } => {
                if index.is_empty() {
                    let dest_idx = match self.lookup(name) {
                        Some(IrSymbol::Var(v)) => Some(*v),
                        Some(IrSymbol::Const(_)) => return Err(CompilerError::new(format!("cannot assign to constant '{name}'"))),
                        None => return Err(CompilerError::new(format!("undefined variable '{name}'"))),
                        _ => return Err(CompilerError::new(format!("cannot assign to array '{name}' without index"))),
                    };
                    let val = self.gen_expr(expr)?;
                    self.b().emit_store(val, IrOperand::Global(dest_idx.unwrap()));
                } else {
                    self.gen_array_assign(name, index, expr)?;
                }
            }
            Stmt::Expr(expr) => { self.gen_expr(expr)?; }
            Stmt::Block(block) => {
                self.enter_scope();
                self.gen_block(block)?;
                self.exit_scope();
            }
            Stmt::If { cond, then_branch, else_branch } => {
                let cond_val = self.gen_expr(cond)?;
                let then_label = self.b().alloc_label();
                let else_label = self.b().alloc_label();
                let end_label = self.b().alloc_label();
                if else_branch.is_some() {
                    self.b().emit_br(cond_val, then_label, else_label);
                } else {
                    self.b().emit_br(cond_val, then_label, end_label);
                }
                self.b().start_block(then_label);
                self.gen_stmt(then_branch)?;
                let then_term = self.b().is_terminated();
                if !then_term { self.b().emit_jump(end_label); }
                let mut else_term = false;
                if let Some(els) = else_branch {
                    self.b().start_block(else_label);
                    self.gen_stmt(els)?;
                    else_term = self.b().is_terminated();
                    if !else_term { self.b().emit_jump(end_label); }
                }
                if else_branch.is_none() || !then_term || !else_term {
                    self.b().start_block(end_label);
                }
            }
            Stmt::While { cond, body } => {
                let entry_label = self.b().alloc_label();
                let body_label = self.b().alloc_label();
                let end_label = self.b().alloc_label();
                self.b().emit_jump(entry_label);
                self.b().start_block(entry_label);
                let cond_val = self.gen_expr(cond)?;
                self.b().emit_br(cond_val, body_label, end_label);
                self.loop_stack.push((entry_label, end_label));
                self.b().start_block(body_label);
                self.gen_stmt(body)?;
                if !self.b().is_terminated() { self.b().emit_jump(entry_label); }
                self.loop_stack.pop();
                self.b().start_block(end_label);
            }
            Stmt::Break => {
                let bl = *self.loop_stack.last()
                    .ok_or_else(|| CompilerError::new("'break' outside of loop"))?;
                self.b().emit_jump(bl.1);
            }
            Stmt::Continue => {
                let cl = *self.loop_stack.last()
                    .ok_or_else(|| CompilerError::new("'continue' outside of loop"))?;
                self.b().emit_jump(cl.0);
            }
            Stmt::Empty => {}
        }
        Ok(())
    }

    fn gen_array_assign(&mut self, name: &str, indices: &[Expr], expr: &Expr) -> CompilerResult<()> {
        // Extract lookup info before mutable operations
        let assign_info: Option<(usize, bool)> = match self.lookup(name) {
            Some(IrSymbol::NdParam { name: pn, .. }) => Some((*pn, false)),
            Some(IrSymbol::Array(n, _)) => Some((*n, false)),
            Some(IrSymbol::PtrArray(n)) => Some((*n, true)),
            _ => None,
        };
        let is_ndparam = matches!(self.lookup(name), Some(IrSymbol::NdParam { .. }));
        match assign_info {
            Some((base, _is_ptr)) if is_ndparam => {
                let val = self.gen_expr(expr)?;
                let first = self.gen_expr(&indices[0])?;
                let mut ptr = self.b().emit_getptr(IrOperand::Global(base), first);
                for idx in &indices[1..] {
                    let iv = self.gen_expr(idx)?;
                    ptr = self.b().emit_getelemptr(ptr, iv);
                }
                self.b().emit_store(val, ptr);
            }
            Some((base, is_ptr)) => {
                let val = self.gen_expr(expr)?;
                let mut ptr = IrOperand::Global(base);
                for idx in indices {
                    let iv = self.gen_expr(idx)?;
                    ptr = if is_ptr { self.b().emit_getptr(ptr, iv) }
                          else { let p0 = self.b().emit_getelemptr(ptr, IrOperand::Int(0)); self.b().emit_getptr(p0, iv) };
                }
                self.b().emit_store(val, ptr);
            }
            None => return Err(CompilerError::new(format!("'{name}' is not an array"))),
        }
        Ok(())
    }

    fn gen_expr_base(&mut self, expr: &Expr) -> CompilerResult<IrOperand> {
        match expr {
            Expr::LVal(name) => {
                let sym = self.lookup(name).cloned();
                match sym {
                    Some(IrSymbol::Var(n)) | Some(IrSymbol::Array(n, _)) | Some(IrSymbol::PtrArray(n)) => Ok(IrOperand::Global(n)),
                    Some(IrSymbol::NdParam { name, .. }) => Ok(IrOperand::Global(name)),
                    _ => Err(CompilerError::new(format!("'{name}' is not an lvalue"))),
                }
            },
            Expr::Index { array, index } => {
                let idx_val = self.gen_expr(index.as_ref())?;
                let base = self.gen_expr_base(array.as_ref())?;
                let p0 = self.b().emit_getelemptr(base, IrOperand::Int(0));
                Ok(self.b().emit_getptr(p0, idx_val))
            }
            _ => Err(CompilerError::new("not an lvalue")),
        }
    }

    fn is_ndparam_chain(&self, expr: &Expr) -> bool {
        // Walk through nested Index to find the base LVal
        let mut cur = expr;
        loop {
            match cur {
                Expr::Index { array, .. } => cur = array.as_ref(),
                Expr::LVal(name) => return matches!(self.lookup(name), Some(IrSymbol::NdParam { .. })),
                _ => return false,
            }
        }
    }

    fn gen_ndparam_index_full(&mut self, expr: &Expr) -> CompilerResult<IrOperand> {
        // Flatten all nested indices, reverse, and handle like koopa_gen's gen_ndparam_index
        let mut indices: Vec<&Expr> = Vec::new();
        let mut cur = expr;
        let base_name;
        let dims;
        loop {
            match cur {
                Expr::Index { array, index } => {
                    indices.push(index.as_ref());
                    cur = array.as_ref();
                }
                Expr::LVal(name) => {
                    if let Some(IrSymbol::NdParam { name: n, dims: d }) = self.lookup(name) {
                        base_name = *n;
                        dims = d.clone();
                    } else {
                        return Err(CompilerError::new("expected NdParam"));
                    }
                    break;
                }
                _ => return Err(CompilerError::new("invalid array access")),
            }
        }
        indices.reverse();
        let total_dims = 1 + dims.len();

        let first_idx = self.gen_expr(indices[0])?;
        let mut ptr = self.b().emit_getptr(IrOperand::Global(base_name), first_idx);
        for idx in &indices[1..] {
            let idx_val = self.gen_expr(idx)?;
            ptr = self.b().emit_getelemptr(ptr, idx_val);
        }
        if indices.len() >= total_dims {
            Ok(self.b().emit_load(ptr))
        } else {
            Ok(self.b().emit_getelemptr(ptr, IrOperand::Int(0)))
        }
    }

    // ── Expressions (eval) ───────────────────────────────────────────────────

    fn eval_const(&self, expr: &Expr) -> CompilerResult<i32> {
        match expr {
            Expr::Int(v) => Ok(*v),
            Expr::LVal(name) => match self.lookup(name) {
                Some(IrSymbol::Const(v)) => Ok(*v),
                _ => Err(CompilerError::new(format!("'{name}' is not a compile-time constant"))),
            },
            Expr::Unary { op, expr } => {
                let v = self.eval_const(expr)?;
                match op {
                    UnaryOp::Plus => Ok(v),
                    UnaryOp::Minus => Ok(v.wrapping_neg()),
                    UnaryOp::Not => Ok((v == 0) as i32),
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                let l = self.eval_const(lhs)?;
                let r = self.eval_const(rhs)?;
                Ok(match op {
                    BinaryOp::Mul => l.wrapping_mul(r),
                    BinaryOp::Div => l.checked_div(r).ok_or_else(|| CompilerError::new("div by zero"))?,
                    BinaryOp::Rem => l.checked_rem(r).ok_or_else(|| CompilerError::new("rem by zero"))?,
                    BinaryOp::Add => l.wrapping_add(r),
                    BinaryOp::Sub => l.wrapping_sub(r),
                    BinaryOp::Lt => (l < r) as i32,
                    BinaryOp::Gt => (l > r) as i32,
                    BinaryOp::Le => (l <= r) as i32,
                    BinaryOp::Ge => (l >= r) as i32,
                    BinaryOp::Eq => (l == r) as i32,
                    BinaryOp::Ne => (l != r) as i32,
                    BinaryOp::And => (l != 0 && r != 0) as i32,
                    BinaryOp::Or => (l != 0 || r != 0) as i32,
                })
            }
            _ => Err(CompilerError::new("not a compile-time constant")),
        }
    }

    fn eval_const_global(&self, expr: &Expr) -> CompilerResult<i32> { self.eval_const(expr) }

    fn gen_expr(&mut self, expr: &Expr) -> CompilerResult<IrOperand> {
        match expr {
            Expr::Int(n) => Ok(IrOperand::Int(*n)),
            Expr::LVal(name) => {
                let sym = self.lookup(name).cloned();
                match sym {
                    Some(IrSymbol::Const(v)) => Ok(IrOperand::Int(v)),
                    Some(IrSymbol::Var(koopa_idx)) => Ok(self.b().emit_load(IrOperand::Global(koopa_idx))),
                    Some(IrSymbol::Array(n, _)) => Ok(self.b().emit_getelemptr(IrOperand::Global(n), IrOperand::Int(0))),
                    Some(IrSymbol::PtrArray(n)) => Ok(IrOperand::Global(n)),
                    Some(IrSymbol::NdParam { name, .. }) => Ok(IrOperand::Global(name)),
                    None => Err(CompilerError::new(format!("undefined identifier '{name}'"))),
                }
            },
            Expr::Unary { op, expr } => {
                let val = self.gen_expr(expr)?;
                match op {
                    UnaryOp::Plus => Ok(val),
                    UnaryOp::Minus => Ok(self.b().emit_arith(IrArithOp::Sub, IrOperand::Int(0), val)),
                    UnaryOp::Not => Ok(self.b().emit_icmp(IrCmpOp::Eq, val, IrOperand::Int(0))),
                }
            }
            Expr::Binary { op, lhs, rhs } => match op {
                BinaryOp::And => {
                    let sc = self.b().alloc_sc();
                    self.b().add_pending_alloca(sc, IrType::I32);
                    let lv = self.as_br_cond(lhs)?;
                    let rhs_l = self.b().alloc_label();
                    let false_l = self.b().alloc_label();
                    let end_l = self.b().alloc_label();
                    self.b().emit_br(lv, rhs_l, false_l);
                    self.b().start_block(rhs_l);
                    let rv = self.gen_expr(rhs)?;
                    let t = self.b().emit_icmp(IrCmpOp::Ne, rv, IrOperand::Int(0));
                    self.b().emit_store(t, IrOperand::Global(sc));
                    self.b().emit_jump(end_l);
                    self.b().start_block(false_l);
                    self.b().emit_store(IrOperand::Int(0), IrOperand::Global(sc));
                    self.b().emit_jump(end_l);
                    self.b().start_block(end_l);
                    Ok(self.b().emit_load(IrOperand::Global(sc)))
                }
                BinaryOp::Or => {
                    let sc = self.b().alloc_sc();
                    self.b().add_pending_alloca(sc, IrType::I32);
                    let lv = self.as_br_cond(lhs)?;
                    let true_l = self.b().alloc_label();
                    let rhs_l = self.b().alloc_label();
                    let end_l = self.b().alloc_label();
                    self.b().emit_br(lv, true_l, rhs_l);
                    self.b().start_block(true_l);
                    self.b().emit_store(IrOperand::Int(1), IrOperand::Global(sc));
                    self.b().emit_jump(end_l);
                    self.b().start_block(rhs_l);
                    let rv = self.gen_expr(rhs)?;
                    let t = self.b().emit_icmp(IrCmpOp::Ne, rv, IrOperand::Int(0));
                    self.b().emit_store(t, IrOperand::Global(sc));
                    self.b().emit_jump(end_l);
                    self.b().start_block(end_l);
                    Ok(self.b().emit_load(IrOperand::Global(sc)))
                }
                _ => {
                    let lv = self.gen_expr(lhs)?;
                    let rv = self.gen_expr(rhs)?;
                    Ok(match op {
                        BinaryOp::Add => self.b().emit_arith(IrArithOp::Add, lv, rv),
                        BinaryOp::Sub => self.b().emit_arith(IrArithOp::Sub, lv, rv),
                        BinaryOp::Mul => self.b().emit_arith(IrArithOp::Mul, lv, rv),
                        BinaryOp::Div => self.b().emit_arith(IrArithOp::Div, lv, rv),
                        BinaryOp::Rem => self.b().emit_arith(IrArithOp::Mod, lv, rv),
                        BinaryOp::Lt => self.b().emit_icmp(IrCmpOp::Lt, lv, rv),
                        BinaryOp::Gt => self.b().emit_icmp(IrCmpOp::Gt, lv, rv),
                        BinaryOp::Le => self.b().emit_icmp(IrCmpOp::Le, lv, rv),
                        BinaryOp::Ge => self.b().emit_icmp(IrCmpOp::Ge, lv, rv),
                        BinaryOp::Eq => self.b().emit_icmp(IrCmpOp::Eq, lv, rv),
                        BinaryOp::Ne => self.b().emit_icmp(IrCmpOp::Ne, lv, rv),
                        _ => unreachable!(),
                    })
                }
            },
            Expr::Call { name, args } => {
                let mut ir_args = Vec::new();
                for a in args { ir_args.push(self.gen_expr(a)?); }
                // Use program-level func index (not builder)
                let func_idx = self.program.intern_func(name.clone());
                let ret_ty = lib_func_ret_type(name)
                    .or_else(|| self.func_ret_types.get(name).copied())
                    .unwrap_or(Type::Int);
                Ok(self.b().emit_call(func_idx, ir_args, ret_ty != Type::Void).unwrap_or(IrOperand::Int(0)))
            }
            Expr::Index { array, index } => {
                // Check for NdParam chain — handle with flattened approach
                if self.is_ndparam_chain(expr) {
                    return self.gen_ndparam_index_full(expr);
                }
                // Check if base is PtrArray (already a pointer)
                let is_ptr = match array.as_ref() {
                    Expr::LVal(name) => matches!(self.lookup(name), Some(IrSymbol::PtrArray(_))),
                    _ => false,
                };
                let idx_val = self.gen_expr(index)?;
                let base = self.gen_expr_base(array)?;
                let elem_ptr = if is_ptr {
                    self.b().emit_getptr(base, idx_val)
                } else {
                    let p0 = self.b().emit_getelemptr(base, IrOperand::Int(0));
                    self.b().emit_getptr(p0, idx_val)
                };
                let num_indices = 1 + count_indices(array);
                let total_dims = self.get_total_dims_for(array);
                if num_indices >= total_dims {
                    Ok(self.b().emit_load(elem_ptr))
                } else {
                    Ok(self.b().emit_getelemptr(elem_ptr, IrOperand::Int(0)))
                }
            }
            Expr::InitList(_) => Err(CompilerError::new("init list not allowed here")),
        }
    }

    fn as_br_cond(&mut self, expr: &Expr) -> CompilerResult<IrOperand> {
        let val = self.gen_expr(expr)?;
        match val {
            IrOperand::Int(n) => Ok(IrOperand::Int((n != 0) as i32)),
            _ => Ok(self.b().emit_icmp(IrCmpOp::Ne, val, IrOperand::Int(0))),
        }
    }

    fn gen_index_expr(&mut self, array: &Expr, index: &Expr) -> CompilerResult<IrOperand> {
        // Count total indices and total dimensions for load-vs-decay decision
        let num_indices = 1 + count_indices(array);
        let total_dims = self.get_total_dims_for(array);

        match array {
            Expr::LVal(name) => {
                let idx_info: Option<(usize, bool, Option<Vec<i32>>)> = match self.lookup(name) {
                    Some(IrSymbol::NdParam { name: pn, dims }) => Some((*pn, false, Some(dims.clone()))),
                    Some(IrSymbol::Array(n, _)) => Some((*n, false, None)),
                    Some(IrSymbol::PtrArray(n)) => Some((*n, true, None)),
                    _ => None,
                };
                match idx_info {
                    Some((base, _, Some(_dims))) => {
                        // NdParam: just getptr for the dynamic index. Fixed dims
                        // are handled by nested Index handlers (getelemptr+getptr).
                        let iv = self.gen_expr(index)?;
                        let elem_ptr = self.b().emit_getptr(IrOperand::Global(base), iv);
                        if num_indices >= total_dims {
                            Ok(self.b().emit_load(elem_ptr))
                        } else {
                            Ok(self.b().emit_getelemptr(elem_ptr, IrOperand::Int(0)))
                        }
                    }
                    Some((base, is_ptr, _)) => {
                        let iv = self.gen_expr(index)?;
                        let elem_ptr = if is_ptr {
                            self.b().emit_getptr(IrOperand::Global(base), iv)
                        } else {
                            let p0 = self.b().emit_getelemptr(IrOperand::Global(base), IrOperand::Int(0));
                            self.b().emit_getptr(p0, iv)
                        };
                        if num_indices >= total_dims {
                            Ok(self.b().emit_load(elem_ptr))
                        } else {
                            Ok(self.b().emit_getelemptr(elem_ptr, IrOperand::Int(0)))
                        }
                    }
                    None => Err(CompilerError::new(format!("'{name}' is not an array"))),
                }
            }
            Expr::Index { array: inner, index: inner_idx } => {
                let base_ptr = self.gen_index_expr(inner.as_ref(), inner_idx.as_ref())?;
                let iv = self.gen_expr(index)?;
                // Each non-NdParam dim: getelemptr→decay, then getptr→index
                let decay = self.b().emit_getelemptr(base_ptr, IrOperand::Int(0));
                let elem_ptr = self.b().emit_getptr(decay, iv);
                if num_indices >= total_dims {
                    Ok(self.b().emit_load(elem_ptr))
                } else {
                    Ok(self.b().emit_getelemptr(elem_ptr, IrOperand::Int(0)))
                }
            }
            _ => Err(CompilerError::new("invalid array access")),
        }
    }

    // ── Array initialization ─────────────────────────────────────────────────

    fn global_init_vals(&self, dims: &[i32], init: &Expr) -> IrGlobalInit {
        let total: usize = dims.iter().map(|&d| d as usize).product();
        let mut flat: Vec<Expr> = Vec::new();
        Self::flatten_init_static(dims, init, &mut flat);
        let mut vals: Vec<i32> = Vec::new();
        for e in &flat {
            if let Expr::Int(n) = e { vals.push(*n); } else { vals.push(0); }
        }
        while vals.len() < total { vals.push(0); }
        IrGlobalInit::Values(vals)
    }

    fn flatten_init_static(dims: &[i32], init: &Expr, flat: &mut Vec<Expr>) {
        if dims.is_empty() { flat.push(init.clone()); return; }
        let inner_dim = *dims.last().unwrap_or(&1) as usize;
        match init {
            Expr::InitList(items) => {
                let start = flat.len();
                for item in items {
                    if let Expr::InitList(_) = item {
                        let rem = (inner_dim - (flat.len() - start) % inner_dim) % inner_dim;
                        for _ in 0..rem { flat.push(Expr::Int(0)); }
                        if dims.len() > 1 {
                            Self::flatten_init_static(&dims[1..], item, flat);
                        }
                    } else {
                        flat.push(item.clone());
                    }
                }
                let total: usize = dims.iter().map(|&d| d as usize).product();
                while flat.len() < start + total { flat.push(Expr::Int(0)); }
            }
            _ => { flat.push(init.clone()); }
        }
    }

    fn gen_array_init(&mut self, base_idx: usize, dims: &[i32], init: &Expr) -> CompilerResult<()> {
        let mut flat_vals: Vec<i32> = Vec::new();
        Self::flatten_init_vals(dims, init, &mut flat_vals);
        let total: usize = dims.iter().map(|&d| d as usize).product();
        while flat_vals.len() < total { flat_vals.push(0); }
        for (i, val) in flat_vals.iter().enumerate().take(total) {
            let ptr = self.emit_array_elem_ptr(base_idx, dims, i as i32);
            self.b().emit_store(IrOperand::Int(*val), ptr);
        }
        Ok(())
    }

    fn flatten_init_vals(dims: &[i32], init: &Expr, vals: &mut Vec<i32>) {
        if dims.is_empty() {
            if let Expr::Int(n) = init { vals.push(*n); }
            return;
        }
        let start_len = vals.len();
        let total: usize = dims.iter().map(|&d| d as usize).product();
        let inner_dim = *dims.last().unwrap_or(&1) as usize;
        match init {
            Expr::InitList(items) => {
                for item in items {
                    match item {
                        Expr::InitList(_) => {
                            let rem = (inner_dim - (vals.len() - start_len) % inner_dim) % inner_dim;
                            for _ in 0..rem { vals.push(0); }
                            let sub_dims: &[i32] = if dims.len() > 1 { &dims[1..] } else { &[] };
                            let sub_start = vals.len();
                            Self::flatten_init_vals(sub_dims, item, vals);
                            let sub_total: usize = sub_dims.iter().map(|&d| d as usize).product();
                            let target = sub_start + if sub_dims.is_empty() { inner_dim } else { sub_total };
                            while vals.len() < target { vals.push(0); }
                        }
                        Expr::Int(n) => vals.push(*n),
                        _ => {}
                    }
                }
                while vals.len() < start_len + total { vals.push(0); }
            }
            Expr::Int(n) => { vals.push(*n); while vals.len() < start_len + total { vals.push(0); } }
            _ => {}
        }
    }

    fn emit_array_elem_ptr(&mut self, base_idx: usize, dims: &[i32], flat_idx: i32) -> IrOperand {
        // For multi-dimensional arrays, we need to chain getelemptr/getptr
        // to go from the array pointer down to *i32.
        if dims.is_empty() {
            return IrOperand::Global(base_idx);
        }
        // First: getelemptr @base, 0 — get pointer to first element of outer array
        let mut ptr = self.b().emit_getelemptr(IrOperand::Global(base_idx), IrOperand::Int(0));
        // For each dimension (except the innermost), we need another getelemptr to
        // go inside. The innermost dimension elements are i32, so we just need
        // getptr to offset within that inner array.
        // Actually: for an array [T, N], getelemptr gives *T. If T is still an array,
        // we need getelemptr again. If T is i32, we use getptr for the offset.
        for _ in 1..dims.len() {
            ptr = self.b().emit_getelemptr(ptr, IrOperand::Int(0));
        }
        // Now ptr is *i32. Use getptr to offset by flat_idx.
        self.b().emit_getptr(ptr, IrOperand::Int(flat_idx))
    }
}

fn count_indices(expr: &Expr) -> usize {
    match expr {
        Expr::Index { array, .. } => 1 + count_indices(array.as_ref()),
        _ => 0,
    }
}

impl AstToIr {
    fn get_total_dims_for(&self, expr: &Expr) -> usize {
        match expr {
            Expr::LVal(name) => match self.lookup(name) {
                Some(IrSymbol::Array(_, ndims)) => *ndims,
                Some(IrSymbol::PtrArray(_)) => 1,
                Some(IrSymbol::NdParam { dims, .. }) => 1 + dims.len(),
                _ => 1,
            },
            Expr::Index { array, .. } => self.get_total_dims_for(array.as_ref()),
            _ => 1,
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn str_to_ir_type(s: &str) -> IrType {
    match s {
        "i32" => IrType::I32,
        "*i32" => IrType::Ptr(Box::new(IrType::I32)),
        _ => IrType::I32,
    }
}

fn make_array_type(dims: &[i32]) -> IrType {
    let mut ty = IrType::I32;
    for &d in dims.iter().rev() {
        ty = IrType::Array(Box::new(ty), d as u32);
    }
    ty
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer;
    use crate::parser;
    use crate::ir_to_koopa;

    fn compile_via_ir(source: &str) -> CompilerResult<String> {
        let tokens = lexer::tokenize(source)?;
        let ast = parser::parse(tokens)?;
        let ir = AstToIr::new().gen_program(&ast)?;
        Ok(ir_to_koopa::emit_koopa(&ir))
    }

    #[test]
    fn simple_main() {
        let source = "int main() { return 1 + 2 * -3; }";
        let out = compile_via_ir(source).unwrap();
        assert!(out.contains("fun @main(): i32 {"), "missing main header: {out}");
        assert!(out.contains("%0 = sub 0, 3"), "missing sub: {out}");
        assert!(out.contains("%1 = mul 2, %0"), "missing mul: {out}");
        assert!(out.contains("%2 = add 1, %1"), "missing add: {out}");
        assert!(out.contains("ret %2"), "missing ret: {out}");
    }

    #[test]
    fn hex_and_octal() {
        let source = "int main() { return 0x10 + 07; }";
        let out = compile_via_ir(source).unwrap();
        // 0x10=16, 07=7, 16+7=23
        assert!(out.contains("23") || out.contains("add 16, 7"));
    }

    #[test]
    fn var_decl_and_assign() {
        let source = "int main() { int x = 42; return x; }";
        let out = compile_via_ir(source).unwrap();
        assert!(out.contains("alloc i32"), "should have alloc: {out}");
        assert!(out.contains("store 42"), "should store 42: {out}");
        assert!(out.contains("load"), "should load x: {out}");
    }

    #[test]
    fn short_circuit_and() {
        let source = "int main() { int a = 1; int b = 0; return a && b; }";
        let out = compile_via_ir(source).unwrap();
        assert!(out.contains("br"), "should have branch: {out}");
    }

    #[test]
    fn if_stmt() {
        let source = "int main() { int x = 1; if (x) { x = 2; } return x; }";
        let out = compile_via_ir(source).unwrap();
        assert!(out.contains("br"), "should have branch: {out}");
    }
}
