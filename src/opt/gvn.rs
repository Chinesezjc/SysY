//! Global value numbering / common subexpression elimination.
//!
//! Local value numbering within each basic block: assigns a "value number"
//! to each expression, reuses previously computed values when possible.

use crate::ir::*;
use crate::opt::IrFuncPass;
use std::collections::HashMap;

pub struct GVN;

#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct ExprKey {
    op: u8,
    operands: Vec<IrOperand>,
}

impl IrFuncPass for GVN {
    fn name(&self) -> &str { "gvn" }

    fn run(&self, func: &mut IrFunc) -> bool {
        let mut changed = false;
        for block in &mut func.blocks {
            if gvn_block(block) { changed = true; }
        }
        changed
    }
}

fn gvn_block(block: &mut IrBlock) -> bool {
    let mut changed = false;
    let mut value_table: HashMap<ExprKey, (usize, usize)> = HashMap::new();
    let mut new_instrs: Vec<IrInst> = Vec::new();
    let mut replacements: HashMap<usize, IrOperand> = HashMap::new();

    for (_i, inst) in block.instrs.iter().enumerate() {
        match inst {
            IrInst::Arith { dest, op, lhs, rhs } => {
                let cl = canonical(*lhs, &replacements);
                let cr = canonical(*rhs, &replacements);
                let key = make_arith_key(*op, cl, cr);
                if let Some(&(canon_dest, _)) = value_table.get(&key) {
                    replacements.insert(*dest, IrOperand::Local(canon_dest));
                    changed = true;
                } else {
                    value_table.insert(key, (*dest, new_instrs.len()));
                    new_instrs.push(IrInst::Arith { dest: *dest, op: *op, lhs: cl, rhs: cr });
                }
            }
            IrInst::Icmp { dest, op, lhs, rhs } => {
                let cl = canonical(*lhs, &replacements);
                let cr = canonical(*rhs, &replacements);
                let key = make_icmp_key(*op, cl, cr);
                if let Some(&(canon_dest, _)) = value_table.get(&key) {
                    replacements.insert(*dest, IrOperand::Local(canon_dest));
                    changed = true;
                } else {
                    value_table.insert(key, (*dest, new_instrs.len()));
                    new_instrs.push(IrInst::Icmp { dest: *dest, op: *op, lhs: cl, rhs: cr });
                }
            }
            _ => {
                new_instrs.push(inst.clone());
            }
        }
    }

    if changed {
        for inst in &mut new_instrs {
            replace_uses(inst, &replacements);
        }
        block.instrs = new_instrs;
    }
    changed
}

fn canonical(op: IrOperand, replacements: &HashMap<usize, IrOperand>) -> IrOperand {
    match op {
        IrOperand::Local(i) => replacements.get(&i).copied().unwrap_or(op),
        _ => op,
    }
}

fn make_arith_key(op: IrArithOp, lhs: IrOperand, rhs: IrOperand) -> ExprKey {
    ExprKey { op: op as u8, operands: vec![lhs, rhs] }
}

fn make_icmp_key(op: IrCmpOp, lhs: IrOperand, rhs: IrOperand) -> ExprKey {
    ExprKey { op: 10 + op as u8, operands: vec![lhs, rhs] }
}

fn replace_uses(inst: &mut IrInst, r: &HashMap<usize, IrOperand>) {
    let mut rep = |op: &mut IrOperand| {
        if let IrOperand::Local(i) = *op {
            if let Some(&new) = r.get(&i) { *op = new; }
        }
    };
    match inst {
        IrInst::Load { src, .. } => rep(src),
        IrInst::Store { value, ptr } => { rep(value); rep(ptr); }
        IrInst::Arith { lhs, rhs, .. } | IrInst::Icmp { lhs, rhs, .. } => { rep(lhs); rep(rhs); }
        IrInst::GetPtr { ptr, index, .. } | IrInst::GetElemPtr { ptr, index, .. } => { rep(ptr); rep(index); }
        IrInst::Call { args, .. } => { for a in args { rep(a); } }
        IrInst::Br { cond, .. } => rep(cond),
        IrInst::Ret { value } => { if let Some(v) = value { rep(v); } }
        _ => {}
    }
}
