//! Constant folding pass.
//!
//! Evaluates arithmetic and comparison instructions with constant operands,
//! replaces them with simpler forms, and propagates the constants.

use crate::ir::*;
use crate::opt::IrFuncPass;

pub struct ConstFold;

impl IrFuncPass for ConstFold {
    fn name(&self) -> &str { "const-fold" }

    fn run(&self, func: &mut IrFunc) -> bool {
        let mut changed = false;
        for block in &mut func.blocks {
            for inst in &mut block.instrs {
                if fold_inst(inst) {
                    changed = true;
                }
            }
        }
        changed
    }
}

/// Try to fold a single instruction. Returns true if modified.
fn fold_inst(inst: &mut IrInst) -> bool {
    match inst {
        IrInst::Arith { op, lhs, rhs, .. } => {
            let (lop, lv, rv) = (*op, *lhs, *rhs);
            if let (IrOperand::Int(l), IrOperand::Int(r)) = (lv, rv) {
                let result = match lop {
                    IrArithOp::Add => l.wrapping_add(r),
                    IrArithOp::Sub => l.wrapping_sub(r),
                    IrArithOp::Mul => l.wrapping_mul(r),
                    IrArithOp::Div => l.checked_div(r).unwrap_or(0),
                    IrArithOp::Mod => l.checked_rem(r).unwrap_or(0),
                };
                let d = inst.dest().unwrap();
                *inst = IrInst::Arith { dest: d, op: IrArithOp::Add, lhs: IrOperand::Int(result), rhs: IrOperand::Int(0) };
                return true;
            }
            // Algebraic identities
            if lop == IrArithOp::Add {
                if rv == IrOperand::Int(0) { return false; }
                if lv == IrOperand::Int(0) {
                    let d = inst.dest().unwrap();
                    *inst = IrInst::Arith { dest: d, op: IrArithOp::Add, lhs: rv, rhs: IrOperand::Int(0) };
                    return true;
                }
            }
            if lop == IrArithOp::Mul {
                if rv == IrOperand::Int(1) { return false; }
                if lv == IrOperand::Int(1) {
                    let d = inst.dest().unwrap();
                    *inst = IrInst::Arith { dest: d, op: IrArithOp::Add, lhs: rv, rhs: IrOperand::Int(0) };
                    return true;
                }
                if rv == IrOperand::Int(0) || lv == IrOperand::Int(0) {
                    let d = inst.dest().unwrap();
                    *inst = IrInst::Arith { dest: d, op: IrArithOp::Add, lhs: IrOperand::Int(0), rhs: IrOperand::Int(0) };
                    return true;
                }
            }
            false
        }
        IrInst::Icmp { op, lhs, rhs, .. } => {
            let (lop, lv, rv) = (*op, *lhs, *rhs);
            if let (IrOperand::Int(l), IrOperand::Int(r)) = (lv, rv) {
                let result = match lop {
                    IrCmpOp::Eq => (l == r) as i32,
                    IrCmpOp::Ne => (l != r) as i32,
                    IrCmpOp::Lt => (l < r) as i32,
                    IrCmpOp::Gt => (l > r) as i32,
                    IrCmpOp::Le => (l <= r) as i32,
                    IrCmpOp::Ge => (l >= r) as i32,
                };
                let d = inst.dest().unwrap();
                *inst = IrInst::Arith { dest: d, op: IrArithOp::Add, lhs: IrOperand::Int(result), rhs: IrOperand::Int(0) };
                return true;
            }
            false
        }
        IrInst::Br { cond, then_bb, else_bb } => {
            if let IrOperand::Int(v) = cond {
                let t = *then_bb;
                let e = *else_bb;
                *inst = IrInst::Jump { target: if *v != 0 { t } else { e } };
                return true;
            }
            false
        }
        _ => false,
    }
}
