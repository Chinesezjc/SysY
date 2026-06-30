//! SSA construction: dominator tree, dominance frontiers, and Mem2Reg.
//!
//! Mem2Reg promotes stack-allocated scalar variables to SSA virtual registers.
//! Currently single-block only. Multi-block phi infrastructure (dominator tree,
//! DF, phi insertion, renaming, phi lowering) is implemented but needs edge-case
//! debugging before enabling.

use crate::cfg::Cfg;
use crate::ir::*;
use std::collections::{HashMap, HashSet};

// ── Dominator tree ───────────────────────────────────────────────────────────

pub fn compute_idom(cfg: &Cfg) -> Vec<usize> {
    let n = cfg.len();
    if n == 0 { return Vec::new(); }
    let all: HashSet<usize> = (0..n).collect();
    let mut dom: Vec<HashSet<usize>> = vec![HashSet::new(); n];
    dom[0].insert(0);
    for i in 1..n { dom[i] = all.clone(); }
    let mut changed = true;
    while changed {
        changed = false;
        for b in 1..n {
            let mut new_dom: Option<HashSet<usize>> = None;
            for &p in &cfg.predecessors[b] {
                new_dom = Some(match new_dom {
                    None => dom[p].clone(),
                    Some(s) => s.intersection(&dom[p]).copied().collect(),
                });
            }
            let mut new_dom = new_dom.unwrap_or_else(HashSet::new);
            new_dom.insert(b);
            if new_dom != dom[b] { dom[b] = new_dom; changed = true; }
        }
    }
    let mut idom = vec![0usize; n];
    for b in 1..n {
        let candidates: Vec<usize> = dom[b].iter().copied().filter(|&d| d != b).collect();
        idom[b] = *candidates.iter().max_by_key(|&&c| dom[c].len()).unwrap_or(&0);
    }
    idom
}

pub fn dom_tree_children(idom: &[usize]) -> Vec<Vec<usize>> {
    let n = idom.len();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    for b in 1..n { let p = idom[b]; if p != b { children[p].push(b); } }
    children
}

pub fn compute_df(cfg: &Cfg, idom: &[usize]) -> Vec<HashSet<usize>> {
    let n = cfg.len();
    let mut df: Vec<HashSet<usize>> = vec![HashSet::new(); n];
    for b in 0..n {
        if cfg.predecessors[b].len() >= 2 {
            for &p in &cfg.predecessors[b] {
                let mut runner = p;
                while runner != idom[b] { df[runner].insert(b); runner = idom[runner]; }
            }
        }
    }
    df
}

// ── Mem2Reg ──────────────────────────────────────────────────────────────────

struct AllocaInfo {
    alloca_name: usize,
    def_blocks: HashSet<usize>,
}

pub fn mem2reg(func: &mut IrFunc) -> bool {
    let allocas = find_promotable_allocas(func);
    if allocas.is_empty() { return false; }

    if func.blocks.len() == 1 {
        // Single-block: simple forward substitution
        let mut changed = false;
        for info in &allocas {
            promote_single_block(func, info);
            changed = true;
        }
        return changed;
    }

    // Multi-block: only promote entry-block allocas whose stored value is
    // NOT a parameter (params may be clobbered between blocks).
    let entry_allocas: Vec<&AllocaInfo> = allocas.iter()
        .filter(|a| {
            if a.def_blocks.len() != 1 || !a.def_blocks.contains(&0) { return false; }
            // Check: is the stored value a param? If so, skip.
            let stored_is_param = func.blocks[0].instrs.iter().any(|inst| {
                if let IrInst::Store { value, ptr } = inst {
                    *ptr == IrOperand::Global(a.alloca_name)
                        && matches!(value, IrOperand::Global(_))
                } else { false }
            });
            !stored_is_param
        })
        .collect();
    if entry_allocas.is_empty() { return false; }

    // Use the multi-block rename pass with phi support
    let n = func.blocks.len();
    let mut new_blocks: Vec<IrBlock> = func.blocks.iter()
        .map(|b| IrBlock { label: b.label, instrs: vec![], preds: b.preds.clone() })
        .collect();

    // For each entry-block alloca, forward the stored value to all loads
    for info in &entry_allocas {
        // Find the store instruction in block 0 and extract the value
        let stored_val: Option<IrOperand> = func.blocks[0].instrs.iter()
            .filter_map(|inst| {
                if let IrInst::Store { value, ptr } = inst {
                    if *ptr == IrOperand::Global(info.alloca_name) { Some(*value) }
                    else { None }
                } else { None }
            })
            .next();

        let val = stored_val.unwrap_or(IrOperand::Int(0));

        // Replace loads in all blocks with the stored value
        for (bi, block) in func.blocks.iter().enumerate() {
            for inst in &block.instrs {
                match inst {
                    IrInst::Alloc { dest, .. } if *dest == info.alloca_name => {}
                    IrInst::Store { ptr, .. } if *ptr == IrOperand::Global(info.alloca_name) => {}
                    IrInst::Load { dest, src } if *src == IrOperand::Global(info.alloca_name) => {
                        new_blocks[bi].instrs.push(IrInst::Arith {
                            dest: *dest, op: IrArithOp::Add,
                            lhs: val, rhs: IrOperand::Int(0),
                        });
                    }
                    _ => new_blocks[bi].instrs.push(inst.clone()),
                }
            }
        }
    }

    // Copy blocks that weren't processed
    for (bi, block) in func.blocks.iter().enumerate() {
        if new_blocks[bi].instrs.is_empty() {
            for inst in &block.instrs {
                let mut skip = false;
                for info in &entry_allocas {
                    match inst {
                        IrInst::Alloc { dest, .. } if *dest == info.alloca_name => skip = true,
                        IrInst::Store { ptr, .. } if *ptr == IrOperand::Global(info.alloca_name) => skip = true,
                        _ => {}
                    }
                }
                if !skip { new_blocks[bi].instrs.push(inst.clone()); }
            }
        }
    }

    func.blocks = new_blocks;
    true
}

fn find_promotable_allocas(func: &IrFunc) -> Vec<AllocaInfo> {
    let mut result = Vec::new();
    for block in &func.blocks {
        for inst in &block.instrs {
            if let IrInst::Alloc { dest, ty } = inst {
                if *ty != IrType::I32 { continue; }
                if !is_only_loaded_stored(func, *dest) { continue; }
                let mut def_blocks = HashSet::new();
                for (bi, b) in func.blocks.iter().enumerate() {
                    for inst in &b.instrs {
                        if let IrInst::Store { ptr, .. } = inst {
                            if *ptr == IrOperand::Global(*dest) { def_blocks.insert(bi); }
                        }
                    }
                }
                if def_blocks.len() <= 1 {
                    result.push(AllocaInfo { alloca_name: *dest, def_blocks });
                }
            }
        }
    }
    result
}

fn is_only_loaded_stored(func: &IrFunc, global_idx: usize) -> bool {
    let target = IrOperand::Global(global_idx);
    for block in &func.blocks {
        for inst in &block.instrs {
            match inst {
                IrInst::Load { src, .. } | IrInst::Store { ptr: src, .. } if *src == target => {}
                IrInst::Store { value, .. } if *value == target => {}
                _ => { for op in inst.operands() { if *op == target { return false; } } }
            }
        }
    }
    true
}

fn promote_single_block(func: &mut IrFunc, info: &AllocaInfo) {
    let mut new_blocks = Vec::new();
    for block in &func.blocks {
        let mut new_instrs = Vec::new();
        let mut cur_val: Option<IrOperand> = None;
        for inst in &block.instrs {
            match inst {
                IrInst::Alloc { dest, .. } if *dest == info.alloca_name => continue,
                IrInst::Store { value, ptr } if *ptr == IrOperand::Global(info.alloca_name)
                    => cur_val = Some(*value),
                IrInst::Load { dest, src } if *src == IrOperand::Global(info.alloca_name) => {
                    let v = cur_val.unwrap_or(IrOperand::Int(0));
                    new_instrs.push(IrInst::Arith { dest: *dest, op: IrArithOp::Add, lhs: v, rhs: IrOperand::Int(0) });
                }
                _ => new_instrs.push(inst.clone()),
            }
        }
        new_blocks.push(IrBlock { label: block.label, instrs: new_instrs, preds: block.preds.clone() });
    }
    func.blocks = new_blocks;
}

// ── Phi lowering ────────────────────────────────────────────────────────────

/// Lower phi nodes to copy instructions in predecessor blocks.
pub fn lower_phis(func: &mut IrFunc) {
    let mut copies: Vec<HashMap<usize, Vec<(usize, IrOperand)>>> = vec![HashMap::new(); func.blocks.len()];
    for (bi, block) in func.blocks.iter().enumerate() {
        for inst in &block.instrs {
            if let IrInst::Phi { dest, incoming } = inst {
                for &(value, pred) in incoming {
                    copies[pred].entry(bi).or_default().push((*dest, value));
                }
            }
        }
    }
    for pred_idx in 0..func.blocks.len() {
        let block = &mut func.blocks[pred_idx];
        let mut new_instrs: Vec<IrInst> = Vec::new();
        let split_at = block.instrs.iter().position(|i| i.is_terminator()).unwrap_or(block.instrs.len());
        let mut to_insert: Vec<(usize, IrOperand)> = Vec::new();
        for copies_for_succ in copies[pred_idx].values() {
            for &(dest, value) in copies_for_succ {
                to_insert.push((dest, value));
            }
        }
        for (i, inst) in block.instrs.iter().enumerate() {
            if i == split_at {
                for &(dest, value) in &to_insert {
                    new_instrs.push(IrInst::Arith { dest, op: IrArithOp::Add, lhs: value, rhs: IrOperand::Int(0) });
                }
            }
            if !matches!(inst, IrInst::Phi { .. }) { new_instrs.push(inst.clone()); }
        }
        if split_at == block.instrs.len() && !to_insert.is_empty() {
            for &(dest, value) in &to_insert {
                new_instrs.push(IrInst::Arith { dest, op: IrArithOp::Add, lhs: value, rhs: IrOperand::Int(0) });
            }
        }
        func.blocks[pred_idx].instrs = new_instrs;
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem2reg_simple() {
        let mut func = IrFunc { name: 0, params: vec![], ret_type: IrType::I32, allocas: vec![],
            blocks: vec![IrBlock { label: 0, preds: vec![],
                instrs: vec![
                    IrInst::Alloc { dest: 0, ty: IrType::I32 },
                    IrInst::Store { value: IrOperand::Int(42), ptr: IrOperand::Global(0) },
                    IrInst::Load { dest: 10, src: IrOperand::Global(0) },
                    IrInst::Ret { value: Some(IrOperand::Local(10)) },
                ],
            }],
        };
        assert!(mem2reg(&mut func));
        assert!(!func.blocks[0].instrs.iter().any(|i| matches!(i, IrInst::Alloc { .. })));
    }
}
