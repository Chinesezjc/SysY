# SysY Compiler in Rust

基于 [PKU MiniC 在线文档](https://pku-minic.github.io/online-doc/) 从零开工的 Rust 版 SysY 编译器。

## 参考文档

- [SysY 语言规范](https://pku-minic.github.io/online-doc/#/misc-app-ref/sysy-spec)
- [Koopa IR 规范](https://pku-minic.github.io/online-doc/#/misc-app-ref/koopa)
- [RISC-V 指令/调用约定](https://pku-minic.github.io/online-doc/#/misc-app-ref/riscv-insts)
- [SysY 运行时库](https://pku-minic.github.io/online-doc/#/misc-app-ref/sysy-runtime)

## 测试状态

| 后端 | 通过 | 总数 |
|------|------|------|
| Koopa IR | 130 | 130 |
| RISC-V | 130 | 130 |

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

## 关键实现要点

### Koopa IR 规范

严格遵循 [Koopa IR 规范](https://pku-minic.github.io/online-doc/#/misc-app-ref/koopa):

| 指令 | 操作数类型 | 返回类型 | 说明 |
|------|-----------|---------|------|
| `getptr` | 指针 `*t` | `*t` | 偏移 `sizeof(t) * idx` |
| `getelemptr` | 数组指针 `*[t, len]` | `*t` | 偏移 `sizeof(t) * idx` + 数组退化 |

NdParam 索引（如 `int arr[][10]`）：
```koopa
%0 = getptr @arr, i       // 第一维（动态）: getptr
%1 = getelemptr %0, j     // 后续维（固定）: getelemptr
```

### RISC-V 调用约定

参数 `a0-a7`，返回值 `a0`。栈由被调用方维护，对齐 16 字节。

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
