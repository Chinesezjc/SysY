//! Function inlining pass.
//!
//! Inlines small, non-recursive user-defined functions at call sites.
//! After inlining, Mem2Reg should be re-run to clean up introduced allocas.

use crate::ir::*;
use crate::opt::IrProgramPass;
use std::collections::{HashMap, HashSet};

pub struct Inline {
    /// Maximum number of instructions in the callee to consider inlining.
    max_instrs: usize,
}

impl Inline {
    pub fn new() -> Self { Inline { max_instrs: 50 } }
    pub fn with_limit(max_instrs: usize) -> Self { Inline { max_instrs } }

    /// Count instructions in a function.
    fn instr_count(func: &IrFunc) -> usize {
        func.blocks.iter().map(|b| b.instrs.len()).sum()
    }
}

impl IrProgramPass for Inline {
    fn name(&self) -> &str { "inline" }

    fn run(&self, program: &mut IrProgram) -> bool {
        let mut changed = false;

        // Build call graph: func_name_idx → set of callee name indices it calls
        let call_graph = build_call_graph(program);

        // Find inlinable functions: non-recursive, small, user-defined
        let inlinable: HashSet<usize> = find_inlinable(program, &call_graph, self.max_instrs);

        if inlinable.is_empty() { return false; }

        // For each function that calls an inlinable function, perform inlining
        let mut new_funcs: Vec<IrFunc> = Vec::new();
        let mut funcs_removed: HashSet<usize> = HashSet::new();

        let mut func_idx = 0usize;
        while func_idx < program.funcs.len() {
            let caller = &program.funcs[func_idx];
            let mut new_caller = caller.clone();
            let mut caller_changed = false;

            // Find call sites in the caller
            let mut new_blocks: Vec<IrBlock> = Vec::new();
            for block in &caller.blocks {
                let mut block_instrs: Vec<IrInst> = Vec::new();
                for inst in &block.instrs {
                    if let IrInst::Call { dest, func, args } = inst {
                        if inlinable.contains(func) {
                            // Get the callee
                            if let Some(callee) = program.find_func(*func) {
                                if !funcs_removed.contains(func) {
                                    // Inline this call
                                    let callee_clone = callee.clone();
                                    inline_call(
                                        &callee_clone, &mut block_instrs,
                                        *dest, args, program,
                                    );
                                    caller_changed = true;
                                    changed = true;
                                    continue; // skip pushing the original call
                                }
                            }
                        }
                    }
                    block_instrs.push(inst.clone());
                }
                new_blocks.push(IrBlock {
                    label: block.label,
                    instrs: block_instrs,
                    preds: block.preds.clone(),
                });
            }

            if caller_changed {
                new_caller.blocks = new_blocks;
            }
            new_funcs.push(new_caller);
            func_idx += 1;
        }

        if changed {
            program.funcs = new_funcs;
            // Remove inlined functions that are no longer called
            // (For simplicity, keep them — DCE will handle dead functions later)
        }

        changed
    }
}

/// Build call graph: func_idx → set of callee indices it calls.
fn build_call_graph(program: &IrProgram) -> HashMap<usize, HashSet<usize>> {
    let mut graph: HashMap<usize, HashSet<usize>> = HashMap::new();
    for func in &program.funcs {
        let mut callees = HashSet::new();
        for block in &func.blocks {
            for inst in &block.instrs {
                if let IrInst::Call { func, .. } = inst {
                    callees.insert(*func);
                }
            }
        }
        graph.insert(func.name, callees);
    }
    graph
}

/// Find functions suitable for inlining: small, non-recursive, user-defined
/// (not library functions).
fn find_inlinable(
    program: &IrProgram,
    call_graph: &HashMap<usize, HashSet<usize>>,
    max_instrs: usize,
) -> HashSet<usize> {
    let mut inlinable = HashSet::new();

    // Collect library function names
    let lib_names: HashSet<usize> = program.func_decls.iter().map(|d| d.name).collect();

    for func in &program.funcs {
        // Skip library functions
        if lib_names.contains(&func.name) { continue; }
        // Skip large functions
        if Inline::instr_count(func) > max_instrs { continue; }
        // Skip recursive functions (check call graph)
        if let Some(callees) = call_graph.get(&func.name) {
            if callees.contains(&func.name) { continue; }
        }
        inlinable.insert(func.name);
    }

    inlinable
}

/// Inline a function call: replace `dest = call @func(args)` with the callee's body.
/// `callee` is the function being inlined.
/// `out_instrs` is the caller's current block instruction list where inlined code is appended.
fn inline_call(
    callee: &IrFunc,
    out_instrs: &mut Vec<IrInst>,
    dest: Option<usize>,
    args: &[IrOperand],
    program: &IrProgram,
) {
    // For single-block callees (simplest case):
    // Replace each param with its argument value.
    // Replace `ret val` with `dest = add val, 0`.
    if callee.blocks.len() == 1 {
        // Build param → arg mapping
        let param_map: HashMap<usize, IrOperand> = callee
            .params
            .iter()
            .enumerate()
            .map(|(i, (p_name, _))| (*p_name, args.get(i).copied().unwrap_or(IrOperand::Int(0))))
            .collect();

        for inst in &callee.blocks[0].instrs {
            match inst {
                IrInst::Ret { value } => {
                    if let Some(d) = dest {
                        let ret_val = value.unwrap_or(IrOperand::Int(0));
                        out_instrs.push(IrInst::Arith {
                            dest: d,
                            op: IrArithOp::Add,
                            lhs: ret_val,
                            rhs: IrOperand::Int(0),
                        });
                    }
                }
                _ => {
                    let mut cloned = inst.clone();
                    // Replace param references with args
                    replace_param_refs(&mut cloned, &param_map);
                    out_instrs.push(cloned);
                }
            }
        }
        return;
    }

    // Multi-block callee: more complex — skip for now
    // Just keep the original call
    out_instrs.push(IrInst::Call { dest, func: callee.name, args: args.to_vec() });
}

/// Replace references to parameter locals with argument values.
fn replace_param_refs(inst: &mut IrInst, param_map: &HashMap<usize, IrOperand>) {
    let mut rep = |op: &mut IrOperand| {
        if let IrOperand::Local(idx) = *op {
            if let Some(&new_val) = param_map.get(&idx) {
                *op = new_val;
            }
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
