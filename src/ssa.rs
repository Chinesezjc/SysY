//! SSA construction: dominator tree, dominance frontiers, and Mem2Reg.
//!
//! Mem2Reg promotes stack-allocated scalar variables to SSA virtual registers,
//! replacing `alloc` / `load` / `store` with value forwarding.
//!
//! Currently supports single-block promotion only.
//! Multi-block promotion with phi nodes (Cytron algorithm) is scaffolded
//! in the dominator/DF infrastructure and ready to be enabled.

use crate::cfg::Cfg;
use crate::ir::*;
use std::collections::{HashMap, HashSet};

// ── Dominator tree ───────────────────────────────────────────────────────────

/// Compute immediate dominators via Cooper-Harvey-Kennedy iterative algorithm.
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
            if new_dom != dom[b] {
                dom[b] = new_dom;
                changed = true;
            }
        }
    }

    let mut idom = vec![0usize; n];
    for b in 1..n {
        let candidates: Vec<usize> = dom[b].iter().copied().filter(|&d| d != b).collect();
        idom[b] = *candidates.iter().max_by_key(|&&c| dom[c].len()).unwrap_or(&0);
    }
    idom
}

/// Build dominator tree children list from idom.
pub fn dom_tree_children(idom: &[usize]) -> Vec<Vec<usize>> {
    let n = idom.len();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    for b in 1..n {
        let parent = idom[b];
        if parent != b { children[parent].push(b); }
    }
    children
}

// ── Dominance frontier ───────────────────────────────────────────────────────

/// Compute dominance frontiers.  DF[b] = blocks where b's dominance ends.
pub fn compute_df(cfg: &Cfg, idom: &[usize]) -> Vec<HashSet<usize>> {
    let n = cfg.len();
    let mut df: Vec<HashSet<usize>> = vec![HashSet::new(); n];
    for b in 0..n {
        if cfg.predecessors[b].len() >= 2 {
            for &p in &cfg.predecessors[b] {
                let mut runner = p;
                while runner != idom[b] {
                    df[runner].insert(b);
                    runner = idom[runner];
                }
            }
        }
    }
    df
}

// ── Mem2Reg ──────────────────────────────────────────────────────────────────

struct AllocaInfo {
    alloca_name: usize,
}

/// Run Mem2Reg on a function. For now, promotes scalar allocas in single-block
/// functions only. Multi-block promotion with phi nodes requires dominance
/// verification that the definition block dominates all uses.
pub fn mem2reg(func: &mut IrFunc) -> bool {
    if func.blocks.len() != 1 {
        return false;
    }

    let allocas = find_promotable_allocas(func);
    if allocas.is_empty() { return false; }

    let mut changed = false;
    for info in &allocas {
        promote_single_block(func, info);
        changed = true;
    }
    changed
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
                            if *ptr == IrOperand::Global(*dest) {
                                def_blocks.insert(bi);
                            }
                        }
                    }
                }
                // Only promote single-def allocas (phi-free, @sc_ excluded)
                if def_blocks.len() <= 1 {
                    result.push(AllocaInfo { alloca_name: *dest });
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
                IrInst::Load { src, .. } | IrInst::Store { ptr: src, .. }
                    if *src == target => {}
                IrInst::Store { value, .. } if *value == target => {}
                _ => {
                    for op in inst.operands() {
                        if *op == target { return false; }
                    }
                }
            }
        }
    }
    true
}

/// Forward stored values to loads within a single block.
fn promote_single_block(func: &mut IrFunc, info: &AllocaInfo) {
    let mut new_blocks = Vec::new();
    for block in &func.blocks {
        let mut new_instrs = Vec::new();
        let mut cur_val: Option<IrOperand> = None;
        for inst in &block.instrs {
            match inst {
                IrInst::Alloc { dest, .. } if *dest == info.alloca_name => continue,
                IrInst::Store { value, ptr }
                    if *ptr == IrOperand::Global(info.alloca_name) =>
                {
                    cur_val = Some(*value);
                }
                IrInst::Load { dest, src }
                    if *src == IrOperand::Global(info.alloca_name) =>
                {
                    let v = cur_val.unwrap_or(IrOperand::Int(0));
                    new_instrs.push(IrInst::Arith {
                        dest: *dest,
                        op: IrArithOp::Add,
                        lhs: v,
                        rhs: IrOperand::Int(0),
                    });
                }
                _ => new_instrs.push(inst.clone()),
            }
        }
        new_blocks.push(IrBlock {
            label: block.label,
            instrs: new_instrs,
            preds: block.preds.clone(),
        });
    }
    func.blocks = new_blocks;
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn build_simple_func() -> IrFunc {
        IrFunc {
            name: 0,
            params: vec![],
            ret_type: IrType::I32,
            allocas: vec![],
            blocks: vec![IrBlock {
                label: 0,
                instrs: vec![
                    IrInst::Alloc { dest: 0, ty: IrType::I32 },
                    IrInst::Store { value: IrOperand::Int(42), ptr: IrOperand::Global(0) },
                    IrInst::Load { dest: 10, src: IrOperand::Global(0) },
                    IrInst::Ret { value: Some(IrOperand::Local(10)) },
                ],
                preds: vec![],
            }],
        }
    }

    #[test]
    fn mem2reg_simple() {
        let mut func = build_simple_func();
        assert!(mem2reg(&mut func));

        let has_alloc = func.blocks[0].instrs.iter().any(|i| matches!(i, IrInst::Alloc { .. }));
        assert!(!has_alloc);

        let has_identity = func.blocks[0].instrs.iter().any(|i| {
            matches!(i, IrInst::Arith { dest: 10, op: IrArithOp::Add, lhs: IrOperand::Int(42), rhs: IrOperand::Int(0) })
        });
        assert!(has_identity, "expected identity add, got: {:#?}", func.blocks[0].instrs);
    }
}
