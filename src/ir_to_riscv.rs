//! Emit RISC-V assembly from optimized [`IrProgram`].
//!
//! Uses stack-based allocation (matching the current `riscv_gen.rs` behavior)
//! with a0 as the expression accumulator.  Graph-coloring register allocation
//! is planned as a follow-up.

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
    fn emit_label(&mut self, name: &str) { self.out.push_str(&format!("{name}:\n")); }
    fn emit_data(&mut self, s: &str) { self.data.push_str(s); self.data.push('\n'); }
    fn fresh_label(&mut self) -> String {
        let l = format!(".L{}", self.label); self.label += 1; l
    }
    fn finish(self) -> String {
        let mut result = String::new();
        if !self.data.is_empty() { result.push_str("  .data\n"); result.push_str(&self.data); }
        result.push_str("  .text\n"); result.push_str(&self.out); result
    }
}

// ── Frame layout ─────────────────────────────────────────────────────────────

struct FrameInfo {
    /// Offset from sp for each alloca (global index).
    alloca_offsets: HashMap<usize, i32>,
    /// Offset from sp for each local temp.
    local_offsets: HashMap<usize, i32>,
    frame_size: i32,
    ra_offset: i32,
}

fn align16(x: i32) -> i32 { (x + 15) & !15 }

fn riscv_label(name: &str) -> String {
    name.strip_prefix('%').unwrap_or(name).to_string()
}

fn emit_lw(emitter: &mut RvEmitter, rd: &str, offset: i32) {
    if offset >= -2048 && offset < 2048 {
        emitter.emit(&format!("  lw {rd}, {offset}(sp)"));
    } else {
        emitter.emit(&format!("  li t0, {offset}"));
        emitter.emit(&format!("  add t0, sp, t0"));
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

fn compute_frame(func: &IrFunc) -> FrameInfo {
    let mut alloca_offsets = HashMap::new();
    let mut local_offsets = HashMap::new();
    let mut slot: i32 = 0;

    // Allocate stack slots for allocas (each 4 bytes)
    for block in &func.blocks {
        for inst in &block.instrs {
            if let IrInst::Alloc { dest, .. } = inst {
                alloca_offsets.insert(*dest, slot * 4);
                slot += 1;
            }
        }
    }

    // Allocate stack slots for all local temps (that are definitions)
    for block in &func.blocks {
        for inst in &block.instrs {
            if let Some(dest) = inst.dest() {
                if !local_offsets.contains_key(&dest) && !alloca_offsets.contains_key(&dest) {
                    local_offsets.insert(dest, slot * 4);
                    slot += 1;
                }
            }
        }
    }

    let frame_size = align16(slot * 4);
    let total = align16(frame_size + 4); // +4 for ra
    let ra_offset = total - 4;

    FrameInfo { alloca_offsets, local_offsets, frame_size: total, ra_offset }
}

// ── Main code generation ─────────────────────────────────────────────────────

pub fn emit_riscv(program: &IrProgram) -> String {
    let mut emitter = RvEmitter::new();

    // Emit global data
    for g in &program.globals {
        let name = program.global_name(g.name).strip_prefix('@').unwrap_or(program.global_name(g.name));
        match &g.init {
            IrGlobalInit::Zero => {
                let size = type_size(&g.ty);
                emitter.emit_data(&format!("{name}:\n  .zero {size}"));
            }
            IrGlobalInit::Values(vals) => {
                let mut data_str = format!("{name}:");
                for v in vals {
                    data_str.push_str(&format!("\n  .word {v}"));
                }
                emitter.emit_data(&data_str);
            }
        }
    }

    // Emit functions
    for func in &program.funcs {
        emit_function(&mut emitter, func, program);
    }

    emitter.finish()
}

fn type_size(ty: &IrType) -> usize {
    match ty {
        IrType::I32 => 4,
        IrType::Array(inner, len) => type_size(inner) * (*len as usize),
        _ => 4,
    }
}

fn emit_function(emitter: &mut RvEmitter, func: &IrFunc, program: &IrProgram) {
    let frame = compute_frame(func);
    let total_frame = frame.frame_size;

    let func_name = program.func_name(func.name).strip_prefix('@').unwrap_or(program.func_name(func.name)).to_string();
    emitter.emit(&format!("  .globl {func_name}"));
    emitter.emit_label(&func_name);

    // Prologue (use li+add for large frames)
    if total_frame <= 2048 {
        emitter.emit(&format!("  addi sp, sp, -{}", total_frame));
    } else {
        emitter.emit(&format!("  li t0, -{}", total_frame));
        emitter.emit("  add sp, sp, t0");
    }
    emitter.emit(&format!("  sw ra, {}(sp)", frame.ra_offset));

    // Store params to their stack slots
    for (i, (param_name, _)) in func.params.iter().enumerate() {
        let reg = match i { 0 => "a0", 1 => "a1", 2 => "a2", 3 => "a3", 4 => "a4", 5 => "a5", 6 => "a6", 7 => "a7", _ => continue };
        // Store param to alloca slot if one exists
        if let Some(&off) = frame.alloca_offsets.get(param_name) {
            emitter.emit(&format!("  sw {reg}, {off}(sp)"));
        } else if let Some(&off) = frame.local_offsets.get(param_name) {
            emitter.emit(&format!("  sw {reg}, {off}(sp)"));
        }
    }

    // Map IR block indices to RISC-V labels (.L0, .L1, ...)
    let mut block_labels: HashMap<usize, String> = HashMap::new();
    for block in &func.blocks {
        let label = emitter.fresh_label();
        block_labels.insert(block.label, label);
    }

    // Emit blocks
    for block in &func.blocks {
        let label_name = &block_labels[&block.label];
        emitter.emit_label(label_name);
        for inst in &block.instrs {
            emit_inst(emitter, inst, &frame, func, program, &block_labels);
        }
    }

    // Epilogue (if last block doesn't have ret, add one)
    let last_block = func.blocks.last();
    let has_ret = last_block.map_or(false, |b| {
        b.instrs.last().map_or(false, |i| matches!(i, IrInst::Ret { .. }))
    });
    if !has_ret {
        if func.ret_type == IrType::Void {
            emitter.emit(&format!("  lw ra, {}(sp)", frame.ra_offset));
            if total_frame <= 2048 {
                emitter.emit(&format!("  addi sp, sp, {}", total_frame));
            } else {
                emitter.emit(&format!("  li t0, {}", total_frame));
                emitter.emit("  add sp, sp, t0");
            }
            emitter.emit("  ret");
        } else {
            emitter.emit("  li a0, 0");
            emitter.emit(&format!("  lw ra, {}(sp)", frame.ra_offset));
            if total_frame <= 2048 {
                emitter.emit(&format!("  addi sp, sp, {}", total_frame));
            } else {
                emitter.emit(&format!("  li t0, {}", total_frame));
                emitter.emit("  add sp, sp, t0");
            }
            emitter.emit("  ret");
        }
    }
}

fn emit_inst(emitter: &mut RvEmitter, inst: &IrInst, frame: &FrameInfo, func: &IrFunc, program: &IrProgram, block_labels: &HashMap<usize, String>) {
    match inst {
        IrInst::Alloc { .. } => {} // Already handled by frame layout

        IrInst::Load { dest, src } => {
            let addr = op_to_reg(emitter, *src, frame, "t2", program);
            let dst_off = frame.local_offsets.get(dest).copied().unwrap_or(0);
            emitter.emit(&format!("  lw t0, 0({addr})"));
            emitter.emit(&format!("  sw t0, {dst_off}(sp)"));
        }
        IrInst::Store { value, ptr } => {
            let val = op_to_reg(emitter, *value, frame, "t1", program);
            let p = op_to_reg(emitter, *ptr, frame, "t2", program);
            emitter.emit(&format!("  sw {val}, 0({p})"));
        }

        IrInst::Arith { dest, op, lhs, rhs } => {
            let lv = op_to_reg(emitter, *lhs, frame, "t0", program);
            let rv = op_to_reg(emitter, *rhs, frame, "t1", program);
            let instr = match op {
                IrArithOp::Add => "add",
                IrArithOp::Sub => "sub",
                IrArithOp::Mul => "mul",
                IrArithOp::Div => "div",
                IrArithOp::Mod => "rem",
            };
            emitter.emit(&format!("  {instr} a0, {lv}, {rv}"));
            let off = frame.local_offsets.get(dest).copied().unwrap_or(0);
            emitter.emit(&format!("  sw a0, {off}(sp)"));
        }

        IrInst::Icmp { dest, op, lhs, rhs } => {
            let lv = op_to_reg(emitter, *lhs, frame, "t0", program);
            let rv = op_to_reg(emitter, *rhs, frame, "t1", program);
            match op {
                IrCmpOp::Lt => emitter.emit(&format!("  slt a0, {lv}, {rv}")),
                IrCmpOp::Gt => emitter.emit(&format!("  slt a0, {rv}, {lv}")),
                IrCmpOp::Le => {
                    emitter.emit(&format!("  slt a0, {rv}, {lv}"));
                    emitter.emit("  xori a0, a0, 1");
                }
                IrCmpOp::Ge => {
                    emitter.emit(&format!("  slt a0, {lv}, {rv}"));
                    emitter.emit("  xori a0, a0, 1");
                }
                IrCmpOp::Eq => {
                    emitter.emit(&format!("  sub a0, {lv}, {rv}"));
                    emitter.emit("  sltiu a0, a0, 1");
                }
                IrCmpOp::Ne => {
                    emitter.emit(&format!("  sub a0, {lv}, {rv}"));
                    emitter.emit("  sltu a0, zero, a0");
                }
            }
            let off = frame.local_offsets.get(dest).copied().unwrap_or(0);
            emitter.emit(&format!("  sw a0, {off}(sp)"));
        }

        IrInst::GetPtr { dest, ptr, index } => {
            let base = op_to_reg(emitter, *ptr, frame, "t0", program);
            let idx = op_to_reg(emitter, *index, frame, "t1", program);
            emitter.emit(&format!("  slli t1, {idx}, 2"));
            emitter.emit(&format!("  add t0, {base}, t1"));
            let off = frame.local_offsets.get(dest).copied().unwrap_or(0);
            emitter.emit(&format!("  sw t0, {off}(sp)"));
        }
        IrInst::GetElemPtr { dest, ptr, index } => {
            // Like getptr but the ptr already points to the element
            let base = op_to_reg(emitter, *ptr, frame, "t0", program);
            let idx = op_to_reg(emitter, *index, frame, "t1", program);
            // For array element access: ptr points to [i32, N], index into that
            emitter.emit(&format!("  slli t1, {idx}, 2"));
            emitter.emit(&format!("  add t0, {base}, t1"));
            let off = frame.local_offsets.get(dest).copied().unwrap_or(0);
            emitter.emit(&format!("  sw t0, {off}(sp)"));
        }

        IrInst::Call { dest, func: callee, args } => {
            // Save any needed registers, load args, call, restore
            let callee_name_str = program.func_name(*callee).strip_prefix('@').unwrap_or(program.func_name(*callee));
            // Save ra and caller-saved regs? For now, save nothing (matches current behavior)
            // Load arguments into a0-a7
            for (i, arg) in args.iter().enumerate() {
                if i < 8 {
                    let reg = match i { 0 => "a0", 1 => "a1", 2 => "a2", 3 => "a3", 4 => "a4", 5 => "a5", 6 => "a6", 7 => "a7", _ => continue };
                    let val = op_to_reg(emitter, *arg, frame, "t3", program);
                    emitter.emit(&format!("  mv {reg}, {val}"));
                }
            }
            emitter.emit(&format!("  call {callee_name_str}"));
            if let Some(d) = dest {
                let off = frame.local_offsets.get(d).copied().unwrap_or(0);
                emitter.emit(&format!("  sw a0, {off}(sp)"));
            }
        }

        IrInst::Br { cond, then_bb, else_bb } => {
            let c = op_to_reg(emitter, *cond, frame, "t0", program);
            let then_label = &block_labels[then_bb];
            let else_label = &block_labels[else_bb];
            emitter.emit(&format!("  bnez {c}, {then_label}"));
            emitter.emit(&format!("  j {else_label}"));
        }
        IrInst::Jump { target } => {
            let target_label = &block_labels[target];
            emitter.emit(&format!("  j {target_label}"));
        }
        IrInst::Ret { value } => {
            if let Some(v) = value {
                let val = op_to_reg(emitter, *v, frame, "a0", program);
                if val != "a0" { emitter.emit(&format!("  mv a0, {val}")); }
            }
            emitter.emit(&format!("  lw ra, {}(sp)", frame.ra_offset));
            emitter.emit(&format!("  addi sp, sp, {}", frame.frame_size));
            emitter.emit("  ret");
        }

        IrInst::Phi { .. } => {
            // Phis should have been eliminated before RISC-V emission
            emitter.emit("  # phi (unexpected — should be lowered)");
        }
        IrInst::Asm(s) => {
            emitter.emit(&format!("  {s}"));
        }
    }
}

/// Load an operand into a register. Returns the register name.
fn op_to_reg(emitter: &mut RvEmitter, op: IrOperand, frame: &FrameInfo, preferred: &str, program: &IrProgram) -> String {
    let reg = preferred.to_string();
    match op {
        IrOperand::Int(n) => {
            emitter.emit(&format!("  li {reg}, {n}"));
            reg
        }
        IrOperand::Local(i) => {
            if let Some(&off) = frame.local_offsets.get(&i) {
                emitter.emit(&format!("  lw {reg}, {off}(sp)"));
            } else if let Some(&off) = frame.alloca_offsets.get(&i) {
                emitter.emit(&format!("  lw {reg}, {off}(sp)"));
            }
            reg
        }
        IrOperand::Global(i) => {
            let name = program.global_name(i);
            if frame.alloca_offsets.contains_key(&i) {
                // Local alloca (stack-allocated)
                let off = frame.alloca_offsets[&i];
                emitter.emit(&format!("  addi {reg}, sp, {off}"));
            } else {
                // Global variable — strip @ prefix for RISC-V
                let rv_name = name.strip_prefix('@').unwrap_or(name);
                emitter.emit(&format!("  la {reg}, {rv_name}"));
            }
            reg
        }
        IrOperand::Undef => {
            emitter.emit(&format!("  li {reg}, 0"));
            reg
        }
    }
}
