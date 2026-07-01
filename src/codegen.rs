use crate::ast_to_ir;
use crate::ir::IrProgram;
use crate::ir::IrOperand;
use crate::ir_to_koopa;
use crate::ir_to_riscv;
use crate::opt::{self, IrFuncPass, IrProgramPass};
use crate::ssa;

use crate::OutputMode;
use crate::ast::{CompUnit, Type};
use crate::error::CompilerResult;

pub(crate) const LIB_FUNCS: &[(&str, Type, &[&str])] = &[
    ("getint", Type::Int, &[]),
    ("getch", Type::Int, &[]),
    ("getarray", Type::Int, &["*i32"]),
    ("putint", Type::Void, &["i32"]),
    ("putch", Type::Void, &["i32"]),
    ("putarray", Type::Void, &["i32", "*i32"]),
    ("starttime", Type::Void, &[]),
    ("stoptime", Type::Void, &[]),
];

/// AST → optimized IR (shared by all new-pipeline backends).
fn compile_to_ir(program: &CompUnit) -> CompilerResult<IrProgram> {
    let mut ir = ast_to_ir::AstToIr::new().gen_program(program)?;
    opt::inline::Inline::new().run(&mut ir);
    for func in &mut ir.funcs { ssa::mem2reg(func); }
    let mut pm = opt::PassManager::new();
    pm.add_func_pass(Box::new(opt::const_fold::ConstFold));
    pm.add_func_pass(Box::new(opt::dce::DeadCodeElim));
    pm.add_func_pass(Box::new(opt::gvn::GVN));
    pm.run(&mut ir);
    // Find max local index and extend name table for phi/rename locals
    let mut max_local = 0usize;
    for func in &ir.funcs {
        for block in &func.blocks {
            for inst in &block.instrs {
                if let Some(d) = inst.dest() { max_local = max_local.max(d); }
                for op in inst.operands() {
                    if let IrOperand::Local(l) = *op { max_local = max_local.max(l); }
                }
            }
        }
    }
    while ir.local_names.len() <= max_local {
        ir.local_names.push(format!("%{}", ir.local_names.len()));
    }
    // Lower phi nodes before RISC-V emission
    for func in &mut ir.funcs {
        ssa::lower_phis(func);
    }
    // Extend frame for new phi-generated locals (they need stack slots)
    // Find max local again after phi lowering (copies add more locals)
    let mut max_after = max_local;
    for func in &ir.funcs {
        for block in &func.blocks {
            for inst in &block.instrs {
                if let Some(d) = inst.dest() { max_after = max_after.max(d); }
                for op in inst.operands() {
                    if let IrOperand::Local(l) = *op { max_after = max_after.max(l); }
                }
            }
        }
    }
    while ir.local_names.len() <= max_after {
        ir.local_names.push(format!("%{}", ir.local_names.len()));
    }
    Ok(ir)
}

pub fn generate(program: &CompUnit, mode: OutputMode) -> CompilerResult<String> {
    match mode {
        OutputMode::Koopa | OutputMode::KoopaIr => {
            let ir = compile_to_ir(program)?;
            Ok(ir_to_koopa::emit_koopa(&ir))
        }
        OutputMode::Riscv | OutputMode::RiscvIr => {
            let ir = compile_to_ir(program)?;
            Ok(ir_to_riscv::emit_riscv(&ir))
        }
    }
}

pub(crate) fn is_lib_func(name: &str) -> bool {
    LIB_FUNCS.iter().any(|(n, _, _)| *n == name)
}

pub(crate) fn lib_func_ret_type(name: &str) -> Option<Type> {
    LIB_FUNCS.iter().find(|(n, _, _)| *n == name).map(|(_, t, _)| *t)
}
