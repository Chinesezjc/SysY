use std::collections::HashMap;

use crate::ast::{BinaryOp, Block, BlockItem, CompUnit, Decl, Expr, GlobalItem, Stmt, Type, UnaryOp};
use crate::error::{CompilerError, CompilerResult};

// ── RISC-V ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub(crate) enum RvSymbol {
    Const(i32),
    Var { offset: i32 },
    Array { offset: i32, dims: Vec<i32>, init_vals: Vec<i32> },
    PtrArray { offset: i32 },
    NdParam { offset: i32, dims: Vec<i32> },
    Global(String, Vec<i32>, Vec<i32>),
}

pub(crate) struct RiscvGen {
    scopes: Vec<HashMap<String, RvSymbol>>,
    globals: HashMap<String, RvSymbol>,
    var_offsets: HashMap<String, i32>,
    mangled_names: HashMap<String, Vec<String>>,
    read_pos: HashMap<String, usize>,
    frame_size: i32,
    extra_sp: i32,
    label: usize,
    loop_stack: Vec<(String, String)>,
    current_ret_type: Type,
    data_section: String,
    out: String,
}

impl RiscvGen {
    pub(crate) fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
            globals: HashMap::new(),
            var_offsets: HashMap::new(),
            mangled_names: HashMap::new(),
            read_pos: HashMap::new(),
            frame_size: 0,
            extra_sp: 0,
            label: 0,
            loop_stack: Vec::new(),
            current_ret_type: Type::Int,
            data_section: String::new(),
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
        self.globals.get(name)
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

    /// Emit stack pointer adjustment handling large delta (>2047)
    fn emit_sp_add(&mut self, delta: i32) {
        if delta > 0 {
            if delta < 2048 { self.emit(&format!("addi sp, sp, {delta}")); }
            else { self.emit(&format!("li t0, {delta}")); self.emit("add sp, sp, t0"); }
        } else if delta < 0 {
            let abs_delta = -delta;
            if abs_delta < 2048 { self.emit(&format!("addi sp, sp, -{abs_delta}")); }
            else { self.emit(&format!("li t0, {abs_delta}")); self.emit("sub sp, sp, t0"); }
        }
    }

    /// Emit load with large sp offset
    fn emit_lw(&mut self, rd: &str, offset: i32) {
        if offset >= -2048 && offset < 2048 { self.emit(&format!("lw {rd}, {offset}(sp)")); }
        else {
            // Use a scratch register to compute the address, then load into rd
            self.emit(&format!("li t0, {offset}"));
            self.emit("add t0, sp, t0");
            self.emit(&format!("lw {rd}, 0(t0)"));
        }
    }

    /// Emit store with large sp offset
    fn emit_sw(&mut self, rs: &str, offset: i32) {
        if offset >= -2048 && offset < 2048 { self.emit(&format!("sw {rs}, {offset}(sp)")); }
        else {
            // Use a scratch register that differs from rs to avoid clobbering the value
            let scratch = if rs == "t0" { "t1" } else { "t0" };
            self.emit(&format!("li {scratch}, {offset}"));
            self.emit(&format!("add {scratch}, sp, {scratch}"));
            self.emit(&format!("sw {rs}, 0({scratch})"));
        }
    }

    /// Emit addi with potentially large immediate (uses t0 as scratch)
    fn emit_addi_large(&mut self, rd: &str, rs: &str, imm: i32) {
        if imm >= -2048 && imm < 2048 {
            self.emit(&format!("addi {rd}, {rs}, {imm}"));
        } else {
            self.emit(&format!("li t0, {imm}"));
            self.emit(&format!("add {rd}, {rs}, t0"));
        }
    }

    /// Push t2 to stack (save accumulator during nested array index eval)
    fn emit_push_t2(&mut self) {
        self.emit("addi sp, sp, -4");
        self.emit("sw t2, 0(sp)");
        self.extra_sp += 4;
    }

    /// Pop t2 from stack (restore accumulator after nested array index eval)
    fn emit_pop_t2(&mut self) {
        self.emit("lw t2, 0(sp)");
        self.emit("addi sp, sp, 4");
        self.extra_sp -= 4;
    }

    pub(crate) fn gen_program(mut self, program: &CompUnit) -> CompilerResult<String> {
        // First pass: collect global declarations
        let mut global_const_vals: HashMap<String, i32> = HashMap::new();
        for item in &program.items {
            if let GlobalItem::Decl(decl) = item {
                match decl {
                    Decl::Const(defs) => {
                        for def in defs {
                            if def.dims.is_empty() && !matches!(&def.init, Expr::InitList(_)) {
                                let val = Self::collect_eval_const(&def.init, &global_const_vals);
                                global_const_vals.insert(def.name.clone(), val);
                                self.globals.insert(def.name.clone(), RvSymbol::Const(val));
                            } else {
                                let label = def.name.clone();
                                let dims: Vec<i32> = def.dims.iter()
                                    .map(|d| Self::collect_eval_const(d, &global_const_vals))
                                    .collect();
                                let total: i32 = dims.iter().product();
                                self.data_section.push_str(&format!("{}:\n", label));
                                let mut vals = Vec::new();
                                self.flatten_init_vals(&dims, &def.init, &mut vals, &mut vec![]);
                                let total_usize = total as usize;
                                if vals.len() > total_usize { vals.truncate(total_usize); }
                                for v in &vals {
                                    self.data_section.push_str(&format!("  .word {}\n", v));
                                }
                                let remaining = total_usize.saturating_sub(vals.len());
                                if remaining > 0 {
                                    self.data_section.push_str(&format!("  .zero {}\n", remaining * 4));
                                }
                                self.globals.insert(label.clone(), RvSymbol::Global(label, dims, vals));
                            }
                        }
                    }
                    Decl::Var(defs) => {
                        for def in defs {
                            let label = def.name.clone();
                            if def.dims.is_empty() {
                                let init_val = def.init.as_ref()
                                    .map(|e| Self::collect_eval_const(e, &global_const_vals))
                                    .unwrap_or(0);
                                self.data_section.push_str(&format!("{}:\n  .word {}\n", label, init_val));
                                self.globals.insert(label.clone(), RvSymbol::Global(label, vec![], vec![]));
                            } else {
                                let dims: Vec<i32> = def.dims.iter()
                                    .map(|d| Self::collect_eval_const(d, &global_const_vals))
                                    .collect();
                                let total: i32 = dims.iter().product();
                                self.data_section.push_str(&format!("{}:\n", label));
                                // Emit initializer values or zeros
                                let mut vals = Vec::new();
                                if let Some(init) = &def.init {
                                    self.flatten_init_vals(dims.as_slice(), init, &mut vals, &mut vec![]);
                                    let total_usize = total as usize;
                                    if vals.len() > total_usize { vals.truncate(total_usize); }
                                    for v in &vals {
                                        self.data_section.push_str(&format!("  .word {}\n", v));
                                    }
                                    let remaining = total_usize.saturating_sub(vals.len());
                                    if remaining > 0 {
                                        self.data_section.push_str(&format!("  .zero {}\n", remaining * 4));
                                    }
                                } else {
                                    self.data_section.push_str(&format!("  .zero {}\n", total * 4));
                                }
                                self.globals.insert(label.clone(), RvSymbol::Global(label, dims, vals));
                            }
                        }
                    }
                }
            }
        }

        let mut out = String::new();
        if !self.data_section.is_empty() {
            out.push_str("  .data\n");
            out.push_str(&self.data_section);
        }
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

                    let mut const_vals: HashMap<String, i32> = global_const_vals.clone();
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
                    self.emit_sp_add(-aligned_frame);
                    self.emit_sw("ra", ra_offset);

                    // Store params into their stack slots
                    for (i, param) in func.params.iter().enumerate() {
                        let (_, offset) = self.next_mangled(&param.name);
                        let adjusted_offset = offset + 4;
                        let reg = match i {
                            0 => "a0", 1 => "a1", 2 => "a2", 3 => "a3",
                            4 => "a4", 5 => "a5", 6 => "a6", 7 => "a7",
                            _ => {
                                // Stack args: load from caller's frame
                                let stack_arg_offset = aligned_frame + (i - 8) as i32 * 4;
                                self.emit_lw("t0", stack_arg_offset);
                                self.emit_sw("t0", adjusted_offset);
                                let sym = if param.is_array {
                                    if param.array_dims.is_empty() { RvSymbol::PtrArray { offset: adjusted_offset } }
                                    else {
                                        let fixed_dims: Vec<i32> = param.array_dims.iter()
                                            .map(|d| Self::collect_eval_const(d, &global_const_vals))
                                            .collect();
                                        RvSymbol::NdParam { offset: adjusted_offset, dims: fixed_dims }
                                    }
                                } else { RvSymbol::Var { offset: adjusted_offset } };
                                self.scopes.last_mut().unwrap().insert(param.name.clone(), sym);
                                continue;
                            }
                        };
                        self.emit_sw(reg, adjusted_offset);
                        let sym = if param.is_array {
                            if param.array_dims.is_empty() {
                                RvSymbol::PtrArray { offset: adjusted_offset }
                            } else {
                                let fixed_dims: Vec<i32> = param.array_dims.iter()
                                    .map(|d| Self::collect_eval_const(d, &global_const_vals))
                                    .collect();
                                RvSymbol::NdParam { offset: adjusted_offset, dims: fixed_dims }
                            }
                        } else {
                            RvSymbol::Var { offset: adjusted_offset }
                        };
                        self.scopes.last_mut().unwrap().insert(param.name.clone(), sym);
                    }

                    // Reset read_pos for local vars
                    // (params already consumed their mangled names)
                    // We don't reset — continue reading from the same position

                    self.gen_block(&func.body, aligned_frame)?;

                    // Ensure void functions return
                    if func.ret_type == Type::Void {
                        let ra_offset = aligned_frame - 4;
                        self.emit_lw("ra", ra_offset);
                        self.emit_sp_add(aligned_frame);
                        self.emit("ret");
                    }

                    out.push_str(&self.out);
                }
                _ => {}
            }
        }
        Ok(out)
    }

    fn flatten_init_vals(&self, dims: &[i32], init: &Expr, vals: &mut Vec<i32>,
                          runtime_inits: &mut Vec<(usize, Expr)>) {
        let start_len = vals.len();
        let total: usize = dims.iter().map(|&d| d as usize).product();
        let inner_dim = *dims.last().unwrap_or(&1) as usize;
        match init {
            Expr::InitList(items) => {
                if items.is_empty() { return; }
                for item in items {
                    match item {
                        Expr::InitList(_) => {
                            // Align to innermost boundary
                            let rem = (inner_dim - (vals.len() - start_len) % inner_dim) % inner_dim;
                            for _ in 0..rem { vals.push(0); }
                            // Process the sub-init (fills one complete sub-array)
                            let sub_start = vals.len();
                            let sub_dims = if dims.len() > 1 { &dims[1..] } else { &[] };
                            let sub_total: usize = sub_dims.iter().map(|&d| d as usize).product();
                            self.flatten_init_vals(sub_dims, item, vals, runtime_inits);
                            // Pad to fill the sub-array
                            let target = sub_start + if sub_dims.is_empty() { inner_dim } else { sub_total };
                            while vals.len() < target { vals.push(0); }
                        }
                        Expr::Int(n) => vals.push(*n),
                        e => {
                            let pos = vals.len();
                            match self.eval_const(e) {
                                Ok(v) => vals.push(v),
                                Err(_) => {
                                    vals.push(0);  // placeholder
                                    runtime_inits.push((pos, e.clone()));
                                }
                            }
                        }
                    }
                }
                while vals.len() < start_len + total { vals.push(0); }
            }
            Expr::Int(n) => { vals.push(*n); while vals.len() < start_len + total { vals.push(0); } }
            _ => {}
        }
    }

    fn collect_eval_const(expr: &Expr, const_vals: &HashMap<String, i32>) -> i32 {
        match expr {
            Expr::Int(n) => *n,
            Expr::LVal(name) => const_vals.get(name).copied().unwrap_or(1),
            Expr::Unary { op: UnaryOp::Minus, expr } => -Self::collect_eval_const(expr, const_vals),
            Expr::Unary { op: UnaryOp::Not, expr } => (Self::collect_eval_const(expr, const_vals) == 0) as i32,
            Expr::Unary { .. } => Self::collect_eval_const(expr, const_vals),
            Expr::Binary { op, lhs, rhs } => {
                let l = Self::collect_eval_const(lhs, const_vals);
                let r = Self::collect_eval_const(rhs, const_vals);
                match op {
                    BinaryOp::Mul => l * r,
                    BinaryOp::Div => if r != 0 { l / r } else { 1 },
                    BinaryOp::Rem => if r != 0 { l % r } else { 1 },
                    BinaryOp::Add => l + r,
                    BinaryOp::Sub => l - r,
                    _ => 1,
                }
            }
            _ => 1,
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
                        if def.dims.is_empty() && !matches!(&def.init, Expr::InitList(_)) {
                            let val = Self::collect_eval_const(&def.init, const_vals);
                            const_vals.insert(def.name.clone(), val);
                        } else {
                            let count = name_count.entry(def.name.clone()).or_insert(0);
                            let mangled = if *count == 0 { def.name.clone() } else { format!("{}_{}", def.name, count) };
                            *count += 1;
                            self.var_offsets.insert(mangled.clone(), *slot * 4);
                            self.mangled_names.entry(def.name.clone()).or_default().push(mangled);
                            let elems: i32 = def.dims.iter()
                                .map(|d| Self::collect_eval_const(d, const_vals))
                                .product();
                            *slot += if elems > 0 { elems } else { 1 };
                        }
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
                    if def.dims.is_empty() && !matches!(&def.init, Expr::InitList(_)) {
                        let val = self.eval_const(&def.init)?;
                        self.scopes.last_mut().unwrap().insert(def.name.clone(), RvSymbol::Const(val));
                    } else {
                        let (_, offset) = self.next_mangled(&def.name);
                        let adjusted_offset = offset + 4;
                        let dims: Vec<i32> = def.dims.iter()
                            .map(|d| self.eval_const(d))
                            .collect::<CompilerResult<_>>()?;
                        // Emit init values for const arrays
                        let mut vals = Vec::new();
                        let mut runtime_inits = Vec::new();
                        self.flatten_init_vals(&dims, &def.init, &mut vals, &mut runtime_inits);
                        let total_usize: usize = dims.iter().map(|&d| d as usize).product();
                        if vals.len() > total_usize { vals.truncate(total_usize); }
                        for (i, v) in vals.iter().enumerate() {
                            self.emit(&format!("li t0, {}", v));
                            let off = adjusted_offset + i as i32 * 4;
                            self.emit_sw("t0", off);
                        }
                        // Emit runtime init for non-const expressions (e.g. function calls)
                        for (pos, expr) in &runtime_inits {
                            self.gen_expr(expr, frame)?;
                            let off = adjusted_offset + *pos as i32 * 4;
                            self.emit_sw("a0", off);
                        }
                        self.scopes.last_mut().unwrap().insert(
                            def.name.clone(),
                            RvSymbol::Array { offset: adjusted_offset, dims, init_vals: vals },
                        );
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
                    let (_, offset) = self.next_mangled(&def.name);
                    let adjusted_offset = offset + 4; // +4 for ra
                    if def.dims.is_empty() {
                        if let Some(init) = &def.init {
                            self.gen_expr(init, frame)?;
                            self.emit_sw("a0", adjusted_offset);
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
                        // Emit init values if present
                        let mut vals = Vec::new();
                        let mut runtime_inits = Vec::new();
                        if let Some(init) = &def.init {
                            self.flatten_init_vals(&dims, init, &mut vals, &mut runtime_inits);
                            let total_usize: usize = dims.iter().map(|&d| d as usize).product();
                            if vals.len() > total_usize { vals.truncate(total_usize); }
                            for (i, v) in vals.iter().enumerate() {
                                self.emit(&format!("li t0, {}", v));
                                let offset = adjusted_offset + i as i32 * 4;
                                self.emit_sw("t0", offset);
                            }
                            // Emit runtime init for non-const expressions (e.g. function calls)
                            for (pos, expr) in &runtime_inits {
                                self.gen_expr(expr, frame)?;
                                let offset = adjusted_offset + *pos as i32 * 4;
                                self.emit_sw("a0", offset);
                            }
                        }
                        self.scopes.last_mut().unwrap().insert(
                            def.name.clone(),
                            RvSymbol::Array {
                                offset: adjusted_offset,
                                dims,
                                init_vals: vals,
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
                self.emit_lw("ra", ra_offset);
                self.emit_sp_add(frame);
                self.emit("ret");
            }
            Stmt::Assign { name, index, expr } => {
                if index.is_empty() {
                    match self.lookup(name) {
                        Some(RvSymbol::Var { offset }) => {
                            let off = *offset;
                            self.gen_expr(expr, frame)?;
                            self.emit_sw("a0", off);
                        }
                        Some(RvSymbol::Global(label, _, _)) => {
                            let l = label.clone();
                            self.gen_expr(expr, frame)?;
                            self.emit("mv t0, a0");
                            self.emit(&format!("la t1, {l}"));
                            self.emit("sw t0, 0(t1)");
                        }
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
                } else {
                    let sym = self.lookup(name).cloned();
                    match sym {
                        Some(RvSymbol::Array { offset, dims, .. }) => {
                            let arr_offset = offset;
                            let arr_dims = dims.clone();
                            self.gen_expr(expr, frame)?;
                            self.emit("mv t1, a0");
                            for (i, idx) in index.iter().enumerate() {
                                self.gen_expr(idx, frame)?;
                                let stride: i32 = arr_dims.iter().skip(i + 1).product();
                                if stride != 1 { self.emit(&format!("li t0, {}", stride)); self.emit("mul a0, a0, t0"); }
                                if i == 0 { self.emit("mv t2, a0"); } else { self.emit("add t2, t2, a0"); }
                            }
                            self.emit("slli t2, t2, 2");
                            self.emit_addi_large("t2", "t2", arr_offset + self.extra_sp);
                            self.emit("add t2, sp, t2");
                            self.emit("sw t1, 0(t2)");
                        }
                        Some(RvSymbol::PtrArray { offset }) => {
                            let addr = offset + self.extra_sp;
                            self.emit_lw("t3", addr);
                            // Push base to stack (handles arbitrary nesting)
                            self.emit("addi sp, sp, -4");
                            self.emit("sw t3, 0(sp)");
                            self.extra_sp += 4;
                            self.gen_expr(expr, frame)?;
                            self.emit("mv t1, a0");
                            for (i, idx) in index.iter().enumerate() {
                                if i > 0 { self.emit_push_t2(); }
                                self.gen_expr(idx, frame)?;
                                if i > 0 { self.emit_pop_t2(); }
                                self.emit("slli a0, a0, 2");
                                if i == 0 { self.emit("mv t2, a0"); } else { self.emit("add t2, t2, a0"); }
                            }
                            // Pop base from stack
                            self.emit("lw t4, 0(sp)");
                            self.emit("addi sp, sp, 4");
                            self.extra_sp -= 4;
                            self.emit("add t2, t4, t2");
                            self.emit("sw t1, 0(t2)");
                        }
                        Some(RvSymbol::NdParam { offset, dims }) => {
                            let addr = offset + self.extra_sp;
                            self.emit_lw("t3", addr);
                            // Push base to stack (handles arbitrary nesting)
                            self.emit("addi sp, sp, -4");
                            self.emit("sw t3, 0(sp)");
                            self.extra_sp += 4;
                            self.gen_expr(expr, frame)?;
                            self.emit("mv t1, a0");
                            for (i, idx) in index.iter().enumerate() {
                                if i > 0 { self.emit_push_t2(); }
                                self.gen_expr(idx, frame)?;
                                if i > 0 { self.emit_pop_t2(); }
                                let stride: i32 = if i == 0 { dims.iter().product() } else { dims.iter().skip(i).product() };
                                if stride != 1 { self.emit(&format!("li t0, {}", stride)); self.emit("mul a0, a0, t0"); }
                                if i == 0 { self.emit("mv t2, a0"); } else { self.emit("add t2, t2, a0"); }
                            }
                            self.emit("slli t2, t2, 2");
                            // Pop base from stack
                            self.emit("lw t4, 0(sp)");
                            self.emit("addi sp, sp, 4");
                            self.extra_sp -= 4;
                            self.emit("add t2, t4, t2");
                            self.emit("sw t1, 0(t2)");
                        }
                        Some(RvSymbol::Global(label, dims, _)) if !dims.is_empty() => {
                            let l = label.clone();
                            let arr_dims = dims.clone();
                            self.emit(&format!("la t3, {l}"));
                            // Push base to stack (handles arbitrary nesting)
                            self.emit("addi sp, sp, -4");
                            self.emit("sw t3, 0(sp)");
                            self.extra_sp += 4;
                            self.gen_expr(expr, frame)?;
                            self.emit("mv t1, a0");
                            for (i, idx) in index.iter().enumerate() {
                                if i > 0 { self.emit_push_t2(); }
                                self.gen_expr(idx, frame)?;
                                if i > 0 { self.emit_pop_t2(); }
                                let stride: i32 = arr_dims.iter().skip(i + 1).product();
                                if stride != 1 { self.emit(&format!("li t0, {}", stride)); self.emit("mul a0, a0, t0"); }
                                if i == 0 { self.emit("mv t2, a0"); } else { self.emit("add t2, t2, a0"); }
                            }
                            self.emit("slli t2, t2, 2");
                            // Pop base from stack
                            self.emit("lw t4, 0(sp)");
                            self.emit("addi sp, sp, 4");
                            self.extra_sp -= 4;
                            self.emit("add t2, t4, t2");
                            self.emit("sw t1, 0(t2)");
                        }
                        _ => {
                            return Err(CompilerError::new(format!(
                                "'{name}' is not an array"
                            )));
                        }
                    }
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
            Stmt::Empty => {}
        }
        Ok(())
    }

    fn eval_const_index(&self, array: &Expr, index: &Expr) -> CompilerResult<i32> {
        // Decompose nested index chain: a[i][j] → base=a, indices=[i, j]
        let mut indices: Vec<&Expr> = vec![index];
        let mut base: &Expr = array;
        while let Expr::Index { array: inner_arr, index: inner_idx } = base {
            indices.push(inner_idx);
            base = inner_arr;
        }
        indices.reverse();
        let name = match base {
            Expr::LVal(n) => n,
            _ => return Err(CompilerError::new("array base is not a variable")),
        };

        // Look up the array symbol and get dims + init_vals
        let (dims, init_vals) = match self.lookup(name) {
            Some(RvSymbol::Array { dims, init_vals, .. }) => (dims.clone(), init_vals.clone()),
            Some(RvSymbol::Global(_, dims, init_vals)) => (dims.clone(), init_vals.clone()),
            _ => return Err(CompilerError::new(format!("'{name}' is not a const array"))),
        };

        // Compute flat offset using strides
        let mut flat: i32 = 0;
        for (i, idx) in indices.iter().enumerate() {
            let idx_val = self.eval_const(idx)?;
            let stride: i32 = dims.iter().skip(i + 1).product();
            flat += idx_val * stride;
        }

        // Look up the value
        init_vals.get(flat as usize).copied()
            .ok_or_else(|| CompilerError::new(format!(
                "const array index out of bounds: index {flat}, len {}",
                init_vals.len()
            )))
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
            Expr::Index { array, index } => {
                self.eval_const_index(array, index)
            }
            Expr::InitList(_) => Err(CompilerError::new(
                "initializer list is not a compile-time constant",
            )),
        }
    }

    fn gen_expr(&mut self, expr: &Expr, frame: i32) -> CompilerResult<()> {
        match expr {
            Expr::Int(n) => {
                self.emit(&format!("li a0, {n}"));
            }
            Expr::LVal(name) => {
                let sym = self.lookup(name).cloned();
                match sym {
                Some(RvSymbol::Const(v)) => {
                    self.emit(&format!("li a0, {v}"));
                }
                Some(RvSymbol::Var { offset }) => {
                    let addr = offset + self.extra_sp;
                    self.emit_lw("a0", addr);
                }
                Some(RvSymbol::Array { offset, .. }) => {
                    self.emit_addi_large("a0", "sp", offset + self.extra_sp);
                }
                Some(RvSymbol::PtrArray { offset }) => {
                    let addr = offset + self.extra_sp;
                    self.emit_lw("a0", addr);
                }
                Some(RvSymbol::NdParam { offset, .. }) => {
                    let addr = offset + self.extra_sp;
                    self.emit_lw("a0", addr);
                }
                Some(RvSymbol::Global(label, dims, _)) => {
                    let l = label.clone();
                    self.emit(&format!("la a0, {l}"));
                    if dims.is_empty() {
                        self.emit("lw a0, 0(a0)");
                    }
                }
                None => {
                    return Err(CompilerError::new(format!(
                        "undefined identifier '{name}'"
                    )));
                }
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
                    self.emit(&format!("j {end_label}"));
                    self.emit_label(&false_label);
                    self.emit("li a0, 0");
                    self.emit_label(&end_label);
                }
                BinaryOp::Or => {
                    self.gen_expr(lhs, frame)?;
                    let true_label = self.new_label();
                    let end_label = self.new_label();
                    self.emit(&format!("bnez a0, {true_label}"));
                    self.gen_expr(rhs, frame)?;
                    self.emit("snez a0, a0");
                    self.emit(&format!("j {end_label}"));
                    self.emit_label(&true_label);
                    self.emit("li a0, 1");
                    self.emit_label(&end_label);
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
                let mut indices: Vec<&Expr> = vec![index.as_ref()];
                let mut base: &Expr = array.as_ref();
                while let Expr::Index { array: inner_arr, index: inner_idx } = base {
                    indices.push(inner_idx.as_ref());
                    base = inner_arr.as_ref();
                }
                indices.reverse();
                if let Expr::LVal(name) = base {
                    let sym = self.lookup(name).cloned();
                    match sym {
                        Some(RvSymbol::Array { offset, dims, .. }) => {
                            let arr_offset = offset;
                            let arr_dims = dims.clone();
                            let total_dims = arr_dims.len();
                            for (i, idx) in indices.iter().enumerate() {
                                if i > 0 { self.emit_push_t2(); }
                                self.gen_expr(idx, frame)?;
                                if i > 0 { self.emit_pop_t2(); }
                                let stride: i32 = arr_dims.iter().skip(i + 1).product();
                                if stride != 1 { self.emit(&format!("li t0, {}", stride)); self.emit("mul a0, a0, t0"); }
                                if i == 0 { self.emit("mv t2, a0"); } else { self.emit("add t2, t2, a0"); }
                            }
                            self.emit("slli t2, t2, 2");
                            self.emit_addi_large("t2", "t2", arr_offset + self.extra_sp);
                            self.emit("add t2, sp, t2");
                            if indices.len() >= total_dims { self.emit("lw a0, 0(t2)"); } else { self.emit("mv a0, t2"); }
                        }
                        Some(RvSymbol::PtrArray { offset }) => {
                            let addr = offset + self.extra_sp;
                            self.emit_lw("t3", addr);
                            // Push base to stack (handles arbitrary nesting)
                            self.emit("addi sp, sp, -4");
                            self.emit("sw t3, 0(sp)");
                            self.extra_sp += 4;
                            for (i, idx) in indices.iter().enumerate() {
                                if i > 0 { self.emit_push_t2(); }
                                self.gen_expr(idx, frame)?;
                                if i > 0 { self.emit_pop_t2(); }
                                self.emit("slli a0, a0, 2");
                                if i == 0 { self.emit("mv t2, a0"); } else { self.emit("add t2, t2, a0"); }
                            }
                            // Pop base from stack
                            self.emit("lw t5, 0(sp)");
                            self.emit("addi sp, sp, 4");
                            self.extra_sp -= 4;
                            self.emit("add t2, t5, t2");
                            self.emit("lw a0, 0(t2)");
                        }
                        Some(RvSymbol::NdParam { offset, dims }) => {
                            let addr = offset + self.extra_sp;
                            self.emit_lw("t3", addr);
                            // Push base to stack (handles arbitrary nesting)
                            self.emit("addi sp, sp, -4");
                            self.emit("sw t3, 0(sp)");
                            self.extra_sp += 4;
                            let total_dims = 1 + dims.len();
                            for (i, idx) in indices.iter().enumerate() {
                                if i > 0 { self.emit_push_t2(); }
                                self.gen_expr(idx, frame)?;
                                if i > 0 { self.emit_pop_t2(); }
                                let stride: i32 = if i == 0 { dims.iter().product() } else { dims.iter().skip(i).product() };
                                if stride != 1 { self.emit(&format!("li t0, {}", stride)); self.emit("mul a0, a0, t0"); }
                                if i == 0 { self.emit("mv t2, a0"); } else { self.emit("add t2, t2, a0"); }
                            }
                            self.emit("slli t2, t2, 2");
                            // Pop base from stack
                            self.emit("lw t5, 0(sp)");
                            self.emit("addi sp, sp, 4");
                            self.extra_sp -= 4;
                            self.emit("add t2, t5, t2");
                            if indices.len() >= total_dims { self.emit("lw a0, 0(t2)"); } else { self.emit("mv a0, t2"); }
                        }
                        Some(RvSymbol::Global(label, dims, _)) if !dims.is_empty() => {
                            let l = label.clone();
                            let arr_dims = dims.clone();
                            let total_dims = arr_dims.len();
                            self.emit(&format!("la t3, {l}"));
                            // Push base to stack (handles arbitrary nesting)
                            self.emit("addi sp, sp, -4");
                            self.emit("sw t3, 0(sp)");
                            self.extra_sp += 4;
                            for (i, idx) in indices.iter().enumerate() {
                                if i > 0 { self.emit_push_t2(); }
                                self.gen_expr(idx, frame)?;
                                if i > 0 { self.emit_pop_t2(); }
                                let stride: i32 = arr_dims.iter().skip(i + 1).product();
                                if stride != 1 { self.emit(&format!("li t0, {}", stride)); self.emit("mul a0, a0, t0"); }
                                if i == 0 { self.emit("mv t2, a0"); } else { self.emit("add t2, t2, a0"); }
                            }
                            self.emit("slli t2, t2, 2");
                            // Pop base from stack
                            self.emit("lw t5, 0(sp)");
                            self.emit("addi sp, sp, 4");
                            self.extra_sp -= 4;
                            self.emit("add t2, t5, t2");
                            if indices.len() >= total_dims { self.emit("lw a0, 0(t2)"); } else { self.emit("mv a0, t2"); }
                        }
                        _ => return Err(CompilerError::new(format!("'{name}' is not an array"))),
                    }
                } else {
                    return Err(CompilerError::new("invalid array access"));
                }
            }
            Expr::InitList(_) => {
                return Err(CompilerError::new(
                    "initializer list not allowed in expression context",
                ));
            }
            Expr::Call { name, args } => {
                // Push args in reverse order so that after popping, a0 gets arg0, a1 gets arg1, etc.
                for (_i, arg) in args.iter().enumerate().rev() {
                    self.gen_expr(arg, frame)?;
                    self.emit("addi sp, sp, -4");
                    self.emit("sw a0, 0(sp)");
                    self.extra_sp += 4;
                }
                // Pop into registers: last pushed = arg0 → a0, arg1 → a1, ..., arg7 → a7
                let reg_count = args.len().min(8);
                for i in 0..reg_count {
                    let reg = match i {
                        0 => "a0", 1 => "a1", 2 => "a2", 3 => "a3",
                        4 => "a4", 5 => "a5", 6 => "a6", 7 => "a7",
                        _ => unreachable!(),
                    };
                    self.emit(&format!("lw {reg}, 0(sp)"));
                    self.emit("addi sp, sp, 4");
                    self.extra_sp -= 4;
                }
                // Stack args stay on the stack (already in correct position)
                // Note: sp now points past the stack args, which the callee expects
                self.emit(&format!("call {name}"));
                // Pop remaining stack args
                let stack_args = args.len().saturating_sub(8);
                if stack_args > 0 {
                    let delta = (stack_args * 4) as i32;
                    self.emit_sp_add(delta);
                    self.extra_sp -= delta;
                }
            }
        }
        Ok(())
    }
}

fn align16(n: i32) -> i32 {
    (n + 15) & !15
}
