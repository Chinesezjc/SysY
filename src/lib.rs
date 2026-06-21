mod ast;
mod ast_to_ir;
mod cfg;
mod codegen;
mod error;
mod ir;
mod ir_builder;
mod ir_to_koopa;
mod koopa_gen;
mod lexer;
mod opt;
mod parser;
mod riscv_gen;
mod ssa;

pub use error::{CompilerError, CompilerResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Koopa,
    Riscv,
    KoopaIr,
}

pub fn compile_source(source: &str, mode: OutputMode) -> CompilerResult<String> {
    let tokens = lexer::tokenize(source)?;
    let program = parser::parse(tokens)?;
    codegen::generate(&program, mode)
}

#[cfg(test)]
mod tests {
    use super::{OutputMode, compile_source};
    use std::fs;

    #[test]
    fn compiles_to_koopa() {
        let source = "int main() { return 1 + 2 * -3; }";
        let output = compile_source(source, OutputMode::Koopa).unwrap();
        assert_eq!(
            output,
            "fun @main(): i32 {\n%entry:\n  %0 = sub 0, 3\n  %1 = mul 2, %0\n  %2 = add 1, %1\n  ret %2\n}\n"
        );
    }

    #[test]
    fn compiles_to_riscv() {
        let source = "int main() { return 0x10 + 07; }";
        let output = compile_source(source, OutputMode::Riscv).unwrap();
        assert_eq!(
            output,
            "  .text\n  .globl main\nmain:\n  addi sp, sp, -16\n  sw ra, 12(sp)\n  li a0, 16\n  addi sp, sp, -4\n  sw a0, 0(sp)\n  li a0, 7\n  lw t0, 0(sp)\n  addi sp, sp, 4\n  add a0, t0, a0\n  lw ra, 12(sp)\n  addi sp, sp, 16\n  ret\n"
        );
    }

    #[test]
    fn ir_pipeline_matches_old() {
        // Test that new IR pipeline produces equivalent output for all lvX tests
        let test_dir = "sysy-testsuit-collection/lvX";
        let mut files: Vec<_> = fs::read_dir(test_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "c"))
            .map(|e| e.path())
            .collect();
        files.sort();

        let mut passed = 0;
        let mut failures: Vec<String> = Vec::new();

        for path in files.iter().take(50) {
            let source = fs::read_to_string(path).unwrap();
            let old_out = compile_source(&source, OutputMode::Koopa).unwrap_or_else(|e| format!("ERROR: {e}"));
            let new_out = compile_source(&source, OutputMode::KoopaIr).unwrap_or_else(|e| format!("ERROR: {e}"));

            if old_out != new_out {
                let name = path.file_stem().unwrap().to_string_lossy();
                failures.push(format!("{name}: mismatch\n  OLD={old_out:?}\n  NEW={new_out:?}"));
                if failures.len() <= 3 {
                    eprintln!("--- {name} ---\nOLD:\n{old_out}\nNEW:\n{new_out}\n---");
                }
            } else {
                passed += 1;
            }
        }

        if !failures.is_empty() {
            for f in &failures[..10.min(failures.len())] {
                eprintln!("{f}");
            }
            panic!("{}/{} tests failed", failures.len(), passed + failures.len());
        }
        eprintln!("All {passed} tests passed!");
    }
}
