# SysY Compiler in Rust

基于 PKU MiniC 在线文档从零开工的 Rust 版 SysY 编译器。

当前版本已完成 PKU MiniC 课程全部等级 (Lv1-Lv9):

- 支持课程要求的命令行接口
- 词法分析、语法分析、AST、语义分析、代码生成
- 可处理的 SysY 子集:
  - 多函数定义，支持 `int`/`void` 返回类型和参数
  - 库函数声明 (`getint`, `putint`, `getch`, `putch`, `getarray`, `putarray`, `starttime`, `stoptime`)
  - 常量声明 (`const`) 和变量声明 (`int`)
  - 赋值语句，支持数组元素赋值
  - `if`/`else` 分支控制流
  - `while`/`break`/`continue` 循环
  - 嵌套作用域和变量遮蔽
  - `&&`/`||` 短路求值
  - 函数调用和参数传递
  - 多维数组声明和索引访问
  - 所有一元/二元运算 (`+ - ! * / % + - < > <= >= == != && ||`)
  - 单行/多行注释
- `-koopa` 输出文本形式 Koopa IR
- `-riscv` 输出 RISC-V 汇编 (RV32I + 调用约定)

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
