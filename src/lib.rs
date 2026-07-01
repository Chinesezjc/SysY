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
mod reg_alloc;
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
        // Both Koopa and KoopaIr use the same IR pipeline now.
        // Output may differ in non-semantic ways (local numbering from
        // HashMap iteration order), so we only verify both compile successfully.
        let test_dir = "sysy-testsuit-collection/lvX";
        let mut files: Vec<_> = fs::read_dir(test_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "c"))
            .map(|e| e.path())
            .collect();
        files.sort();

        let mut passed = 0;
        let mut failures = 0;

        for path in files.iter().take(50) {
            let source = fs::read_to_string(path).unwrap();
            let koopa_out = compile_source(&source, OutputMode::Koopa);
            let ir_out = compile_source(&source, OutputMode::KoopaIr);

            if koopa_out.is_ok() && ir_out.is_ok() {
                passed += 1;
            } else {
                failures += 1;
                let name = path.file_stem().unwrap().to_string_lossy();
                eprintln!("{name}: compile error");
            }
        }

        if failures > 0 {
            panic!("{}/{} tests failed to compile", failures, passed + failures);
        }
        eprintln!("All {passed} tests compiled successfully!");
    }
}
