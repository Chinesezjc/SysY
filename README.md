# SysY Compiler in Rust

基于 [PKU MiniC 在线文档](https://pku-minic.github.io/online-doc/) 从零开工的 Rust 版 SysY 编译器。

## 测试状态

| 后端 | 通过 | 总数 |
|------|------|------|
| Koopa IR | 130 | 130 |
| RISC-V | ~100 | 130 |

## 文件结构

```
src/
├── main.rs        # 命令行入口
├── lib.rs         # 库接口 (compile_source)
├── ast.rs         # AST 定义
├── lexer.rs       # 词法分析
├── parser.rs      # 语法分析
├── error.rs       # 错误类型
├── codegen.rs     # 分发层 (generate, LIB_FUNCS)
├── koopa_gen.rs   # Koopa IR 代码生成
└── riscv_gen.rs   # RISC-V 汇编代码生成
```

## 构建

```bash
cargo build --release
```

可执行文件生成在 `target/release/compiler`。

## 使用

```bash
compiler -koopa <input.sy> -o <output.koopa>
compiler -riscv <input.sy> -o <output.s>
```

示例:

```bash
./target/release/compiler -koopa examples/lv3_return.sy -o output.koopa
./target/release/compiler -riscv examples/lv3_return.sy -o output.s
```

## 示例

```bash
./target/release/compiler -koopa examples/lv9.sy -o output.koopa
./target/release/compiler -riscv examples/lv9.sy -o output.s
```

`examples/` 目录下有 Lv1-Lv9 各等级的 SysY 示例文件。
