mod ast;
mod codegen;
mod error;
mod koopa_gen;
mod lexer;
mod parser;
mod riscv_gen;

pub use error::{CompilerError, CompilerResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Koopa,
    Riscv,
}

pub fn compile_source(source: &str, mode: OutputMode) -> CompilerResult<String> {
    let tokens = lexer::tokenize(source)?;
    let program = parser::parse(tokens)?;
    codegen::generate(&program, mode)
}

#[cfg(test)]
mod tests {
    use super::{OutputMode, compile_source};

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
}
