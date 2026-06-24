//! Emit RISC-V assembly from optimized [`IrProgram`].

use crate::ir::*;
use std::collections::HashMap;

// ── RISC-V emitter helper ────────────────────────────────────────────────────

struct RvEmitter {
    out: String,
    data: String,
    label: usize,
}

impl RvEmitter {
    fn new() -> Self { RvEmitter { out: String::new(), data: String::new(), label: 0 } }
    fn emit(&mut self, s: &str) { self.out.push_str(s); self.out.push('\n'); }
    fn emit_data(&mut self, s: &str) { self.data.push_str(s); self.data.push('\n'); }
    fn emit_label(&mut self, name: &str) { self.out.push_str(&format!("{name}:\n")); }
    fn fresh_label(&mut self) -> String { let l = format!(".L{}", self.label); self.label += 1; l }
    fn finish(self) -> String {
        let mut r = String::new();
        if !self.data.is_empty() { r.push_str("  .data\n"); r.push_str(&self.data); }
        r.push_str("  .text\n"); r.push_str(&self.out); r
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn align16(x: i32) -> i32 { (x + 15) & !15 }

fn emit_lw(emitter: &mut RvEmitter, rd: &str, offset: i32) {
    if offset >= -2048 && offset < 2048 {
        emitter.emit(&format!("  lw {rd}, {offset}(sp)"));
    } else {
        emitter.emit(&format!("  li t0, {offset}"));
        emitter.emit("  add t0, sp, t0");
        emitter.emit(&format!("  lw {rd}, 0(t0)"));
    }
}

fn emit_sw(emitter: &mut RvEmitter, rs: &str, offset: i32) {
    if offset >= -2048 && offset < 2048 {
        emitter.emit(&format!("  sw {rs}, {offset}(sp)"));
    } else {
        let scratch = if rs == "t0" { "t1" } else { "t0" };
        emitter.emit(&format!("  li {scratch}, {offset}"));
        emitter.emit(&format!("  add {scratch}, sp, {scratch}"));
        emitter.emit(&format!("  sw {rs}, 0({scratch})"));
    }
}

fn emit_addi_sp(emitter: &mut RvEmitter, delta: i32) {
    if delta >= -2048 && delta < 2048 {
        emitter.emit(&format!("  addi sp, sp, {delta}"));
    } else {
        emitter.emit(&format!("  li t0, {delta}"));
        emitter.emit("  add sp, sp, t0");
    }
}

fn emit_offset_mul(emitter: &mut RvEmitter, elem_size: i32, rd: String, idx_reg: &str) {
    match elem_size {
        0 | 1 => {} // no-op: result is 0 or identity
        4 => { emitter.emit(&format!("  slli {rd}, {idx_reg}, 2")); }
        _ => {
            emitter.emit(&format!("  li t3, {elem_size}"));
            emitter.emit(&format!("  mul {rd}, {idx_reg}, t3"));
        }
    }
}

fn emit_addr(emitter: &mut RvEmitter, rd: &str, offset: i32) {
    if offset >= -2048 && offset < 2048 {
        emitter.emit(&format!("  addi {rd}, sp, {offset}"));
    } else {
        emitter.emit(&format!("  li {rd}, {offset}"));
        emitter.emit(&format!("  add {rd}, sp, {rd}"));
    }
}

// ── Frame layout ─────────────────────────────────────────────────────────────

struct FrameInfo {
    alloca_offsets: HashMap<usize, i32>,
    local_offsets: HashMap<usize, i32>,
    frame_size: i32,
    ra_offset: i32,
    call_spill_base: i32,
}

fn compute_frame(func: &IrFunc) -> FrameInfo {
    let mut alloca_offsets = HashMap::new();
    let mut local_offsets = HashMap::new();
    let mut offset: i32 = 0;
    for block in &func.blocks {
        for inst in &block.instrs {
            if let IrInst::Alloc { dest, ty } = inst {
                alloca_offsets.insert(*dest, offset);
                offset += type_size(ty) as i32;
            }
        }
    }
    let mut slot = offset; // continue from end of alloca area
    for block in &func.blocks {
        for inst in &block.instrs {
            // Skip Alloc — already handled in first pass (Global index namespace)
            if matches!(inst, IrInst::Alloc { .. }) { continue; }
            if let Some(dest) = inst.dest() {
                if !local_offsets.contains_key(&dest) {
                    local_offsets.insert(dest, slot);
                    slot += 4;
                }
            }
        }
    }
    // Reserve space for call argument spill area
    let max_call_args = func.blocks.iter()
        .flat_map(|b| b.instrs.iter())
        .filter_map(|i| if let IrInst::Call { args, .. } = i { Some(args.len()) } else { None })
        .max().unwrap_or(0);
    let call_spill_base = slot; // start of call arg spill area (within frame)
    slot += (max_call_args * 4) as i32;

    let total = align16(align16(slot) + 4);
    FrameInfo { alloca_offsets, local_offsets, frame_size: total, ra_offset: total - 4, call_spill_base }
}

fn type_size(ty: &IrType) -> usize {
    match ty { IrType::I32 => 4, IrType::Array(inner, len) => type_size(inner) * (*len as usize), _ => 4 }
}

// ── Main generation ──────────────────────────────────────────────────────────

pub fn emit_riscv(program: &IrProgram) -> String {
    let mut e = RvEmitter::new();
    for g in &program.globals {
        let name = program.global_name(g.name).strip_prefix('@').unwrap_or(program.global_name(g.name));
        match &g.init {
            IrGlobalInit::Zero => e.emit_data(&format!("{name}:\n  .zero {}", type_size(&g.ty))),
            IrGlobalInit::Values(vals) => {
                let mut s = format!("{name}:"); for v in vals { s.push_str(&format!("\n  .word {v}")); }
                e.emit_data(&s);
            }
        }
    }
    for func in &program.funcs { emit_function(&mut e, func, program); }
    e.finish()
}

fn emit_function(e: &mut RvEmitter, func: &IrFunc, program: &IrProgram) {
    let mut frame = compute_frame(func);

    // Build param register map (for params 0-7)
    let mut param_regs: HashMap<usize, String> = HashMap::new();
    // Param spill slots: for params without alloca slots (e.g., array params),
    // we must spill their register value to the stack so it survives clobbers.
    let mut param_spill: HashMap<usize, i32> = HashMap::new();

    // Allocate spill slots within the frame for params without allocas
    let mut spill_slot = frame.frame_size;
    for (i, (pn, _)) in func.params.iter().enumerate() {
        if i < 8 {
            param_regs.insert(*pn, format!("a{}", i));
            if !frame.alloca_offsets.contains_key(pn) {
                param_spill.insert(*pn, spill_slot);
                spill_slot += 4;
            }
        }
    }
    // Grow frame to include spill slots if needed
    if spill_slot > frame.frame_size {
        frame.frame_size = align16(spill_slot + 4); // +4 for ra at top
        frame.ra_offset = frame.frame_size - 4;
    }

    // Stack param offsets (for params >8): use final frame_size
    let mut stack_param_offsets: HashMap<usize, i32> = HashMap::new();
    for (i, (pn, _)) in func.params.iter().enumerate() {
        if i >= 8 {
            let off = frame.frame_size + (i - 8) as i32 * 4;
            stack_param_offsets.insert(*pn, off);
        }
    }

    let tf = frame.frame_size;
    let fn_name = program.func_name(func.name).strip_prefix('@').unwrap_or(program.func_name(func.name));
    e.emit(&format!("  .globl {fn_name}")); e.emit_label(fn_name);

    // Prologue
    emit_addi_sp(e, -tf);
    emit_sw(e, "ra", frame.ra_offset);

    // Store register params to their stack slots (alloca slots or spill slots)
    for (i, (pn, _)) in func.params.iter().enumerate() {
        if i >= 8 { continue; }
        let reg = ["a0","a1","a2","a3","a4","a5","a6","a7"][i];
        if let Some(&off) = frame.alloca_offsets.get(pn) { emit_sw(e, reg, off); }
        else if let Some(&off) = param_spill.get(pn) { emit_sw(e, reg, off); }
    }

    // Block labels
    let mut block_labels: HashMap<usize, String> = HashMap::new();
    for block in &func.blocks { block_labels.insert(block.label, e.fresh_label()); }

    // Emit blocks
    for block in &func.blocks {
        e.emit_label(&block_labels[&block.label]);
        for inst in &block.instrs { emit_inst(e, inst, &frame, program, &block_labels, &param_regs, &stack_param_offsets, &param_spill); }
    }

    // Fallback epilogue
    let has_ret = func.blocks.last().map_or(false, |b| b.instrs.last().map_or(false, |i| matches!(i, IrInst::Ret{..})));
    if !has_ret {
        if func.ret_type != IrType::Void { e.emit("  li a0, 0"); }
        emit_lw(e, "ra", frame.ra_offset);
        emit_addi_sp(e, tf);
        e.emit("  ret");
    }
}

fn emit_inst(e: &mut RvEmitter, inst: &IrInst, frame: &FrameInfo, program: &IrProgram, block_labels: &HashMap<usize, String>, param_regs: &HashMap<usize, String>, stack_param_offsets: &HashMap<usize, i32>, param_spill: &HashMap<usize, i32>) {
    let lo = |i: usize| frame.local_offsets.get(&i).copied().unwrap_or(0);

    match inst {
        IrInst::Alloc { .. } => {}

        IrInst::Load { dest, src } => {
            let addr = op_to_reg(e, *src, frame, program, "t2", param_regs, stack_param_offsets, param_spill);
            e.emit(&format!("  lw t0, 0({addr})"));
            emit_sw(e, "t0", lo(*dest));
        }
        IrInst::Store { value, ptr } => {
            let val = op_to_reg(e, *value, frame, program, "t1", param_regs, stack_param_offsets, param_spill);
            let p = op_to_reg(e, *ptr, frame, program, "t2", param_regs, stack_param_offsets, param_spill);
            e.emit(&format!("  sw {val}, 0({p})"));
        }
        IrInst::Arith { dest, op, lhs, rhs } => {
            let lv = op_to_reg(e, *lhs, frame, program, "t0", param_regs, stack_param_offsets, param_spill);
            let rv = op_to_reg(e, *rhs, frame, program, "t1", param_regs, stack_param_offsets, param_spill);
            let ins = match op { IrArithOp::Add=>"add", IrArithOp::Sub=>"sub", IrArithOp::Mul=>"mul", IrArithOp::Div=>"div", IrArithOp::Mod=>"rem" };
            e.emit(&format!("  {ins} a0, {lv}, {rv}"));
            emit_sw(e, "a0", lo(*dest));
        }
        IrInst::Icmp { dest, op, lhs, rhs } => {
            let lv = op_to_reg(e, *lhs, frame, program, "t0", param_regs, stack_param_offsets, param_spill);
            let rv = op_to_reg(e, *rhs, frame, program, "t1", param_regs, stack_param_offsets, param_spill);
            match op {
                IrCmpOp::Lt => e.emit(&format!("  slt a0, {lv}, {rv}")),
                IrCmpOp::Gt => e.emit(&format!("  slt a0, {rv}, {lv}")),
                IrCmpOp::Le => { e.emit(&format!("  slt a0, {rv}, {lv}")); e.emit("  xori a0, a0, 1"); }
                IrCmpOp::Ge => { e.emit(&format!("  slt a0, {lv}, {rv}")); e.emit("  xori a0, a0, 1"); }
                IrCmpOp::Eq => { e.emit(&format!("  sub a0, {lv}, {rv}")); e.emit("  sltiu a0, a0, 1"); }
                IrCmpOp::Ne => { e.emit(&format!("  sub a0, {lv}, {rv}")); e.emit("  sltu a0, zero, a0"); }
            }
            emit_sw(e, "a0", lo(*dest));
        }
        IrInst::GetPtr { dest, ptr, index, elem_size } => {
            let base = op_to_reg(e, *ptr, frame, program, "t0", param_regs, stack_param_offsets, param_spill);
            let idx = op_to_reg(e, *index, frame, program, "t1", param_regs, stack_param_offsets, param_spill);
            emit_offset_mul(e, *elem_size, format!("t1"), &idx);
            e.emit(&format!("  add t0, {base}, t1"));
            emit_sw(e, "t0", lo(*dest));
        }
        IrInst::GetElemPtr { dest, ptr, index, elem_size } => {
            let base = op_to_reg(e, *ptr, frame, program, "t0", param_regs, stack_param_offsets, param_spill);
            let idx = op_to_reg(e, *index, frame, program, "t1", param_regs, stack_param_offsets, param_spill);
            emit_offset_mul(e, *elem_size, format!("t1"), &idx);
            e.emit(&format!("  add t0, {base}, t1"));
            emit_sw(e, "t0", lo(*dest));
        }
        IrInst::Call { dest, func, args } => {
            let callee = program.func_name(*func).strip_prefix('@').unwrap_or(program.func_name(*func)).to_string();
            let n = args.len();

            // Evaluate all args at original sp into a0, push immediately in reverse
            // order. To avoid sp changes affecting subsequent evaluations, we first
            // evaluate all args, spilling results to temporary slots at the end
            // of the current frame. Then push them all at once.

            // Temp slots: use call spill area within the frame.
            // We save each arg value to sp+frame.call_spill_base + i*4, then
            // in a second pass, push onto stack and pop into registers.
            let base = frame.call_spill_base;
            for (i, arg) in args.iter().enumerate() {
                let val = op_to_reg(e, *arg, frame, program, "a0", param_regs, stack_param_offsets, param_spill);
                if val != "a0" { e.emit(&format!("  mv a0, {val}")); }
                let off = base + i as i32 * 4;
                // Store to temp slot (above frame, at original sp)
                emit_sw(e, "a0", off);
            }

            // Now push all args in reverse order (arg0 ends up at sp+0)
            let mut extra: i32 = 0;
            for i in (0..n).rev() {
                let off = base + i as i32 * 4;
                e.emit("  addi sp, sp, -4");
                extra += 4;
                emit_lw(e, "t0", off + extra);
                e.emit("  sw t0, 0(sp)");
            }

            // Pop first 8 args into a0-a7 registers
            let reg_count = n.min(8);
            for i in 0..reg_count {
                let reg = ["a0","a1","a2","a3","a4","a5","a6","a7"][i];
                e.emit(&format!("  lw {reg}, 0(sp)"));
                e.emit("  addi sp, sp, 4");
            }

            e.emit(&format!("  call {callee}"));

            // Remove remaining stack args (params 8+)
            let stack_args = n.saturating_sub(8);
            if stack_args > 0 {
                e.emit(&format!("  addi sp, sp, {}", (stack_args * 4) as i32));
            }

            if let Some(d) = dest { emit_sw(e, "a0", lo(*d)); }
        }
        IrInst::Br { cond, then_bb, else_bb } => {
            let c = op_to_reg(e, *cond, frame, program, "t0", param_regs, stack_param_offsets, param_spill);
            e.emit(&format!("  bnez {c}, {}", block_labels[then_bb]));
            e.emit(&format!("  j {}", block_labels[else_bb]));
        }
        IrInst::Jump { target } => { e.emit(&format!("  j {}", block_labels[target])); }
        IrInst::Ret { value } => {
            if let Some(v) = value {
                let val = op_to_reg(e, *v, frame, program, "a0", param_regs, stack_param_offsets, param_spill);
                if val != "a0" { e.emit(&format!("  mv a0, {val}")); }
            }
            emit_lw(e, "ra", frame.ra_offset);
            emit_addi_sp(e, frame.frame_size);
            e.emit("  ret");
        }
        IrInst::Phi { .. } => e.emit("  # phi"),
        IrInst::Asm(s) => e.emit(&format!("  {s}")),
    }
}

fn op_to_reg(e: &mut RvEmitter, op: IrOperand, frame: &FrameInfo, program: &IrProgram, pref: &str, param_regs: &HashMap<usize, String>, stack_param_offsets: &HashMap<usize, i32>, param_spill: &HashMap<usize, i32>) -> String {
    let r = pref.to_string();
    match op {
        IrOperand::Int(n) => { e.emit(&format!("  li {r}, {n}")); r }
        IrOperand::Local(i) => { emit_lw(e, &r, frame.local_offsets.get(&i).copied().unwrap_or(0)); r }
        IrOperand::Global(i) => {
            // If this param was spilled (array params), load VALUE from spill slot
            if let Some(&off) = param_spill.get(&i) {
                emit_lw(e, &r, off);
                return r;
            }
            // Check if this is a function parameter (arrives in register, no spill needed)
            if let Some(preg) = param_regs.get(&i) {
                return preg.clone();
            }
            // Check if this is a stack-passed parameter (>8 args): load VALUE
            if let Some(&off) = stack_param_offsets.get(&i) {
                emit_lw(e, &r, off);
                return r;
            }
            // Check stack-allocated alloca: compute ADDRESS
            if let Some(off) = frame.alloca_offsets.get(&i) {
                emit_addr(e, &r, *off);
            } else {
                e.emit(&format!("  la {r}, {}", program.global_name(i).strip_prefix('@').unwrap_or(program.global_name(i))));
            }
            r
        }
        IrOperand::Undef => { e.emit(&format!("  li {r}, 0")); r }
    }
}
