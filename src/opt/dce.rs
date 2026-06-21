//! Dead code elimination — removes unused instructions.
//!
//! Conservative approach: only remove instructions whose destination local
//! is never referenced by any other instruction in the function.

use crate::ir::*;
use crate::opt::IrFuncPass;
use std::collections::HashSet;

pub struct DeadCodeElim;

impl IrFuncPass for DeadCodeElim {
    fn name(&self) -> &str { "dce" }

    fn run(&self, func: &mut IrFunc) -> bool {
        let mut changed = false;

        // Collect all used locals AND globals across the function.
        // (Allocas use global indices for dests; Load/Store use global operands.)
        let mut used_locals: HashSet<usize> = HashSet::new();
        let mut used_globals: HashSet<usize> = HashSet::new();
        for block in &func.blocks {
            for inst in &block.instrs {
                for op in inst.operands() {
                    match op {
                        IrOperand::Local(idx) => { used_locals.insert(*idx); }
                        IrOperand::Global(idx) => { used_globals.insert(*idx); }
                        _ => {}
                    }
                }
                // Also mark Alloc dests as "used" (they're needed for Load/Store)
                if let IrInst::Alloc { dest, .. } = inst {
                    used_globals.insert(*dest);
                }
            }
        }

        // Remove unused instructions (no side effects, dest not used)
        for block in &mut func.blocks {
            let old_len = block.instrs.len();
            block.instrs.retain(|inst| {
                if is_critical(inst) {
                    return true;
                }
                if let Some(dest) = inst.dest() {
                    // Check both: could be local or global dest
                    if used_locals.contains(&dest) || used_globals.contains(&dest) {
                        return true;
                    }
                    false // unused, remove
                } else {
                    true // no dest, keep
                }
            });
            if block.instrs.len() != old_len {
                changed = true;
            }
        }

        changed
    }
}

fn is_critical(inst: &IrInst) -> bool {
    matches!(
        inst,
        IrInst::Store { .. }
            | IrInst::Call { .. }
            | IrInst::Br { .. }
            | IrInst::Jump { .. }
            | IrInst::Ret { .. }
            | IrInst::Asm(_)
    )
}
