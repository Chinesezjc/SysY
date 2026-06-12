use std::collections::{HashMap, HashSet};

use crate::ast::{BinaryOp, Block, BlockItem, CompUnit, Decl, Expr, GlobalItem, Stmt, Type, UnaryOp};
use crate::error::{CompilerError, CompilerResult};
use crate::codegen::{LIB_FUNCS, is_lib_func, lib_func_ret_type};

// ── Koopa IR ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub(crate) enum Symbol {
    Const(i32),
    Var(String),
    Array(String, usize),
    PtrArray(String),
    NdParam { name: String, dims: Vec<i32> },
}

pub(crate) struct KoopaGen {
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
    pending_sc_allocas: Vec<String>,
    block_terminated: bool,
    param_sig_names: HashMap<String, String>,
}

impl KoopaGen {
    pub(crate) fn new() -> Self {
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
            pending_sc_allocas: Vec::new(),
            block_terminated: false,
            param_sig_names: HashMap::new(),
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
        self.pending_sc_allocas.push(format!("{t} = alloc i32"));
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
        self.block_terminated = false;
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
        if let Some((_, ret_ty, param_types)) = LIB_FUNCS.iter().find(|(n, _, _)| *n == name) {
            let params_str = if param_types.is_empty() {
                String::new()
            } else {
                param_types.join(", ")
            };
            let ret_str = match ret_ty {
                Type::Int => ": i32",
                Type::Void => "",
            };
            self.emit_decl(&format!("decl @{name}({params_str}){ret_str}"));
        }
    }

    fn type_str(t: Type) -> &'static str {
        match t {
            Type::Int => "i32",
            Type::Void => "",
        }
    }

    fn array_type_str(dims: &[i32]) -> String {
        let mut result = "i32".to_string();
        for &dim in dims.iter().rev() {
            result = format!("[{result}, {dim}]");
        }
        result
    }

    fn global_init_string(&self, dims: &[i32], init: &Expr) -> String {
        let mut flat = Vec::new();
        Self::flatten_init(dims, init, &mut flat);
        let total: usize = dims.iter().map(|&d| d as usize).product();
        while flat.len() < total {
            flat.push(Expr::Int(0));
        }
        let flat_vals: Vec<i32> = flat.iter()
            .map(|e| self.eval_const(e).unwrap_or(0))
            .collect();
        let mut start = 0usize;
        Self::build_init_str(dims, &flat_vals, &mut start)
    }

    fn flatten_init(dims: &[i32], init: &Expr, flat: &mut Vec<Expr>) {
        let total: usize = dims.iter().map(|&d| d as usize).product();
        match init {
            Expr::InitList(items) => {
                if items.is_empty() { return; }
                let has_nested = items.iter().any(|i| matches!(i, Expr::InitList(_)));
                if dims.len() <= 1 || !has_nested {
                    for item in items {
                        match item {
                            Expr::InitList(_) => Self::flatten_init(&[], item, flat),
                            _ => flat.push(item.clone()),
                        }
                    }
                } else {
                    let sub_dims = &dims[1..];
                    let sub_size: usize = sub_dims.iter().map(|&d| d as usize).product();
                    for item in items {
                        let before = flat.len();
                        match item {
                            Expr::InitList(_) => Self::flatten_init(sub_dims, item, flat),
                            _ => { flat.push(item.clone()); }
                        }
                        let after = flat.len();
                        let remainder = (sub_size - (after - before) % sub_size) % sub_size;
                        for _ in 0..remainder { flat.push(Expr::Int(0)); }
                    }
                }
            }
            _ => { flat.push(init.clone()); }
        }
        while flat.len() < total { flat.push(Expr::Int(0)); }
    }

    fn build_init_str(dims: &[i32], flat: &[i32], start: &mut usize) -> String {
        if dims.is_empty() {
            let v = flat.get(*start).copied().unwrap_or(0);
            *start += 1;
            return v.to_string();
        }
        let count = dims[0] as usize;
        let sub_dims = &dims[1..];
        let mut parts = Vec::new();
        for _ in 0..count {
            parts.push(Self::build_init_str(sub_dims, flat, start));
        }
        format!("{{{}}}", parts.join(", "))
    }

    fn gen_array_init(&mut self, base: &str, dims: &[i32], init: &Expr) -> CompilerResult<()> {
        let mut flat = Vec::new();
        Self::flatten_init(dims, init, &mut flat);
        let total: usize = dims.iter().map(|&d| d as usize).product();
        while flat.len() < total { flat.push(Expr::Int(0)); }
        for (i, item) in flat.iter().enumerate().take(total) {
            let val = self.gen_expr(item)?;
            self.gen_array_store(base, dims, i as i32, &val);
        }
        Ok(())
    }

    fn gen_array_store(&mut self, base_ptr: &str, dims: &[i32], flat_idx: i32, val: &str) {
        if dims.len() == 1 {
            let p0 = self.alloc_tmp();
            self.emit(&format!("{p0} = getelemptr {base_ptr}, 0"));
            let p1 = self.alloc_tmp();
            self.emit(&format!("{p1} = getptr {p0}, {flat_idx}"));
            self.emit(&format!("store {val}, {p1}"));
        } else {
            let sub_size: i32 = dims[1..].iter().product();
            let outer_idx = flat_idx / sub_size;
            let inner_idx = flat_idx % sub_size;
            let p0 = self.alloc_tmp();
            self.emit(&format!("{p0} = getelemptr {base_ptr}, 0"));
            let p1 = self.alloc_tmp();
            self.emit(&format!("{p1} = getptr {p0}, {outer_idx}"));
            self.gen_array_store(&p1, &dims[1..], inner_idx, val);
        }
    }

    pub(crate) fn gen_program(mut self, program: &CompUnit) -> CompilerResult<String> {
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
                                if def.dims.is_empty() && !matches!(&def.init, Expr::InitList(_)) {
                                    let val = self.eval_const(&def.init)?;
                                    self.globals.insert(def.name.clone(), Symbol::Const(val));
                                } else {
                                    // Const array: allocate in global data
                                    let label = def.name.clone();
                                    let dims: Vec<i32> = def.dims.iter()
                                        .map(|d| self.eval_const(d))
                                        .collect::<CompilerResult<_>>()?;
                                    let array_type = Self::array_type_str(&dims);
                                    let init_str = self.global_init_string(&dims, &def.init);
                                    self.global_decls.push_str(&format!(
                                        "global @{} = alloc {}, {}\n", label, array_type, init_str
                                    ));
                                    self.globals.insert(label.clone(), Symbol::Array(label, dims.len()));
                                }
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
                                    let dims: Vec<i32> = def.dims.iter()
                                        .map(|d| self.eval_const(d))
                                        .collect::<CompilerResult<_>>()?;
                                    let array_type = Self::array_type_str(&dims);
                                    let init_str = if let Some(init) = &def.init {
                                        self.global_init_string(&dims, init)
                                    } else {
                                        "zeroinit".to_string()
                                    };
                                    self.global_decls.push_str(&format!(
                                        "global @{} = alloc {}, {}\n", def.name, array_type, init_str
                                    ));
                                    self.globals.insert(def.name.clone(), Symbol::Array(def.name.clone(), dims.len()));
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
                    self.block_terminated = false;

                    // Emit lib decls for called functions
                    self.gen_block_for_lib_decls(&func.body)?;

                    // Add function params to scope
                    for param in &func.params {
                        if param.is_array {
                            if self.globals.contains_key(&param.name) {
                                self.mangle(&param.name);
                            }
                            let koopa_name = self.mangle(&param.name);
                            self.param_sig_names.insert(param.name.clone(), koopa_name.clone());
                            if param.array_dims.is_empty() {
                                self.scopes.last_mut().unwrap()
                                    .insert(param.name.clone(), Symbol::PtrArray(koopa_name));
                            } else {
                                let fixed_dims: Vec<i32> = param.array_dims.iter()
                                    .map(|d| self.eval_const(d).unwrap_or(1))
                                    .collect();
                                self.scopes.last_mut().unwrap()
                                    .insert(param.name.clone(), Symbol::NdParam {
                                        name: koopa_name,
                                        dims: fixed_dims,
                                    });
                            }
                        } else {
                            if self.globals.contains_key(&param.name) {
                                self.mangle(&param.name);
                            }
                            let sig_name = self.mangle(&param.name);
                            let koopa_name = self.mangle(&param.name);
                            self.pending_sc_allocas
                                .push(format!("@{koopa_name} = alloc i32"));
                            self.emit(&format!(
                                "store @{}, @{}",
                                sig_name, koopa_name
                            ));
                            self.scopes
                                .last_mut()
                                .unwrap()
                                .insert(param.name.clone(), Symbol::Var(koopa_name));
                            self.param_sig_names.insert(param.name.clone(), sig_name);
                        }
                    }

                    self.gen_block(&func.body)?;

                    // Ensure void functions have a ret at end
                    if func.ret_type == Type::Void && !self.block_terminated {
                        self.emit("ret");
                        self.block_terminated = true;
                    }

                    // Emit pending allocas in entry block
                    let mut entry_allocas = String::new();
                    for inst in &self.pending_sc_allocas {
                        entry_allocas.push_str(&format!("  {inst}\n"));
                    }
                    self.body = entry_allocas + &self.body;
                    self.pending_sc_allocas.clear();

                    let params_str: Vec<String> = func
                        .params
                        .iter()
                        .map(|p| {
                            if p.is_array {
                                let sig_name = self.param_sig_names.get(&p.name)
                                    .cloned()
                                    .unwrap_or_else(|| p.name.clone());
                                if p.array_dims.is_empty() {
                                    format!("@{sig_name}: *i32")
                                } else {
                                    let eval_dims: Vec<i32> = p.array_dims.iter()
                                        .map(|d| self.eval_const(d).unwrap_or(1))
                                        .collect();
                                    let inner = Self::array_type_str(&eval_dims);
                                    format!("@{sig_name}: *{}", inner)
                                }
                            } else {
                                let sig_name = self.param_sig_names.get(&p.name)
                                    .cloned()
                                    .unwrap_or_else(|| p.name.clone());
                                format!("@{sig_name}: i32")
                            }
                        })
                        .collect();
                    let params_sig = if params_str.is_empty() {
                        String::new()
                    } else {
                        params_str.join(", ")
                    };

                    let ret_str = Self::type_str(func.ret_type);
                    let header = format!("fun @{}", func.name);
                    let ret_part = if ret_str.is_empty() {
                        String::new()
                    } else {
                        format!(": {ret_str}")
                    };
                    let header = if params_sig.is_empty() {
                        format!("{header}(){ret_part} {{\n%entry:")
                    } else {
                        format!("{header}({params_sig}){ret_part} {{\n%entry:")
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
            Expr::InitList(items) => {
                for item in items {
                    self.find_calls_in_expr(item);
                }
            }
            _ => {}
        }
    }

    fn gen_block(&mut self, block: &Block) -> CompilerResult<()> {
        for item in &block.items {
            if self.block_terminated {
                break;
            }
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
                    if def.dims.is_empty() && !matches!(&def.init, Expr::InitList(_)) {
                        let val = self.eval_const(&def.init)?;
                        self.scopes
                            .last_mut()
                            .unwrap()
                            .insert(def.name.clone(), Symbol::Const(val));
                    } else {
                        // Const array: allocate and initialize like var array
                        if self.globals.contains_key(&def.name) {
                            self.mangle(&def.name);
                        }
                        let koopa_name = self.mangle(&def.name);
                        let dims: Vec<i32> = def.dims.iter()
                            .map(|d| self.eval_const(d))
                            .collect::<CompilerResult<_>>()?;
                        let array_type = Self::array_type_str(&dims);
                        let base = format!("@{koopa_name}");
                        self.pending_sc_allocas.push(format!(
                            "{base} = alloc {}",
                            array_type
                        ));
                        self.scopes
                            .last_mut()
                            .unwrap()
                            .insert(def.name.clone(), Symbol::Array(koopa_name, dims.len()));
                        self.gen_array_init(&base, &dims, &def.init)?;
                    }
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
                    // Reserve base name if local shadows a global
                    if self.globals.contains_key(&def.name) {
                        self.mangle(&def.name);
                    }
                    let koopa_name = self.mangle(&def.name);
                    if def.dims.is_empty() {
                        self.pending_sc_allocas
                            .push(format!("@{koopa_name} = alloc i32"));
                        if let Some(init) = &def.init {
                            let val = self.gen_expr(init)?;
                            self.emit(&format!("store {val}, @{koopa_name}"));
                        }
                        self.scopes
                            .last_mut()
                            .unwrap()
                            .insert(def.name.clone(), Symbol::Var(koopa_name));
                    } else {
                        let dims: Vec<i32> = def.dims.iter()
                            .map(|d| self.eval_const(d))
                            .collect::<CompilerResult<_>>()?;
                        let array_type = Self::array_type_str(&dims);
                        let base = format!("@{koopa_name}");
                        self.pending_sc_allocas.push(format!(
                            "{base} = alloc {}",
                            array_type
                        ));
                        self.scopes
                            .last_mut()
                            .unwrap()
                            .insert(def.name.clone(), Symbol::Array(koopa_name, dims.len()));
                        if let Some(init) = &def.init {
                            self.gen_array_init(&base, &dims, init)?;
                        }
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
                self.block_terminated = true;
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
                    match self.lookup(name) {
                        Some(Symbol::NdParam { name: param_name, dims }) => {
                            let n = param_name.clone();
                            let dims = dims.clone();
                            let val = self.gen_expr(expr)?;
                            // Spec: first index → getptr, remaining → getelemptr
                            let first_idx = self.gen_expr(&index[0])?;
                            let p = self.alloc_tmp();
                            self.emit(&format!("{p} = getptr @{n}, {first_idx}"));
                            let mut ptr = p;
                            for (i, idx) in index.iter().enumerate().skip(1) {
                                let idx_val = self.gen_expr(idx)?;
                                let p1 = self.alloc_tmp();
                                self.emit(&format!("{p1} = getelemptr {ptr}, {idx_val}"));
                                ptr = p1;
                            }
                            self.emit(&format!("store {val}, {ptr}"));
                        }
                        _ => {
                            let (koopa_name, is_ptr_array) = match self.lookup(name) {
                                Some(Symbol::Array(n, _)) => (n.clone(), false),
                                Some(Symbol::PtrArray(n)) => (n.clone(), true),
                                _ => {
                                    return Err(CompilerError::new(format!(
                                        "'{name}' is not an array"
                                    )));
                                }
                            };
                            let val = self.gen_expr(expr)?;
                            let mut ptr = format!("@{}", koopa_name);
                            for idx in index {
                                let idx_val = self.gen_expr(idx)?;
                                if is_ptr_array {
                                    let p = self.alloc_tmp();
                                    self.emit(&format!("{p} = getptr {ptr}, {idx_val}"));
                                    ptr = p;
                                } else {
                                    let p0 = self.alloc_tmp();
                                    self.emit(&format!("{p0} = getelemptr {ptr}, 0"));
                                    let p1 = self.alloc_tmp();
                                    self.emit(&format!("{p1} = getptr {p0}, {idx_val}"));
                                    ptr = p1;
                                }
                            }
                            self.emit(&format!("store {val}, {ptr}"));
                        }
                    }
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
                self.block_terminated = true;
                self.emit_label(&then_label);
                self.gen_stmt(then_branch)?;
                let then_terminated = self.block_terminated;
                if !then_terminated {
                    self.emit(&format!("jump {end_label}"));
                    self.block_terminated = true;
                }
                let mut else_terminated = false;
                if let Some(else_s) = else_branch {
                    self.emit_label(&else_label);
                    self.gen_stmt(else_s)?;
                    else_terminated = self.block_terminated;
                    if !else_terminated {
                        self.emit(&format!("jump {end_label}"));
                        self.block_terminated = true;
                    }
                }
                // Emit end_label only if some path can reach it
                let need_end = else_branch.is_none() || !then_terminated || !else_terminated;
                if need_end {
                    self.emit_label(&end_label);
                }
            }
            Stmt::While { cond, body } => {
                let entry_label = self.new_label();
                let body_label = self.new_label();
                let end_label = self.new_label();
                self.emit(&format!("jump {entry_label}"));
                self.block_terminated = true;
                self.emit_label(&entry_label);
                let cond_val = self.gen_expr(cond)?;
                self.emit(&format!("br {cond_val}, {body_label}, {end_label}"));
                self.block_terminated = true;
                self.loop_stack
                    .push((entry_label.clone(), end_label.clone()));
                self.emit_label(&body_label);
                self.gen_stmt(body)?;
                if !self.block_terminated {
                    self.emit(&format!("jump {entry_label}"));
                    self.block_terminated = true;
                }
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
                self.block_terminated = true;
            }
            Stmt::Continue => {
                let (continue_label, _) = self
                    .loop_stack
                    .last()
                    .ok_or_else(|| CompilerError::new("'continue' outside of loop"))?;
                let label = continue_label.clone();
                self.emit(&format!("jump {label}"));
                self.block_terminated = true;
            }
            Stmt::Empty => {}
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
            Expr::InitList(_) => Err(CompilerError::new(
                "initializer list is not a compile-time constant",
            )),
        }
    }

    fn as_br_cond(&mut self, expr: &Expr) -> CompilerResult<String> {
        let val = self.gen_expr(expr)?;
        if val.starts_with('%') || val.starts_with('@') {
            Ok(val)
        } else {
            let t = self.alloc_tmp();
            self.emit(&format!("{t} = ne {val}, 0"));
            Ok(t)
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
                    Some(Symbol::Array(n, _)) => {
                        let n = n.clone();
                        let tmp = self.alloc_tmp();
                        self.emit(&format!("{tmp} = getelemptr @{n}, 0"));
                        Ok(tmp)
                    }
                    Some(Symbol::PtrArray(n)) => {
                        Ok(format!("@{}", n))
                    }
                    Some(Symbol::NdParam { name, .. }) => {
                        Ok(format!("@{}", name))
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
                    let lv = self.as_br_cond(lhs)?;
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
                    let lv = self.as_br_cond(lhs)?;
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
                // Check for NdParam — handle with gen_ndparam_index
                if let Some(_dims) = self.get_ndparam_dims(expr) {
                    return self.gen_ndparam_index(expr);
                }
                // Check if base is a PtrArray (already a pointer, no getelemptr needed)
                let is_ptr = match array.as_ref() {
                    Expr::LVal(name) => matches!(self.lookup(name), Some(Symbol::PtrArray(_))),
                    _ => false,
                };
                let idx_val = self.gen_expr(index)?;
                let base = self.gen_expr_base(array)?;
                let elem_ptr = if is_ptr {
                    let p = self.alloc_tmp();
                    self.emit(&format!("{p} = getptr {base}, {idx_val}"));
                    p
                } else {
                    let p0 = self.alloc_tmp();
                    self.emit(&format!("{p0} = getelemptr {base}, 0"));
                    let p1 = self.alloc_tmp();
                    self.emit(&format!("{p1} = getptr {p0}, {idx_val}"));
                    p1
                };
                let num_indices = 1 + Self::count_indices(array);
                let total_dims = match array.as_ref() {
                    Expr::LVal(name) => match self.lookup(name) {
                        Some(Symbol::Array(_, ndims)) => *ndims,
                        Some(Symbol::PtrArray(_)) => 1,
                        _ => 1,
                    },
                    Expr::Index { .. } => {
                        let mut cur = array.as_ref();
                        loop {
                            match cur {
                                Expr::LVal(name) => break match self.lookup(name) {
                                    Some(Symbol::Array(_, ndims)) => *ndims,
                                    _ => 1,
                                },
                                Expr::Index { array: a, .. } => cur = a.as_ref(),
                                _ => break 1,
                            }
                        }
                    }
                    _ => 1,
                };
                if num_indices >= total_dims {
                    let result = self.alloc_tmp();
                    self.emit(&format!("{result} = load {elem_ptr}"));
                    Ok(result)
                } else {
                    let decay = self.alloc_tmp();
                    self.emit(&format!("{decay} = getelemptr {elem_ptr}, 0"));
                    Ok(decay)
                }
            }
            Expr::InitList(items) => {
                let mut last = String::new();
                for item in items {
                    last = self.gen_expr(item)?;
                }
                Ok(last)
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

    fn get_ndparam_dims(&self, expr: &Expr) -> Option<Vec<i32>> {
        match expr {
            Expr::LVal(name) => match self.lookup(name) {
                Some(Symbol::NdParam { dims, .. }) => Some(dims.clone()),
                _ => None,
            },
            Expr::Index { array, .. } => self.get_ndparam_dims(array),
            _ => None,
        }
    }

    fn count_indices(expr: &Expr) -> usize {
        match expr {
            Expr::Index { array, .. } => 1 + Self::count_indices(array),
            _ => 0,
        }
    }

    fn gen_ndparam_index(&mut self, idx_expr: &Expr) -> CompilerResult<String> {
        let mut indices: Vec<&Expr> = Vec::new();
        let mut cur = idx_expr;
        let base_name;
        let dims;
        loop {
            match cur {
                Expr::Index { array, index } => {
                    indices.push(index.as_ref());
                    cur = array.as_ref();
                }
                Expr::LVal(name) => {
                    if let Some(Symbol::NdParam { name: koopa_name, dims: d }) = self.lookup(name) {
                        base_name = koopa_name.clone();
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

        // Spec: first index → getptr, remaining → getelemptr
        let first_idx = self.gen_expr(indices[0])?;
        let p = self.alloc_tmp();
        self.emit(&format!("{p} = getptr @{base_name}, {first_idx}"));
        let mut ptr = p;
        for (i, idx) in indices.iter().enumerate().skip(1) {
            let idx_val = self.gen_expr(idx)?;
            let p1 = self.alloc_tmp();
            self.emit(&format!("{p1} = getelemptr {ptr}, {idx_val}"));
            ptr = p1;
        }
        if indices.len() >= total_dims {
            let result = self.alloc_tmp();
            self.emit(&format!("{result} = load {ptr}"));
            Ok(result)
        } else {
            // Partial indexing: decay array pointer to element pointer
            let decay = self.alloc_tmp();
            self.emit(&format!("{decay} = getelemptr {ptr}, 0"));
            Ok(decay)
        }
    }

    fn gen_expr_base(&mut self, expr: &Expr) -> CompilerResult<String> {
        match expr {
            Expr::LVal(name) => match self.lookup(name) {
                Some(Symbol::Var(n)) | Some(Symbol::Array(n, _)) | Some(Symbol::PtrArray(n)) => Ok(format!("@{}", n)),
                Some(Symbol::NdParam { name: n, .. }) => Ok(format!("@{}", n)),
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

