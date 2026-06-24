//! IR Builder — convenience API for constructing one function's IR.
//!
//! The builder accumulates basic blocks and instructions and finally
//! produces an `IrFunc` via [`IrBuilder::build`].
//!
//! It maintains its own string tables; these must be merged into the
//! parent `IrProgram` after building.

use crate::ir::*;

/// Fresh-name counters emitted by the builder alongside the function.
#[derive(Debug, Clone)]
pub struct BuilderMeta {
    pub local_names: Vec<String>,
    pub global_names: Vec<String>,
    pub block_names: Vec<String>,
    pub func_names: Vec<String>,
}

/// State for constructing one `IrFunc`.
pub struct IrBuilder {
    // ── String tables (owned, merged into IrProgram at build time) ──
    local_names: Vec<String>,
    global_names: Vec<String>,
    block_names: Vec<String>,
    func_names: Vec<String>,

    // ── Function identity ──
    func_name: usize,
    params: Vec<(usize, IrType)>,
    ret_type: IrType,

    // ── Accumulated blocks ──
    blocks: Vec<IrBlock>,
    cur_block: usize,

    // ── Pending entry-block allocas ──
    pub(crate) pending_allocas: Vec<(usize, IrType)>,

    // ── Counters for fresh names ──
    local_counter: usize,
    global_counter: usize,
    block_counter: usize,
    func_counter: usize,
    tmp_counter: u32,
    sc_counter: u32,

    block_terminated: bool,
}

impl IrBuilder {
    /// Create a new builder. `base_*` set the starting indices for each name
    /// table (pass program-level counts so indices are program-absolute).
    pub fn new(
        name: usize,
        params: Vec<(usize, IrType)>,
        ret_type: IrType,
        base_local: usize,
        base_global: usize,
        base_block: usize,
        base_func: usize,
    ) -> Self {
        let mut block_names = Vec::new();
        let entry_label = base_block;
        block_names.push("%entry".to_string());

        let entry = IrBlock {
            label: entry_label,
            instrs: Vec::new(),
            preds: Vec::new(),
        };

        IrBuilder {
            local_names: Vec::new(),
            global_names: Vec::new(),
            block_names,
            func_names: Vec::new(),
            func_name: name,
            params,
            ret_type,
            blocks: vec![entry],
            cur_block: entry_label,
            pending_allocas: Vec::new(),
            local_counter: base_local,
            global_counter: base_global,
            block_counter: base_block + 1, // entry block consumed base_block
            func_counter: base_func,
            tmp_counter: 0,
            sc_counter: 0,
            block_terminated: false,
        }
    }

    // ── Name generation ──────────────────────────────────────────────────────

    pub fn alloc_tmp(&mut self) -> usize {
        let idx = self.local_counter;
        self.local_counter += 1;
        self.local_names.push(format!("%{}", self.tmp_counter));
        self.tmp_counter += 1;
        idx
    }

    pub fn alloc_label(&mut self) -> usize {
        let idx = self.block_counter;
        self.block_counter += 1;
        self.block_names.push(format!("%label_{}", idx));
        idx
    }

    pub fn alloc_sc(&mut self) -> usize {
        let idx = self.global_counter;
        self.global_counter += 1;
        self.global_names.push(format!("@sc_{}", self.sc_counter));
        self.sc_counter += 1;
        idx
    }

    pub fn intern_global(&mut self, name: String) -> usize {
        let name = if name.starts_with('@') { name } else { format!("@{name}") };
        if let Some(pos) = self.global_names.iter().position(|n| n == &name) {
            return pos;
        }
        let idx = self.global_counter;
        self.global_counter += 1;
        self.global_names.push(name);
        idx
    }

    pub fn intern_local(&mut self, name: String) -> usize {
        let idx = self.local_counter;
        self.local_counter += 1;
        self.local_names.push(name);
        idx
    }

    pub fn intern_block(&mut self, name: String) -> usize {
        let idx = self.block_counter;
        self.block_counter += 1;
        self.block_names.push(name);
        idx
    }

    pub fn intern_func(&mut self, name: String) -> usize {
        let idx = self.func_counter;
        self.func_counter += 1;
        self.func_names.push(name);
        idx
    }

    // ── String table queries ─────────────────────────────────────────────────

    pub fn local_name(&self, idx: usize) -> &str { &self.local_names[idx] }
    pub fn global_name(&self, idx: usize) -> &str { &self.global_names[idx] }
    pub fn block_name(&self, idx: usize) -> &str { &self.block_names[idx] }
    pub fn func_name(&self, idx: usize) -> &str { &self.func_names[idx] }

    // ── Block management ─────────────────────────────────────────────────────

    pub fn cur_block_idx(&self) -> usize { self.cur_block }

    pub fn is_terminated(&self) -> bool { self.block_terminated }

    /// Start a new block with the given label and make it current.
    pub fn start_block(&mut self, label_idx: usize) {
        let block = IrBlock {
            label: label_idx,
            instrs: Vec::new(),
            preds: Vec::new(),
        };
        self.blocks.push(block);
        self.cur_block = label_idx;
        self.block_terminated = false;
    }

    fn cur_instrs(&mut self) -> &mut Vec<IrInst> {
        &mut self.blocks.iter_mut().find(|b| b.label == self.cur_block)
            .expect("current block not found").instrs
    }

    // ── Instruction emitters ─────────────────────────────────────────────────

    pub fn push(&mut self, inst: IrInst) {
        assert!(!self.block_terminated, "push into terminated block");
        if inst.is_terminator() { self.block_terminated = true; }
        self.cur_instrs().push(inst);
    }

    pub fn emit_alloc(&mut self, dest: usize, ty: IrType) {
        self.push(IrInst::Alloc { dest, ty });
    }

    pub fn emit_load(&mut self, src: IrOperand) -> IrOperand {
        let dest = self.alloc_tmp();
        self.push(IrInst::Load { dest, src });
        IrOperand::Local(dest)
    }

    pub fn emit_store(&mut self, value: IrOperand, ptr: IrOperand) {
        self.push(IrInst::Store { value, ptr });
    }

    pub fn emit_arith(&mut self, op: IrArithOp, lhs: IrOperand, rhs: IrOperand) -> IrOperand {
        let dest = self.alloc_tmp();
        self.push(IrInst::Arith { dest, op, lhs, rhs });
        IrOperand::Local(dest)
    }

    pub fn emit_icmp(&mut self, op: IrCmpOp, lhs: IrOperand, rhs: IrOperand) -> IrOperand {
        let dest = self.alloc_tmp();
        self.push(IrInst::Icmp { dest, op, lhs, rhs });
        IrOperand::Local(dest)
    }

    pub fn emit_getptr(&mut self, ptr: IrOperand, index: IrOperand, elem_size: i32) -> IrOperand {
        let dest = self.alloc_tmp();
        self.push(IrInst::GetPtr { dest, ptr, index, elem_size });
        IrOperand::Local(dest)
    }

    pub fn emit_getelemptr(&mut self, ptr: IrOperand, index: IrOperand, elem_size: i32) -> IrOperand {
        let dest = self.alloc_tmp();
        self.push(IrInst::GetElemPtr { dest, ptr, index, elem_size });
        IrOperand::Local(dest)
    }

    pub fn emit_call(&mut self, func: usize, args: Vec<IrOperand>, has_ret: bool) -> Option<IrOperand> {
        let dest = if has_ret { Some(self.alloc_tmp()) } else { None };
        self.push(IrInst::Call { dest, func, args });
        dest.map(IrOperand::Local)
    }

    pub fn emit_br(&mut self, cond: IrOperand, then_bb: usize, else_bb: usize) {
        self.push(IrInst::Br { cond, then_bb, else_bb });
    }

    pub fn emit_jump(&mut self, target: usize) {
        self.push(IrInst::Jump { target });
    }

    pub fn emit_ret(&mut self, value: Option<IrOperand>) {
        self.push(IrInst::Ret { value });
    }

    pub fn emit_phi(&mut self, incoming: Vec<(IrOperand, usize)>) -> IrOperand {
        let dest = self.alloc_tmp();
        self.push(IrInst::Phi { dest, incoming });
        IrOperand::Local(dest)
    }

    /// Emit inline assembly as a statement (no return value).
    pub fn emit_asm(&mut self, raw: String) {
        self.push(IrInst::Asm { dest: None, code: raw });
    }

    /// Emit inline assembly as an expression (result in a0, assigned to temp).
    pub fn emit_asm_expr(&mut self, raw: String) -> IrOperand {
        let dest = self.alloc_tmp();
        self.push(IrInst::Asm { dest: Some(dest), code: raw });
        IrOperand::Local(dest)
    }

    // ── Pending allocas ──────────────────────────────────────────────────────

    pub fn add_pending_alloca(&mut self, name_idx: usize, ty: IrType) {
        self.pending_allocas.push((name_idx, ty));
    }

    // ── Finalisation ─────────────────────────────────────────────────────────

    /// Consume the builder and produce an `IrFunc` and metadata (string tables).
    pub fn build(mut self) -> (IrFunc, BuilderMeta) {
        if !self.pending_allocas.is_empty() {
            let entry = &mut self.blocks[0];
            let mut new_instrs: Vec<IrInst> = self.pending_allocas.drain(..)
                .map(|(name, ty)| IrInst::Alloc { dest: name, ty })
                .collect();
            new_instrs.append(&mut entry.instrs);
            entry.instrs = new_instrs;
        }

        let func = IrFunc {
            name: self.func_name,
            params: self.params,
            ret_type: self.ret_type,
            allocas: Vec::new(),
            blocks: self.blocks,
        };

        let meta = BuilderMeta {
            local_names: self.local_names,
            global_names: self.global_names,
            block_names: self.block_names,
            func_names: self.func_names,
        };

        (func, meta)
    }
}
