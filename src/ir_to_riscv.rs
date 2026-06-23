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
}

fn compute_frame(func: &IrFunc) -> FrameInfo {
    let mut alloca_offsets = HashMap::new();
    let mut local_offsets = HashMap::new();
    let mut slot: i32 = 0;
    for block in &func.blocks {
        for inst in &block.instrs {
            if let IrInst::Alloc { dest, .. } = inst { alloca_offsets.insert(*dest, slot * 4); slot += 1; }
        }
    }
    for block in &func.blocks {
        for inst in &block.instrs {
            if let Some(dest) = inst.dest() {
                if !local_offsets.contains_key(&dest) && !alloca_offsets.contains_key(&dest) {
                    local_offsets.insert(dest, slot * 4); slot += 1;
                }
            }
        }
    }
    let total = align16(align16(slot * 4) + 4);
    FrameInfo { alloca_offsets, local_offsets, frame_size: total, ra_offset: total - 4 }
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
            IrGlobalInit::Zero => e.emit(&format!("{name}:\n  .zero {}", type_size(&g.ty))),
            IrGlobalInit::Values(vals) => {
                let mut s = format!("{name}:"); for v in vals { s.push_str(&format!("\n  .word {v}")); }
                e.emit(&s);
            }
        }
    }
    for func in &program.funcs { emit_function(&mut e, func, program); }
    e.finish()
}

fn emit_function(e: &mut RvEmitter, func: &IrFunc, program: &IrProgram) {
    let frame = compute_frame(func);
    let tf = frame.frame_size;
    let fn_name = program.func_name(func.name).strip_prefix('@').unwrap_or(program.func_name(func.name));
    e.emit(&format!("  .globl {fn_name}")); e.emit_label(fn_name);

    // Prologue
    emit_addi_sp(e, -tf);
    emit_sw(e, "ra", frame.ra_offset);

    // Store params
    for (i, (pn, _)) in func.params.iter().enumerate() {
        let reg = match i { 0=>"a0",1=>"a1",2=>"a2",3=>"a3",4=>"a4",5=>"a5",6=>"a6",7=>"a7", _=>continue };
        if let Some(&off) = frame.alloca_offsets.get(pn) { emit_sw(e, reg, off); }
        else if let Some(&off) = frame.local_offsets.get(pn) { emit_sw(e, reg, off); }
    }

    // Block labels
    let mut block_labels: HashMap<usize, String> = HashMap::new();
    for block in &func.blocks { block_labels.insert(block.label, e.fresh_label()); }

    // Emit blocks
    for block in &func.blocks {
        e.emit_label(&block_labels[&block.label]);
        for inst in &block.instrs { emit_inst(e, inst, &frame, program, &block_labels); }
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

fn emit_inst(e: &mut RvEmitter, inst: &IrInst, frame: &FrameInfo, program: &IrProgram, block_labels: &HashMap<usize, String>) {
    let lo = |i: usize| frame.local_offsets.get(&i).copied().unwrap_or(0);
    let ao = |i: usize| frame.alloca_offsets.get(&i).copied();
    let glbl = |i: usize| program.global_name(i).strip_prefix('@').unwrap_or(program.global_name(i)).to_string();

    match inst {
        IrInst::Alloc { .. } => {}

        IrInst::Load { dest, src } => {
            let addr = op_to_reg(e, *src, frame, program, "t2");
            e.emit(&format!("  lw t0, 0({addr})"));
            emit_sw(e, "t0", lo(*dest));
        }
        IrInst::Store { value, ptr } => {
            let val = op_to_reg(e, *value, frame, program, "t1");
            let p = op_to_reg(e, *ptr, frame, program, "t2");
            e.emit(&format!("  sw {val}, 0({p})"));
        }
        IrInst::Arith { dest, op, lhs, rhs } => {
            let lv = op_to_reg(e, *lhs, frame, program, "t0");
            let rv = op_to_reg(e, *rhs, frame, program, "t1");
            let ins = match op { IrArithOp::Add=>"add", IrArithOp::Sub=>"sub", IrArithOp::Mul=>"mul", IrArithOp::Div=>"div", IrArithOp::Mod=>"rem" };
            e.emit(&format!("  {ins} a0, {lv}, {rv}"));
            emit_sw(e, "a0", lo(*dest));
        }
        IrInst::Icmp { dest, op, lhs, rhs } => {
            let lv = op_to_reg(e, *lhs, frame, program, "t0");
            let rv = op_to_reg(e, *rhs, frame, program, "t1");
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
        IrInst::GetPtr { dest, ptr, index } => {
            let base = op_to_reg(e, *ptr, frame, program, "t0");
            let idx = op_to_reg(e, *index, frame, program, "t1");
            e.emit(&format!("  slli t1, {idx}, 2"));
            e.emit(&format!("  add t0, {base}, t1"));
            emit_sw(e, "t0", lo(*dest));
        }
        IrInst::GetElemPtr { dest, ptr, index } => {
            let base = op_to_reg(e, *ptr, frame, program, "t0");
            let idx = op_to_reg(e, *index, frame, program, "t1");
            e.emit(&format!("  slli t1, {idx}, 2"));
            e.emit(&format!("  add t0, {base}, t1"));
            emit_sw(e, "t0", lo(*dest));
        }
        IrInst::Call { dest, func, args } => {
            let callee = glbl(*func);
            for (i, arg) in args.iter().enumerate() {
                if i < 8 {
                    let areg = ["a0","a1","a2","a3","a4","a5","a6","a7"][i];
                    let val = op_to_reg(e, *arg, frame, program, "t3");
                    e.emit(&format!("  mv {areg}, {val}"));
                }
            }
            e.emit(&format!("  call {callee}"));
            if let Some(d) = dest { emit_sw(e, "a0", lo(*d)); }
        }
        IrInst::Br { cond, then_bb, else_bb } => {
            let c = op_to_reg(e, *cond, frame, program, "t0");
            e.emit(&format!("  bnez {c}, {}", block_labels[then_bb]));
            e.emit(&format!("  j {}", block_labels[else_bb]));
        }
        IrInst::Jump { target } => { e.emit(&format!("  j {}", block_labels[target])); }
        IrInst::Ret { value } => {
            if let Some(v) = value {
                let val = op_to_reg(e, *v, frame, program, "a0");
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

fn op_to_reg(e: &mut RvEmitter, op: IrOperand, frame: &FrameInfo, program: &IrProgram, pref: &str) -> String {
    let r = pref.to_string();
    match op {
        IrOperand::Int(n) => { e.emit(&format!("  li {r}, {n}")); r }
        IrOperand::Local(i) => { emit_lw(e, &r, frame.local_offsets.get(&i).copied().unwrap_or(0)); r }
        IrOperand::Global(i) => {
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
