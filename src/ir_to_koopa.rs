//! Emit Koopa IR text from an in-memory [`IrProgram`].
//!
//! Phi nodes are eliminated before emission: each phi is replaced by copies
//! (via `store`) at the end of each predecessor block.

use crate::ir::*;
use std::collections::HashMap;

/// Emit complete Koopa IR text for a program.
pub fn emit_koopa(program: &IrProgram) -> String {
    let mut out = String::new();
    let mut emitted_decls: HashMap<usize, bool> = HashMap::new(); // func name idx → emitted

    // Emit globals
    for g in &program.globals {
        out.push_str(&format_global(program, g));
    }

    // Emit lib-function decls (those without bodies)
    for decl in &program.func_decls {
        out.push_str(&format_func_decl(program, decl));
        emitted_decls.insert(decl.name, true);
    }

    // Emit each function
    for func in &program.funcs {
        // Emit any decls for called library functions not already emitted
        for block in &func.blocks {
            for inst in &block.instrs {
                if let IrInst::Call { func: f, .. } = inst {
                    if !emitted_decls.contains_key(f) && !has_body(program, *f) {
                        // Find or create the decl
                        if let Some(decl) = program.func_decls.iter().find(|d| d.name == *f) {
                            out.push_str(&format_func_decl(program, decl));
                            emitted_decls.insert(*f, true);
                        }
                    }
                }
            }
        }
        out.push_str(&format_function(program, func));
    }

    out
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn has_body(program: &IrProgram, func_idx: usize) -> bool {
    program.funcs.iter().any(|f| f.name == func_idx)
}

fn format_global(program: &IrProgram, g: &IrGlobal) -> String {
    let name = program.global_name(g.name);
    let init_str = match &g.init {
        IrGlobalInit::Zero => "zeroinit".to_string(),
        IrGlobalInit::Values(vals) => {
            if vals.len() == 1 && g.ty == IrType::I32 {
                vals[0].to_string()
            } else {
                format_nested_init(&g.ty, vals, &mut 0)
            }
        }
    };
    format!("global {} = alloc {}, {}\n", name, g.ty, init_str)
}

/// Format nested initializer for multi-dimensional arrays.
fn format_nested_init(ty: &IrType, vals: &[i32], pos: &mut usize) -> String {
    match ty {
        IrType::Array(inner, len) => {
            let mut items = Vec::new();
            for _ in 0..*len {
                items.push(format_nested_init(inner, vals, pos));
            }
            format!("{{{}}}", items.join(", "))
        }
        IrType::I32 => {
            let v = vals.get(*pos).copied().unwrap_or(0);
            *pos += 1;
            v.to_string()
        }
        _ => {
            // Ptr type shouldn't appear in globals
            let v = vals.get(*pos).copied().unwrap_or(0);
            *pos += 1;
            v.to_string()
        }
    }
}

fn format_func_decl(program: &IrProgram, decl: &IrFuncDecl) -> String {
    let name = program.func_name(decl.name);
    let params: Vec<String> = decl
        .param_types
        .iter()
        .map(|t| t.to_string())
        .collect();
    let ret = match &decl.ret_type {
        IrType::Void => String::new(),
        t => format!(": {t}"),
    };
    if params.is_empty() {
        format!("decl @{name}(){ret}\n")
    } else {
        format!("decl @{name}({}){ret}\n", params.join(", "))
    }
}

/// Format one function — phi nodes are lowered to copy-inserts in predecessor blocks.
fn format_function(program: &IrProgram, func: &IrFunc) -> String {
    let name = program.func_name(func.name);

    // Build block index lookup
    let block_idx: HashMap<usize, usize> = func
        .blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.label, i))
        .collect();

    // ── Phi lowering ──
    // For each phi `%d = phi [(v0, bb0), (v1, bb1)]` in block B:
    // append `%d = add v0, 0`  (or `store v0, %d_alloca`) to predecessor bb0,
    // append `%d = add v1, 0` to predecessor bb1, etc.
    // Since our IR has no "move" instruction, we use `add v, 0` as a copy.
    let mut extra_predecessor_instrs: HashMap<usize, Vec<(IrOperand, usize)>> = HashMap::new();
    // (pred_block_idx → list of (phi_dest, phi_dest_value) moves to append)

    for (bi, block) in func.blocks.iter().enumerate() {
        for inst in &block.instrs {
            if let IrInst::Phi { dest, incoming } = inst {
                for (val, pred_label) in incoming {
                    let pred_pos = block_idx[pred_label];
                    extra_predecessor_instrs
                        .entry(pred_pos)
                        .or_default()
                        .push((*val, *dest));
                }
            }
        }
    }

    // Clone blocks and insert phi copies + remove phis
    let mut blocks: Vec<IrBlock> = func.blocks.clone();
    for (pred_idx, copies) in &extra_predecessor_instrs {
        let pred_block = &mut blocks[*pred_idx];
        let terminator = pred_block.instrs.pop(); // save terminator
        for (val, dest) in copies {
            // `dest = add val, 0`  as a copy
            pred_block.instrs.push(IrInst::Arith {
                dest: *dest,
                op: IrArithOp::Add,
                lhs: *val,
                rhs: IrOperand::Int(0),
            });
        }
        if let Some(term) = terminator {
            pred_block.instrs.push(term);
        }
    }

    // Remove phi instructions from all blocks
    for block in &mut blocks {
        block.instrs.retain(|inst| !matches!(inst, IrInst::Phi { .. }));
    }

    // ── Format header ──
    let params_str: Vec<String> = func
        .params
        .iter()
        .map(|(p_name, p_type)| format!("{}: {p_type}", program.global_name(*p_name)))
        .collect();
    let ret_str = match &func.ret_type {
        IrType::Void => String::new(),
        t => format!(": {t}"),
    };
    let header = if params_str.is_empty() {
        format!("fun @{name}(){ret_str} {{\n")
    } else {
        format!("fun @{name}({}){ret_str} {{\n", params_str.join(", "))
    };

    let mut body = header;

    // Emit blocks
    for block in &blocks {
        body.push_str(&format!("{}:", program.block_name(block.label)));
        body.push('\n');
        for inst in &block.instrs {
            body.push_str(&format_inst(program, inst));
        }
    }

    body.push_str("}\n");
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_program() {
        let mut prog = IrProgram::new();
        let main_func = prog.intern_func("main".to_string());
        let getint_func = prog.intern_func("getint".to_string());

        // decl @getint(): i32
        prog.func_decls.push(IrFuncDecl {
            name: getint_func,
            param_types: vec![],
            ret_type: IrType::I32,
        });

        // fun @main(): i32 { %entry: ... }
        let entry = prog.intern_block("%entry".to_string());
        let t0 = prog.intern_local("%0".to_string());
        let t1 = prog.intern_local("%1".to_string());
        let t2 = prog.intern_local("%2".to_string());

        prog.funcs.push(IrFunc {
            name: main_func,
            params: vec![],
            ret_type: IrType::I32,
            allocas: vec![],
            blocks: vec![IrBlock {
                label: entry,
                instrs: vec![
                    IrInst::Call {
                        dest: Some(t0),
                        func: getint_func,
                        args: vec![],
                    },
                    IrInst::Call {
                        dest: Some(t1),
                        func: getint_func,
                        args: vec![],
                    },
                    IrInst::Arith {
                        dest: t2,
                        op: IrArithOp::Add,
                        lhs: IrOperand::Local(t0),
                        rhs: IrOperand::Local(t1),
                    },
                    IrInst::Ret {
                        value: Some(IrOperand::Local(t2)),
                    },
                ],
                preds: vec![],
            }],
        });

        let out = emit_koopa(&prog);
        let expected = "decl @getint(): i32\nfun @main(): i32 {\n%entry:\n  %0 = call @getint()\n  %1 = call @getint()\n  %2 = add %0, %1\n  ret %2\n}\n";
        assert_eq!(out, expected);
    }
}

/// Format a single IR instruction as Koopa IR text (one indented line).
fn format_inst(program: &IrProgram, inst: &IrInst) -> String {
    let locals = &program.local_names;
    let globals = &program.global_names;
    let blocks = &program.block_names;
    let funcs = &program.func_names;

    let op_str = |op: IrOperand| op.display(locals, globals);

    match inst {
        IrInst::Alloc { dest, ty } => {
            format!("  {} = alloc {ty}\n", globals[*dest])
        }
        IrInst::Load { dest, src } => {
            format!("  {} = load {}\n", locals[*dest], op_str(*src))
        }
        IrInst::Store { value, ptr } => {
            format!("  store {}, {}\n", op_str(*value), op_str(*ptr))
        }
        IrInst::Arith { dest, op, lhs, rhs } => {
            format!(
                "  {} = {} {}, {}\n",
                locals[*dest],
                op,
                op_str(*lhs),
                op_str(*rhs)
            )
        }
        IrInst::Icmp { dest, op, lhs, rhs } => {
            format!(
                "  {} = {} {}, {}\n",
                locals[*dest],
                op,
                op_str(*lhs),
                op_str(*rhs)
            )
        }
        IrInst::GetPtr { dest, ptr, index } => {
            format!(
                "  {} = getptr {}, {}\n",
                locals[*dest],
                op_str(*ptr),
                op_str(*index)
            )
        }
        IrInst::GetElemPtr { dest, ptr, index } => {
            format!(
                "  {} = getelemptr {}, {}\n",
                locals[*dest],
                op_str(*ptr),
                op_str(*index)
            )
        }
        IrInst::Call {
            dest, func, args, ..
        } => {
            let args_str: Vec<String> = args.iter().map(|a| op_str(*a)).collect();
            if let Some(d) = dest {
                format!(
                    "  {} = call @{}({})\n",
                    locals[*d],
                    funcs[*func],
                    args_str.join(", ")
                )
            } else {
                format!("  call @{}({})\n", funcs[*func], args_str.join(", "))
            }
        }
        IrInst::Br {
            cond,
            then_bb,
            else_bb,
        } => {
            format!(
                "  br {}, {}, {}\n",
                op_str(*cond),
                blocks[*then_bb],
                blocks[*else_bb]
            )
        }
        IrInst::Jump { target } => {
            format!("  jump {}\n", blocks[*target])
        }
        IrInst::Ret { value } => {
            if let Some(v) = value {
                format!("  ret {}\n", op_str(*v))
            } else {
                "  ret\n".to_string()
            }
        }
        IrInst::Phi { .. } => {
            // Phis should have been lowered; if any remain, emit a comment.
            "  // phi (unexpected)\n".to_string()
        }
        IrInst::Asm(s) => {
            format!("  // asm(\"{s}\")  ; not supported in Koopa IR\n")
        }
    }
}
