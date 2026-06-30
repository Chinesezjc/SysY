//! SSA construction: dominator tree, dominance frontiers, and Mem2Reg.
//!
//! Implements the Cytron et al. algorithm for promoting stack-allocated scalar
//! variables to SSA virtual registers with phi nodes at iterated dominance
//! frontiers.

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
    for b in 1..n {
        let parent = idom[b];
        if parent != b { children[parent].push(b); }
    }
    children
}

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

pub fn mem2reg(func: &mut IrFunc) -> bool {
    // Multi-block phi infrastructure is complete but @sc_ filtering
    // and dominance verification need more work. For now, only
    // single-block promotion is enabled.
    if func.blocks.len() != 1 {
        return false;
    }

    let allocas = find_promotable_allocas(func);
    if allocas.is_empty() { return false; }

    let n = func.blocks.len();
    let cfg = Cfg::build(func);
    let idom = compute_idom(&cfg);
    let df = compute_df(&cfg, &idom);
    let dom_children = dom_tree_children(&idom);

    // Build dominator sets for quick dominance checks
    let dominates = build_dominates(&cfg, &idom);

    // Build successor lists
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); n];
    for b in 0..n {
        for &p in &cfg.predecessors[b] {
            succ[p].push(b);
        }
    }

    // Step 1: Insert phi nodes for each alloca at iterated dominance frontiers
    // phi_nodes[block][alloca] = phi_dest
    let mut phi_nodes: Vec<HashMap<usize, usize>> = vec![HashMap::new(); n];

    for info in &allocas {
        let mut worklist: Vec<usize> = info.def_blocks.iter().copied().collect();
        let mut has_phi: HashSet<usize> = HashSet::new();
        // Also include blocks that use the alloca but aren't dominated by a def
        for (bi, b) in func.blocks.iter().enumerate() {
            for inst in &b.instrs {
                if let IrInst::Load { src, .. } = inst {
                    if *src == IrOperand::Global(info.alloca_name) {
                        if !info.def_blocks.iter().any(|&d| dominates[d][bi]) {
                            worklist.push(bi);
                        }
                    }
                }
            }
        }
        while let Some(b) = worklist.pop() {
            for &d in &df[b] {
                if !has_phi.contains(&d) {
                    let phi_dest = fresh_local(func);
                    phi_nodes[d].insert(info.alloca_name, phi_dest);
                    has_phi.insert(d);
                    worklist.push(d);
                }
            }
        }
    }

    // Step 2: Rename — walk dominator tree
    let mut stacks: HashMap<usize, Vec<IrOperand>> = HashMap::new();
    for info in &allocas {
        stacks.insert(info.alloca_name, Vec::new());
    }

    let mut new_blocks: Vec<IrBlock> = func.blocks.iter()
        .map(|b| IrBlock { label: b.label, instrs: Vec::new(), preds: b.preds.clone() })
        .collect();

    rename(
        0, &func.blocks, &dom_children, &succ, &phi_nodes,
        &mut stacks, &mut new_blocks,
    );

    func.blocks = new_blocks;
    true
}

/// DFS rename pass.
fn rename(
    b: usize,
    old_blocks: &[IrBlock],
    dom_children: &[Vec<usize>],
    succ: &[Vec<usize>],
    phi_nodes: &[HashMap<usize, usize>],
    stacks: &mut HashMap<usize, Vec<IrOperand>>,
    new_blocks: &mut [IrBlock],
) {
    let mut pushed_phi: Vec<usize> = Vec::new();
    let mut pushed_store: Vec<usize> = Vec::new();

    // 1. Emit phi instructions and set new reaching definitions
    for (&alloca, &phi_dest) in &phi_nodes[b] {
        let stack = stacks.get_mut(&alloca).unwrap();
        stack.push(IrOperand::Local(phi_dest));
        pushed_phi.push(alloca);
        new_blocks[b].instrs.push(IrInst::Phi {
            dest: phi_dest,
            incoming: Vec::new(),
        });
    }

    // 2. Rewrite instructions
    for inst in &old_blocks[b].instrs {
        match inst {
            IrInst::Alloc { dest, .. } if stacks.contains_key(dest) => {}
            IrInst::Store { value, ptr } => {
                if let IrOperand::Global(g) = *ptr {
                    if let Some(stack) = stacks.get_mut(&g) {
                        stack.push(*value);
                        pushed_store.push(g);
                    } else {
                        new_blocks[b].instrs.push(inst.clone());
                    }
                } else {
                    new_blocks[b].instrs.push(inst.clone());
                }
            }
            IrInst::Load { dest, src } => {
                if let IrOperand::Global(g) = *src {
                    if let Some(stack) = stacks.get(&g) {
                        let v = stack.last().copied().unwrap_or(IrOperand::Undef);
                        new_blocks[b].instrs.push(IrInst::Arith {
                            dest: *dest,
                            op: IrArithOp::Add,
                            lhs: v,
                            rhs: IrOperand::Int(0),
                        });
                    } else {
                        new_blocks[b].instrs.push(inst.clone());
                    }
                } else {
                    new_blocks[b].instrs.push(inst.clone());
                }
            }
            _ => new_blocks[b].instrs.push(inst.clone()),
        }
    }

    // 3. Fill phi incoming values in successors
    for &s in &succ[b] {
        for (&alloca, &phi_dest) in &phi_nodes[s] {
            let cur = stacks.get(&alloca)
                .and_then(|s| s.last().copied())
                .unwrap_or(IrOperand::Undef);
            for inst in &mut new_blocks[s].instrs {
                if let IrInst::Phi { dest, incoming } = inst {
                    if *dest == phi_dest {
                        incoming.push((cur, b));
                        break;
                    }
                }
            }
        }
    }

    // 4. Recurse into dominator tree children
    for &child in &dom_children[b] {
        rename(child, old_blocks, dom_children, succ, phi_nodes, stacks, new_blocks);
    }

    // 5. Pop values pushed in this block
    for _ in 0..pushed_store.len() {
        let alloca = pushed_store.pop().unwrap();
        stacks.get_mut(&alloca).unwrap().pop();
    }
    for _ in 0..pushed_phi.len() {
        let alloca = pushed_phi.pop().unwrap();
        stacks.get_mut(&alloca).unwrap().pop();
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

struct AllocaInfo {
    alloca_name: usize,
    def_blocks: HashSet<usize>,
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
                // Only promote single-def allocas. Multi-def allocas
                // (@sc_ short-circuit temps, loop variables) need
                // more analysis to distinguish promotable from not.
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

fn fresh_local(func: &IrFunc) -> usize {
    let mut max: usize = 0;
    for block in &func.blocks {
        for inst in &block.instrs {
            if let Some(d) = inst.dest() { max = max.max(d); }
            for op in inst.operands() {
                if let IrOperand::Local(l) = *op { max = max.max(l); }
            }
        }
    }
    max + 1
}

fn build_dominates(cfg: &Cfg, idom: &[usize]) -> Vec<Vec<bool>> {
    let n = cfg.len();
    let mut dom = vec![vec![false; n]; n];
    // dom[i][j] = true iff block i dominates block j
    for i in 0..n {
        let mut cur = i;
        loop {
            dom[cur][i] = true;
            if cur == 0 { break; }
            cur = idom[cur];
        }
    }
    dom
}

// ── Phi lowering ────────────────────────────────────────────────────────────

/// Lower phi nodes to copy instructions in predecessor blocks.
/// After this pass, all phi nodes are removed from the function.
pub fn lower_phis(func: &mut IrFunc) {
    // Collect phi info: for each block, gather (dest, value, pred_block)
    let mut copies: Vec<HashMap<usize, Vec<(usize, IrOperand)>>> = vec![HashMap::new(); func.blocks.len()];
    // copies[pred_block][block_containing_phi] = list of (dest, value) to append

    for (bi, block) in func.blocks.iter().enumerate() {
        for inst in &block.instrs {
            if let IrInst::Phi { dest, incoming } = inst {
                for &(value, pred) in incoming {
                    copies[pred].entry(bi).or_default().push((*dest, value));
                }
            }
        }
    }

    // Insert copy instructions before terminators in predecessor blocks
    for pred_idx in 0..func.blocks.len() {
        let block = &mut func.blocks[pred_idx];
        let mut new_instrs: Vec<IrInst> = Vec::new();
        let terminator_pos = block.instrs.iter().position(|i| i.is_terminator());

        // Split: non-terminator instructions, then phi copies, then terminator
        let split_at = terminator_pos.unwrap_or(block.instrs.len());
        // Gather copies for all successor blocks that had phis
        let mut to_insert: Vec<(usize, IrOperand)> = Vec::new();
        for (&succ_idx, copies_for_succ) in &copies[pred_idx] {
            for &(dest, value) in copies_for_succ {
                to_insert.push((dest, value));
            }
        }

        for (i, inst) in block.instrs.iter().enumerate() {
            if i == split_at {
                // Insert phi copies before terminator
                for &(dest, value) in &to_insert {
                    new_instrs.push(IrInst::Arith {
                        dest,
                        op: IrArithOp::Add,
                        lhs: value,
                        rhs: IrOperand::Int(0),
                    });
                }
            }
            if !matches!(inst, IrInst::Phi { .. }) {
                new_instrs.push(inst.clone());
            }
        }

        // If no terminator found, append copies at end
        if split_at == block.instrs.len() && !to_insert.is_empty() {
            for &(dest, value) in &to_insert {
                new_instrs.push(IrInst::Arith {
                    dest,
                    op: IrArithOp::Add,
                    lhs: value,
                    rhs: IrOperand::Int(0),
                });
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
        let mut func = IrFunc {
            name: 0, params: vec![], ret_type: IrType::I32, allocas: vec![],
            blocks: vec![IrBlock {
                label: 0, preds: vec![],
                instrs: vec![
                    IrInst::Alloc { dest: 0, ty: IrType::I32 },
                    IrInst::Store { value: IrOperand::Int(42), ptr: IrOperand::Global(0) },
                    IrInst::Load { dest: 10, src: IrOperand::Global(0) },
                    IrInst::Ret { value: Some(IrOperand::Local(10)) },
                ],
            }],
        };
        assert!(mem2reg(&mut func));
        let has_alloc = func.blocks[0].instrs.iter().any(|i| matches!(i, IrInst::Alloc { .. }));
        assert!(!has_alloc);
    }

    #[test]
    fn mem2reg_multi_block() {
        let mut func = IrFunc {
            name: 0, params: vec![], ret_type: IrType::I32, allocas: vec![],
            blocks: vec![
                IrBlock {
                    label: 0, preds: vec![],
                    instrs: vec![
                        IrInst::Alloc { dest: 0, ty: IrType::I32 },
                        IrInst::Store { value: IrOperand::Int(10), ptr: IrOperand::Global(0) },
                        IrInst::Jump { target: 1 },
                    ],
                },
                IrBlock {
                    label: 1, preds: vec![0],
                    instrs: vec![
                        IrInst::Load { dest: 11, src: IrOperand::Global(0) },
                        IrInst::Ret { value: Some(IrOperand::Local(11)) },
                    ],
                },
            ],
        };
        assert!(mem2reg(&mut func));
        let has_alloc = func.blocks.iter().any(|b| b.instrs.iter().any(|i| matches!(i, IrInst::Alloc{..})));
        assert!(!has_alloc, "alloca should be removed");
    }
}
