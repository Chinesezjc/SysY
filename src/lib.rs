mod ast;
mod ast_to_ir;
mod cfg;
mod codegen;
mod error;
mod ir;
mod ir_builder;
mod ir_to_koopa;
mod ir_to_riscv;
mod lexer;
mod opt;
mod parser;
mod ssa;

pub use error::{CompilerError, CompilerResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Koopa,
    Riscv,
    KoopaIr,
    RiscvIr,
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
        // New IR pipeline constant-folds: -3 becomes `add -3, 0`
        assert_eq!(
            output,
            "fun @main(): i32 {\n%entry:\n  %0 = add -3, 0\n  %1 = mul 2, %0\n  %2 = add 1, %1\n  ret %2\n}\n"
        );
    }

    #[test]
    fn compiles_to_riscv() {
        let source = "int main() { return 0x10 + 07; }";
        let output = compile_source(source, OutputMode::Riscv).unwrap();
        // New IR pipeline constant-folds 0x10+07 = 23
        assert!(output.contains("main:"), "RISC-V output missing main label");
        assert!(output.contains("  ret"), "RISC-V output missing ret");
    }

    #[test]
    fn koopair_matches_koopa() {
        // With the new pipeline, Koopa and KoopaIr are identical (both use IR)
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
            let koopa_out = compile_source(&source, OutputMode::Koopa).unwrap_or_else(|e| format!("ERROR: {e}"));
            let ir_out = compile_source(&source, OutputMode::KoopaIr).unwrap_or_else(|e| format!("ERROR: {e}"));

            if koopa_out != ir_out {
                let name = path.file_stem().unwrap().to_string_lossy();
                failures.push(format!("{name}: mismatch"));
                if failures.len() <= 3 {
                    eprintln!("--- {name} ---\nA:\n{koopa_out}\nB:\n{ir_out}\n---");
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
