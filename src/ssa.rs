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

    // Single-block: simple forward substitution
    if func.blocks.len() == 1 {
        let mut changed = false;
        for info in &allocas {
            promote_single_block(func, info);
            changed = true;
        }
        return changed;
    }

    // Multi-block: classify allocas
    let n = func.blocks.len();
    let cfg = Cfg::build(func);
    let idom = compute_idom(&cfg);
    let dom = build_dominates(&cfg, &idom);
    let df = compute_df(&cfg, &idom);
    let dom_children = dom_tree_children(&idom);

    // Build successor lists
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); n];
    for b in 0..n { for &p in &cfg.predecessors[b] { succ[p].push(b); } }

    // Classify: single-def → phi-free promotion; multi-def with dominant chain → phi
    let mut phi_allocas: Vec<usize> = Vec::new();
    let mut simple_allocas: Vec<(&AllocaInfo, IrOperand)> = Vec::new();

    for info in &allocas {
        // Check if stored value is a param (Global) — skip promotion
        let stored_is_param = func.blocks[0].instrs.iter().any(|inst| {
            if let IrInst::Store { value, ptr } = inst {
                *ptr == IrOperand::Global(info.alloca_name) && matches!(value, IrOperand::Global(_))
            } else { false }
        });
        if stored_is_param { continue; }

        if info.def_blocks.len() == 1 && info.def_blocks.contains(&0) {
            // Entry-block single-def: constant/expression init
            let stored_val = func.blocks[0].instrs.iter().find_map(|inst| {
                if let IrInst::Store { value, ptr } = inst {
                    if *ptr == IrOperand::Global(info.alloca_name) { Some(*value) }
                    else { None }
                } else { None }
            });
            if let Some(val) = stored_val {
                simple_allocas.push((info, val));
                continue;
            }
        }
        // Multi-def with dominant chain → phi promotion candidate.
        let has_arrays_or_calls = func.blocks.iter().any(|b| b.instrs.iter().any(|i| {
            matches!(i, IrInst::Call{..}) || matches!(i, IrInst::Alloc { ty: IrType::Array(..), .. })
        }));
        if info.def_blocks.len() >= 2 && !has_arrays_or_calls {
            let has_dom = info.def_blocks.iter().any(|&d1|
                info.def_blocks.iter().any(|&d2| d1 != d2 && dom[d1][d2])
            );
            if has_dom {
                phi_allocas.push(info.alloca_name);
            }
        }
    }

    if simple_allocas.is_empty() && phi_allocas.is_empty() { return false; }

    // Step 1: Insert phi nodes (before rename so incomings can be filled)
    let mut phi_nodes: Vec<HashMap<usize, usize>> = vec![HashMap::new(); n];
    let mut fresh_cnt: usize = 0;
    for &alloca in &phi_allocas {
        let info = allocas.iter().find(|a| a.alloca_name == alloca).unwrap();
        let mut worklist: Vec<usize> = info.def_blocks.iter().copied().collect();
        let mut has_phi: HashSet<usize> = HashSet::new();
        while let Some(b) = worklist.pop() {
            for &d in &df[b] {
                if !has_phi.contains(&d) {
                    let phi_dest = fresh_local(func, &mut fresh_cnt);
                    phi_nodes[d].insert(alloca, phi_dest);
                    has_phi.insert(d);
                    worklist.push(d);
                }
            }
        }
    }

    // Pre-insert phi instructions into new_blocks (empty incoming)
    let mut new_blocks: Vec<IrBlock> = func.blocks.iter()
        .map(|b| IrBlock { label: b.label, instrs: vec![], preds: b.preds.clone() })
        .collect();
    for bi in 0..n {
        for (&_alloca, &phi_dest) in &phi_nodes[bi] {
            new_blocks[bi].instrs.push(IrInst::Phi { dest: phi_dest, incoming: Vec::new() });
        }
    }

    // Step 2: Rename — fill incomings + rewrite Load/Store
    let mut stacks: HashMap<usize, Vec<IrOperand>> = HashMap::new();
    for &alloca in &phi_allocas { stacks.insert(alloca, Vec::new()); }
    for (info, _) in &simple_allocas { stacks.insert(info.alloca_name, Vec::new()); }

    let all_promoted: HashSet<usize> = phi_allocas.iter().copied()
        .chain(simple_allocas.iter().map(|(a, _)| a.alloca_name))
        .collect();

    rename_multi(0, &func.blocks, &dom_children, &succ, &phi_nodes,
        &mut stacks, &mut new_blocks, &all_promoted);

    // Step 3: Apply simple_allocas (replace loads with known value)
    for (info, val) in &simple_allocas {
        for bi in 0..n {
            let mut new_instrs = Vec::new();
            for inst in &new_blocks[bi].instrs {
                match inst {
                    IrInst::Load { dest, src } if *src == IrOperand::Global(info.alloca_name) => {
                        new_instrs.push(IrInst::Arith {
                            dest: *dest, op: IrArithOp::Add, lhs: *val, rhs: IrOperand::Int(0),
                        });
                    }
                    _ => new_instrs.push(inst.clone()),
                }
            }
            new_blocks[bi].instrs = new_instrs;
        }
    }

    // Remove Alloc/Store for promoted allocas
    for bi in 0..n {
        new_blocks[bi].instrs.retain(|inst| {
            match inst {
                IrInst::Alloc { dest, .. } => !all_promoted.contains(dest),
                IrInst::Store { ptr, .. } => {
                    if let IrOperand::Global(g) = *ptr { !all_promoted.contains(&g) }
                    else { true }
                }
                _ => true,
            }
        });
    }

    func.blocks = new_blocks;
    true
}

/// Multi-block rename pass (handles phi nodes + simple Load/Store).
fn rename_multi(
    b: usize, old: &[IrBlock], dom_ch: &[Vec<usize>], succ: &[Vec<usize>],
    phi_nodes: &[HashMap<usize, usize>], stacks: &mut HashMap<usize, Vec<IrOperand>>,
    new: &mut [IrBlock], promoted: &HashSet<usize>,
) {
    let mut pushed_phi: Vec<usize> = Vec::new();
    let mut pushed_store: Vec<usize> = Vec::new();

    // 1. Define phi values (phis already inserted in new_blocks pre-pass)
    for (&alloca, &phi_dest) in &phi_nodes[b] {
        stacks.get_mut(&alloca).unwrap().push(IrOperand::Local(phi_dest));
        pushed_phi.push(alloca);
    }

    // 2. Rewrite
    for inst in &old[b].instrs {
        match inst {
            IrInst::Store { value, ptr } => {
                if let IrOperand::Global(g) = *ptr {
                    if promoted.contains(&g) {
                        stacks.get_mut(&g).unwrap().push(*value);
                        pushed_store.push(g);
                        continue;
                    }
                }
                new[b].instrs.push(inst.clone());
            }
            IrInst::Load { dest, src } => {
                if let IrOperand::Global(g) = *src {
                    if promoted.contains(&g) {
                        let v = stacks.get(&g).and_then(|s| s.last().copied())
                            .unwrap_or(IrOperand::Undef);
                        new[b].instrs.push(IrInst::Arith {
                            dest: *dest, op: IrArithOp::Add, lhs: v, rhs: IrOperand::Int(0),
                        });
                        continue;
                    }
                }
                new[b].instrs.push(inst.clone());
            }
            _ => new[b].instrs.push(inst.clone()),
        }
    }

    // 3. Fill phi incoming in successors
    for &s in &succ[b] {
        for (&alloca, &phi_dest) in &phi_nodes[s] {
            let cur = stacks.get(&alloca).and_then(|s| s.last().copied())
                .unwrap_or(IrOperand::Undef);
            for inst in &mut new[s].instrs {
                if let IrInst::Phi { dest, incoming } = inst {
                    if *dest == phi_dest { incoming.push((cur, b)); break; }
                }
            }
        }
    }

    // 4. Recurse
    for &child in &dom_ch[b] {
        rename_multi(child, old, dom_ch, succ, phi_nodes, stacks, new, promoted);
    }

    // 5. Pop
    for _ in 0..pushed_store.len() { let a = pushed_store.pop().unwrap(); stacks.get_mut(&a).unwrap().pop(); }
    for _ in 0..pushed_phi.len() { let a = pushed_phi.pop().unwrap(); stacks.get_mut(&a).unwrap().pop(); }
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
                result.push(AllocaInfo { alloca_name: *dest, def_blocks });
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

// ── Helpers ──────────────────────────────────────────────────────────────────

fn build_dominates(cfg: &Cfg, idom: &[usize]) -> Vec<Vec<bool>> {
    let n = cfg.len();
    let mut dom = vec![vec![false; n]; n];
    for i in 0..n {
        let mut cur = i;
        loop { dom[cur][i] = true; if cur == 0 { break; } cur = idom[cur]; }
    }
    dom
}

fn fresh_local(func: &IrFunc, counter: &mut usize) -> usize {
    if *counter == 0 {
        let mut max: usize = 0;
        for b in &func.blocks { for i in &b.instrs {
            if let Some(d) = i.dest() { max = max.max(d); }
            for op in i.operands() { if let IrOperand::Local(l) = *op { max = max.max(l); } }
        }}
        // Use a safe base well above any existing local index
        *counter = (max + 1).max(1000);
    }
    let val = *counter;
    *counter += 1;
    val
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
