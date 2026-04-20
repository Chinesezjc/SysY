# SysY Compiler in Rust

基于 PKU MiniC 在线文档从零开工的 Rust 版 SysY 编译器。

当前版本已经完成一个适合课程前几级实验继续迭代的骨架:

- 支持课程要求的命令行接口
- 已实现词法分析, 语法分析, AST 和常量表达式求值
- 当前可处理的 SysY 子集:
  - 单个无参 `int` 函数
  - `return Exp;`
  - `INT_CONST`
  - 一元运算 `+ - !`
  - 二元运算 `* / % + - < > <= >= == != && ||`
  - 括号表达式
  - 单行/多行注释
- `-koopa` 输出文本形式 Koopa IR
- `-riscv` 输出最小 RISC-V 汇编

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

## 当前限制

- 还不支持变量、常量声明、赋值、作用域、`if`、`while`、函数调用和数组
- 表达式中的标识符还没有接入符号表

这份骨架已经把模块边界拆好了，后面可以按课程 Lv4 之后的内容继续往里填。
