use crate::ast_to_ir;
use crate::ir_to_koopa;
use crate::ir_to_riscv;
use crate::koopa_gen;
use crate::opt::{self, IrFuncPass, IrProgramPass};
use crate::riscv_gen;
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

pub fn generate(program: &CompUnit, mode: OutputMode) -> CompilerResult<String> {
    match mode {
        OutputMode::Koopa => koopa_gen::KoopaGen::new().gen_program(program),
        OutputMode::Riscv => riscv_gen::RiscvGen::new().gen_program(program),
        OutputMode::KoopaIr => {
            let mut ir = ast_to_ir::AstToIr::new().gen_program(program)?;
            opt::inline::Inline::new().run(&mut ir);
            for func in &mut ir.funcs { ssa::mem2reg(func); }
            let mut pm = opt::PassManager::new();
            pm.add_func_pass(Box::new(opt::const_fold::ConstFold));
            pm.add_func_pass(Box::new(opt::dce::DeadCodeElim));
            pm.add_func_pass(Box::new(opt::gvn::GVN));
            pm.run(&mut ir);
            Ok(ir_to_koopa::emit_koopa(&ir))
        }
        OutputMode::RiscvIr => {
            let mut ir = ast_to_ir::AstToIr::new().gen_program(program)?;
            opt::inline::Inline::new().run(&mut ir);
            for func in &mut ir.funcs { ssa::mem2reg(func); }
            let mut pm = opt::PassManager::new();
            pm.add_func_pass(Box::new(opt::const_fold::ConstFold));
            pm.add_func_pass(Box::new(opt::dce::DeadCodeElim));
            pm.add_func_pass(Box::new(opt::gvn::GVN));
            pm.run(&mut ir);
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
