//! Optimization pass infrastructure.

pub mod const_fold;
pub mod dce;
pub mod gvn;

use crate::ir::*;

/// Trait for an optimization pass that operates on a single function.
pub trait IrFuncPass {
    fn name(&self) -> &str;
    /// Returns `true` if the function IR was modified.
    fn run(&self, func: &mut IrFunc) -> bool;
}

/// Trait for an optimization pass that operates on the whole program.
pub trait IrProgramPass {
    fn name(&self) -> &str;
    /// Returns `true` if the program IR was modified.
    fn run(&self, program: &mut IrProgram) -> bool;
}

/// Manages a sequence of optimization passes and runs them to a fixed point.
pub struct PassManager {
    func_passes: Vec<Box<dyn IrFuncPass>>,
    program_passes: Vec<Box<dyn IrProgramPass>>,
}

impl PassManager {
    pub fn new() -> Self {
        PassManager {
            func_passes: Vec::new(),
            program_passes: Vec::new(),
        }
    }

    pub fn add_func_pass(&mut self, pass: Box<dyn IrFuncPass>) {
        self.func_passes.push(pass);
    }

    pub fn add_program_pass(&mut self, pass: Box<dyn IrProgramPass>) {
        self.program_passes.push(pass);
    }

    /// Run all passes to a fixed point (iterate until no changes).
    /// Program-level passes run first (e.g., inlining), then function-level.
    pub fn run(&mut self, program: &mut IrProgram) {
        let mut changed = true;
        let mut iteration = 0;
        let max_iterations = 10; // safety limit

        while changed && iteration < max_iterations {
            changed = false;
            iteration += 1;

            // Program-level passes
            for pass in &self.program_passes {
                if pass.run(program) {
                    changed = true;
                }
            }

            // Function-level passes
            for func in &mut program.funcs {
                for pass in &self.func_passes {
                    if pass.run(func) {
                        changed = true;
                    }
                }
            }
        }
    }
}

impl Default for PassManager {
    fn default() -> Self {
        Self::new()
    }
}
