//! SSA construction: dominator tree, dominance frontiers, and Mem2Reg.
//!
//! Mem2Reg promotes stack-allocated scalar variables to SSA virtual registers,
//! replacing `alloc` / `load` / `store` with `phi` nodes and value renaming.

use crate::cfg::Cfg;
use crate::ir::*;
use std::collections::{HashMap, HashSet, VecDeque};

// ── Dominator tree ───────────────────────────────────────────────────────────

/// Immediate dominators: `idom[b] = c` means block c immediately dominates b.
/// The entry block has `idom[entry] = entry`.
pub fn compute_idom(cfg: &Cfg) -> Vec<usize> {
    let n = cfg.len();
    if n == 0 {
        return Vec::new();
    }

    // Initialize: entry dominates itself, all others dominated by everything
    let mut dom: Vec<HashSet<usize>> = vec![HashSet::new(); n];
    let all: HashSet<usize> = (0..n).collect();
    dom[0].insert(0);
    for i in 1..n {
        dom[i] = all.clone();
    }

    // Cooper-Harvey-Kennedy iterative algorithm
    let mut changed = true;
    while changed {
        changed = false;
        for b in 1..n {
            // new_dom = intersection of dom[p] for all predecessors p
            let mut new_dom: Option<HashSet<usize>> = None;
            for &p in &cfg.predecessors[b] {
                new_dom = Some(match new_dom {
                    None => dom[p].clone(),
                    Some(s) => s.intersection(&dom[p]).copied().collect(),
                });
            }
            let mut new_dom = new_dom.unwrap_or_else(HashSet::new);
            new_dom.insert(b); // block dominates itself
            if new_dom != dom[b] {
                dom[b] = new_dom;
                changed = true;
            }
        }
    }

    // Compute immediate dominator from dom sets
    let mut idom = vec![0usize; n];
    idom[0] = 0; // entry's idom is itself
    for b in 1..n {
        let mut candidates: Vec<usize> = dom[b].iter().copied().filter(|&d| d != b).collect();
        // idom[b] is the unique node in dom[b]-{b} that dominates all others
        // (strict dominator that is closest to b)
        idom[b] = *candidates
            .iter()
            .max_by_key(|&&c| dom[c].len())
            .unwrap_or(&0);
    }

    idom
}

/// Build dominator tree children list from idom.
pub fn dom_tree_children(idom: &[usize]) -> Vec<Vec<usize>> {
    let n = idom.len();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    for b in 1..n {
        let parent = idom[b];
        if parent != b {
            children[parent].push(b);
        }
    }
    children
}

// ── Dominance frontier ───────────────────────────────────────────────────────

/// Compute dominance frontiers.
/// DF[b] = blocks where b's dominance stops (first join points from b).
pub fn compute_df(cfg: &Cfg, idom: &[usize]) -> Vec<HashSet<usize>> {
    let n = cfg.len();
    let mut df: Vec<HashSet<usize>> = vec![HashSet::new(); n];
    for b in 0..n {
        let preds = &cfg.predecessors[b];
        if preds.len() >= 2 {
            for &p in preds {
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

/// Information about a promotable alloca.
struct AllocaInfo {
    /// The alloca instruction's dest (global index).
    alloca_name: usize,
    /// Alloca type (must be scalar).
    alloca_type: IrType,
    /// Blocks containing stores to this alloca (definition sites).
    def_blocks: HashSet<usize>,
    /// Blocks containing loads from this alloca (use sites).
    use_blocks: HashSet<usize>,
}

/// Run the Mem2Reg pass on a function.
/// Returns `true` if any alloca was promoted.
pub fn mem2reg(func: &mut IrFunc) -> bool {
    let cfg = Cfg::build(func);
    let idom = compute_idom(&cfg);
    let df = compute_df(&cfg, &idom);
    let dom_children = dom_tree_children(&idom);

    // Find promotable allocas
    let allocas = find_promotable_allocas(func);

    if allocas.is_empty() {
        return false;
    }

    for info in &allocas {
        promote_alloca(func, info, &cfg, &idom, &df, &dom_children);
    }

    true
}

/// Find all allocas that can be promoted: scalar type, not address-taken.
fn find_promotable_allocas(func: &IrFunc) -> Vec<AllocaInfo> {
    // For now: collect allocas from the entry block
    let mut result = Vec::new();

    for block in &func.blocks {
        for inst in &block.instrs {
            if let IrInst::Alloc { dest, ty } = inst {
                // Only promote scalar allocas (i32), not arrays
                if *ty != IrType::I32 {
                    continue;
                }
                // Check if the alloca is only used by Load/Store
                if is_only_loaded_stored(func, *dest) {
                    let mut def_blocks = HashSet::new();
                    let mut use_blocks = HashSet::new();

                    for (bi, b) in func.blocks.iter().enumerate() {
                        for inst in &b.instrs {
                            match inst {
                                IrInst::Store { ptr, .. } => {
                                    if ptr == &IrOperand::Global(*dest) {
                                        def_blocks.insert(bi);
                                    }
                                }
                                IrInst::Load { src, .. } => {
                                    if src == &IrOperand::Global(*dest) {
                                        use_blocks.insert(bi);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }

                    result.push(AllocaInfo {
                        alloca_name: *dest,
                        alloca_type: ty.clone(),
                        def_blocks,
                        use_blocks,
                    });
                }
            }
        }
    }

    result
}

/// Check if a global alloca is only used by Load and Store instructions
/// (not in GetPtr, Call args, etc.).
fn is_only_loaded_stored(func: &IrFunc, global_idx: usize) -> bool {
    let target = IrOperand::Global(global_idx);
    for block in &func.blocks {
        for inst in &block.instrs {
            match inst {
                IrInst::Load { src, .. } | IrInst::Store { ptr: src, .. }
                    if *src == target => {}
                IrInst::Store { value, .. } if *value == target => {}
                _ => {
                    // Check all operands
                    for op in inst.operands() {
                        if *op == target {
                            // Used in a non-load/store context (e.g., getptr, call arg)
                            return false;
                        }
                    }
                }
            }
        }
    }
    true
}

/// Promote a single alloca: insert phi nodes and rename.
fn promote_alloca(
    func: &mut IrFunc,
    info: &AllocaInfo,
    cfg: &Cfg,
    idom: &[usize],
    df: &[HashSet<usize>],
    dom_children: &[Vec<usize>],
) {
    let n = func.blocks.len();

    // 1. Compute DF+ (iterated dominance frontier) of definition blocks
    let mut phi_blocks: HashSet<usize> = HashSet::new();
    let mut worklist: Vec<usize> = info.def_blocks.iter().copied().collect();
    let mut visited: HashSet<usize> = worklist.iter().copied().collect();

    while let Some(b) = worklist.pop() {
        for &f in &df[b] {
            if phi_blocks.insert(f) {
                if visited.insert(f) {
                    worklist.push(f);
                }
            }
        }
    }

    // 2. Insert phi nodes in phi_blocks
    let mut phis_to_insert: Vec<(usize, Vec<(IrOperand, usize)>)> = Vec::new();
    for &pb in &phi_blocks {
        let pred_labels: Vec<(IrOperand, usize)> = cfg.predecessors[pb]
            .iter()
            .map(|&p| (IrOperand::Undef, func.blocks[p].label))
            .collect();
        phis_to_insert.push((pb, pred_labels));
    }
    for (pb, pred_labels) in phis_to_insert {
        let block = &mut func.blocks[pb];
        block.instrs.insert(
            0,
            IrInst::Phi {
                dest: 0,
                incoming: pred_labels,
            },
        );
    }

    // 3. Rename variables (DFS of dominator tree)
    // We use a simplified renaming: track reaching definitions per alloca
    let mut stacks: HashMap<usize, Vec<IrOperand>> = HashMap::new();
    // Initialize with Undef for each alloca
    stacks.insert(info.alloca_name, vec![IrOperand::Undef]);

    // Block label index → block position map
    let label_to_pos: HashMap<usize, usize> = func
        .blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.label, i))
        .collect();

    // DFS the dominator tree
    let mut new_blocks: Vec<IrBlock> = Vec::new();

    rename_blocks(
        func,
        info,
        cfg,
        idom,
        dom_children,
        &phi_blocks,
        &label_to_pos,
        &mut stacks,
        0, // start at entry block (position 0)
        &mut Vec::new(),
        &mut new_blocks,
    );

    // Remove the alloca instruction
    for block in &mut new_blocks {
        block
            .instrs
            .retain(|inst| !matches!(inst, IrInst::Alloc { dest, .. } if *dest == info.alloca_name));
        // Remove load/store to this alloca
        block.instrs.retain(|inst| match inst {
            IrInst::Load { src, .. } => *src != IrOperand::Global(info.alloca_name),
            IrInst::Store { ptr, .. } => *ptr != IrOperand::Global(info.alloca_name),
            _ => true,
        });
    }

    func.blocks = new_blocks;
}

fn rename_blocks(
    func: &IrFunc,
    info: &AllocaInfo,
    cfg: &Cfg,
    idom: &[usize],
    dom_children: &[Vec<usize>],
    phi_blocks: &HashSet<usize>,
    label_to_pos: &HashMap<usize, usize>,
    stacks: &mut HashMap<usize, Vec<IrOperand>>,
    b: usize,
    visited_block_labels: &mut Vec<usize>,
    new_blocks: &mut Vec<IrBlock>,
) {
    let block = &func.blocks[b];
    let mut new_instrs: Vec<IrInst> = Vec::new();

    // Process instructions
    for inst in &block.instrs {
        match inst {
            IrInst::Phi { .. } if phi_blocks.contains(&b) => {
                // Phi nodes already inserted — update their dest during the rename pass
                // We'll assign a fresh tmp name for the phi dest
                // For now skip — phis were inserted with placeholder dest 0
                new_instrs.push(inst.clone());
            }
            IrInst::Store { value, ptr }
                if *ptr == IrOperand::Global(info.alloca_name) =>
            {
                // Push new definition onto stack
                stacks
                    .entry(info.alloca_name)
                    .or_default()
                    .push(*value);
                // Remove the store (it's now SSA)
            }
            IrInst::Load { dest, src }
                if *src == IrOperand::Global(info.alloca_name) =>
            {
                // Replace load with current reaching definition
                let cur_val = stacks
                    .get(&info.alloca_name)
                    .and_then(|s| s.last())
                    .copied()
                    .unwrap_or(IrOperand::Undef);
                // Add an identity assignment: dest = add cur_val, 0
                new_instrs.push(IrInst::Arith {
                    dest: *dest,
                    op: IrArithOp::Add,
                    lhs: cur_val,
                    rhs: IrOperand::Int(0),
                });
            }
            _ => {
                new_instrs.push(inst.clone());
            }
        }
    }

    // Update phi incoming values for successors
    for &succ in &cfg.successors[b] {
        if phi_blocks.contains(&succ) {
            let succ_block = &func.blocks[succ];
            for (i, inst) in succ_block.instrs.iter().enumerate() {
                if let IrInst::Phi {
                    dest: _,
                    incoming,
                } = inst
                {
                    if incoming.iter().any(|(_, lbl)| *lbl == block.label) {
                        let cur_val = stacks
                            .get(&info.alloca_name)
                            .and_then(|s| s.last())
                            .copied()
                            .unwrap_or(IrOperand::Undef);
                        // We can't modify the original block here, so we'll fix phis
                        // during the actual rewrite. For now just record the value.
                        // (The phi incoming will be updated when we process the successor)
                    }
                }
            }
        }
    }

    new_blocks.push(IrBlock {
        label: block.label,
        instrs: new_instrs,
        preds: block.preds.clone(),
    });

    // Push definitions that were created in this block
    let mut pushed: Vec<usize> = Vec::new();
    for inst in &block.instrs {
        if let IrInst::Store { ptr, .. } = inst {
            if *ptr == IrOperand::Global(info.alloca_name) {
                pushed.push(info.alloca_name);
            }
        }
    }

    // Recurse into dominator tree children
    for &child in &dom_children[b] {
        rename_blocks(
            func, info, cfg, idom, dom_children, phi_blocks,
            label_to_pos, stacks, child, visited_block_labels, new_blocks,
        );
    }

    // Pop definitions pushed in this block
    for _ in 0..pushed.len() {
        stacks.entry(info.alloca_name).or_default().pop();
    }
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
                    IrInst::Store { value: IrOperand::Int(1), ptr: IrOperand::Global(0) },
                    IrInst::Load { dest: 2, src: IrOperand::Global(0) },
                    IrInst::Ret { value: Some(IrOperand::Local(2)) },
                ],
                preds: vec![],
            }],
        }
    }

    #[test]
    fn mem2reg_simple() {
        let mut func = build_simple_func();
        let changed = mem2reg(&mut func);
        assert!(changed);
        let has_alloc = func.blocks[0].instrs.iter().any(|i| matches!(i, IrInst::Alloc { .. }));
        assert!(!has_alloc, "alloc should be removed");
        let has_identity = func.blocks[0].instrs.iter().any(|i| {
            matches!(i, IrInst::Arith { dest: 2, op: IrArithOp::Add, lhs: IrOperand::Int(1), rhs: IrOperand::Int(0) })
        });
        assert!(has_identity, "load should be replaced by identity: {:#?}", func.blocks[0].instrs);
    }
}
