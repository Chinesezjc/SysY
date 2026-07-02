# 课程项目报告：SysY → RISC-V 编译器

---

## 一、项目背景

### 1.1 项目目标

本项目实现了一个将 **SysY 语言**（C 语言的精简子集）编译到 **RISC-V 32-bit 汇编** 的编译器。SysY 支持 `int`、`void`、多维数组、函数定义与调用、`if`/`else`、`while` 循环、`break`/`continue`、短路求值 `&&`/`||`、全局变量、作用域嵌套等核心语言特性。

### 1.2 扩展特性

在 SysY 标准语法之上，本项目实现了以下扩展：

**内嵌汇编 (Inline Assembly)**。SysY 标准语法不具备底层硬件控制能力，我们扩展了 `asm` 关键字，支持在 SysY 源码中直接嵌入 RISC-V 汇编指令。该特性覆盖语句形式 `asm("指令");` 和表达式形式 `x = asm("指令");`，汇编字符串透传至最终输出，与 SSA IR 优化管线无缝集成。这为性能关键路径的手工优化和硬件 CSR 寄存器访问提供了接口。

### 1.3 侧重方向

本项目的技术重心在**中后端**：

- **中端**: 自建 SSA 形式的 IR、多基本块 Mem2Reg（含支配树、phi 插入与重命名）、常量折叠、全局值编号、死代码消除、函数内联等优化
- **后端**: 线性扫描寄存器分配（含活跃区间分析、帧省略）、尾调用优化、内嵌汇编代码生成

---

## 二、项目设计

### 2.1 输入输出

| | 格式 | 说明 |
|------|------|------|
| **输入** | SysY 源码 (`.sy` / `.c`) | C 语言的精简子集 |
| **输出** | RISC-V 32-bit 汇编 (`.S`) | rv32im + ilp32 ABI |
| **备选输出** | Koopa IR (`.koopa`) | 参考编译器中间表示，用于兼容性验证 |

### 2.2 整体流水线

```
SysY 源码 (.sy)
  │
  ├── [1] Lexer (lexer.rs)
  │   字符流 → Token 流 (关键字/标识符/字面量/运算符/界符)
  │
  ├── [2] Parser (parser.rs)
  │   Token 流 → AST (递归下降，含 asm 语法扩展)
  │
  ├── [3] AST → IR (ast_to_ir.rs)
  │   AST → 自定义 SSA IR (含 Alloc/Load/Store, 控制流基本块)
  │
  ├── [4] IR 优化管线 (opt/ + ssa.rs)
  │   ├── ConstFold    常量折叠
  │   ├── GVN          全局值编号 (消除冗余计算)
  │   ├── DCE          死代码消除
  │   ├── Inline       函数内联 (小函数)
  │   ├── Mem2Reg      Alloca → SSA 提升
  │   │   ├── 支配树 + 支配边界计算
  │   │   ├── Phi 节点插入 (在 join 点)
  │   │   ├── 变量重命名 (DFS 写栈)
  │   │   └── Phi lowering (降级到并行复制)
  │   └── CFG 分析     控制流图构建
  │
  ├── [5] 代码生成 (ir_to_riscv.rs)
  │   ├── 栈帧布局 (compute_frame)
  │   ├── 线性扫描寄存器分配 (reg_alloc.rs)
  │   │   ├── 活跃区间分析 (last-use / first-def)
  │   │   ├── advance() 死寄存器自动释放
  │   │   └── 寄存器池 t0-t2, 参数寄存器 a0-a7
  │   ├── 尾调用优化 (自递归 call→j)
  │   └── 内嵌汇编透传
  │
  └── [6] RISC-V 汇编 (.S)
       → clang -c → .o → ld.lld → ELF → qemu-riscv32
```

### 2.3 自定义 SSA IR 设计

IR 采用**基本块 + SSA 变量**的组织形式，每条指令单一定值：

```rust
pub enum IrInst {
    Alloc  { dest, ty },                  // 栈分配
    Load   { dest, src },                 // 加载
    Store  { value, ptr },                // 存储
    Arith  { dest, op, lhs, rhs },        // 算术运算
    Icmp   { dest, op, lhs, rhs },        // 比较
    GetPtr { dest, ptr, index, sz },      // 数组元素地址
    GetElemPtr { dest, ptr, index, sz },  // 嵌套数组地址
    Call   { dest, func, args },          // 函数调用
    Br     { cond, then_bb, else_bb },    // 条件分支
    Jump   { target },                    // 无条件跳转
    Ret    { value },                     // 返回
    Phi    { dest, incoming },            // Phi 节点 (Mem2Reg)
    Asm    { dest, code },                // 内嵌汇编
}
```

IR 程序由 `Vec<IrFunc>` 组成，每个函数包含 `Vec<IrBlock>`，每个基本块以终止指令（`Br`/`Jump`/`Ret`）结束。

### 2.4 关键 Pass 详解

**Mem2Reg (ssa.rs, ~430 行)**：将栈分配的局部变量提升为 SSA 寄存器变量。首先对每个函数构建支配树（迭代数据流算法），计算支配边界。然后在每个 join 点的支配边界插入 phi 指令。最后 DFS 遍历支配树进行变量重命名，维护写栈处理多重赋值。排除包含数组访问的函数以保证正确性。

**寄存器分配 (reg_alloc.rs, ~240 行)**：采用线性扫描策略。对每个基本块预计算各局部变量的最后使用位置 (`last_use`) 和首次定义位置 (`first_def`)。代码生成时，`advance(pos)` 在每条指令前检查并释放已死亡寄存器。寄存器池 `t0, t1, t2` 用于临时值；当池满时选择最远使用位置的变量溢出。`a0-a7` 保留用于参数传递。单基本块叶子函数且寄存器压力 ≤3 时完全省略栈帧。

**尾调用优化 (ir_to_riscv.rs)**：在代码生成阶段检测基本块末尾的 `Call(dest) + Ret(dest)` 模式，若被调用方是自身（`callee == self`），则将 `call` 替换为：覆写 `a0-a7` 参数寄存器 → `j` 跳转到函数入口。该优化将自递归函数的栈深度从 O(n) 降为 O(1)，消除快速选择/快速排序类算法在大规模逆序输入上的栈溢出。

**内嵌汇编 (parser.rs + ir_to_riscv.rs)**：`asm` 作为关键字在词法/语法层面解析，AST 节点为 `Stmt::Asm(String)` 和 `Expr::Asm(String)`。IR 层面统一为 `IrInst::Asm { dest, code }`，参与标准优化管线（死代码消除等）。代码生成时汇编字符串直接透传到输出；表达式形式额外将 `a0` 写入目标栈槽。

---

## 三、实现情况

### 3.1 工作量

| 指标 | 数值 |
|------|------|
| Rust 源码行数 | ~5,300 行 |
| 源文件数 | 15 个（含 4 个优化 pass） |
| Git 提交数 | 88 commits |
| 功能测试用例 | 110 个 (lv1–lv9) |
| 性能测试用例 | 30 个 |
| 单元测试 | 14 个 |

### 3.2 代码结构设计

```
src/
├── main.rs            # 入口：CLI 参数解析，driver
├── lib.rs             # 集成测试
├── error.rs           # 统一错误类型
├── lexer.rs           # 词法分析
├── parser.rs          # 递归下降语法分析
├── ast.rs             # AST 节点定义
├── ast_to_ir.rs       # AST → IR 转换（最大文件 ~1150 行）
├── ir.rs              # IR 数据结构定义
├── ir_builder.rs      # IR 构造辅助（SSA 分配、块管理）
├── cfg.rs             # 控制流图
├── ssa.rs             # Mem2Reg (支配树 + phi)
├── reg_alloc.rs       # 寄存器分配器
├── ir_to_riscv.rs     # RISC-V 代码生成
├── ir_to_koopa.rs     # Koopa IR 输出（兼容参考编译器）
├── codegen.rs         # 优化管线编排
└── opt/
    ├── mod.rs         # Pass trait 定义
    ├── const_fold.rs  # 常量折叠
    ├── gvn.rs         # 全局值编号
    ├── dce.rs         # 死代码消除
    └── inline.rs      # 函数内联
```

设计原则：
- **单一职责**：前/中/后端严格分离，每个文件职责明确
- **IR 为中心**：所有优化以自定义 IR 为中间层，前后端解耦
- **Pass 管线化**：优化 pass 实现统一 trait，按序编排
- **安全 Rust**：未使用 unsafe，依赖仅标准库

### 3.3 开发经验

1. **SSA 构造的工程挑战**：Mem2Reg 的 phi 插入逻辑对多定义情况（同一变量在多个前驱块中被赋值）需要仔细处理。实际实现中对数组访问函数采用保守策略（排除提升），避免指针别名分析的复杂性。

2. **寄存器分配的权衡**：3 寄存器池是 rv32 的工程平衡——寄存器太多会导致帧省略阈值过高（栈帧始终存在），太少则溢出频繁。3 个临时寄存器配合 a0-a7 参数寄存器，在 110 个测试中达到零溢出错误。

3. **尾调用优化的陷阱**：初始实现在 IR 层面匹配模式失败，原因是 IR 中函数名索引与实际函数名索引因 `@` 前缀而不一致。改为在代码生成阶段用字符串比较解决。

4. **调用约定的细节**：RISC-V caller-saved 寄存器在调用后全部失效。初版仅写回 dirty 寄存器，导致 clean 寄存器在调用后静默持有过期值，触发隐蔽错误。修复为调用前驱逐所有已追踪寄存器。

### 3.4 AI 使用说明

本项目在开发过程中使用了 **Claude Code**（Anthropic 的 AI 编程助手）辅助开发。

**使用环节**：
- **调试与错误定位**（40%）：快速分析 110 个测试中的失败用例，通过比对汇编输出定位根因（如 `param_regs` 过期、phi 插入位置错误、GetPtr 操作数求值顺序等）
- **代码生成与修改**（35%）：实现尾调用优化、寄存器追踪修复、内嵌汇编透传等具体 pass；通过 Edit 工具直接修改源码
- **架构咨询**（15%）：讨论 Mem2Reg 的多定义 phi 策略、寄存器分配器的活跃区间设计、帧省略的安全条件
- **测试执行**（10%）：批量运行测试套件，收集通过率数据

**使用的 Agent**：
- 主对话中直接进行代码分析、修改和测试
- 未启用多 Agent 工作流（Workflow），所有工作在主会话中完成

**使用方式与监督**：
- 所有 AI 生成的代码由人工审查后通过 Edit 工具应用
- 每次修改后立即运行完整测试套件（`cargo test` + 110 功能测试）验证
- AI 辅助发现的 bug（如调用寄存器损坏、尾调用匹配失败）均由人工确认根因后再修复
- 关键设计决策（IR 结构、寄存器池大小、phi 排除策略）由人工主导，AI 提供分析支持

**预估用量**：
- 对话轮次：约 80–100 轮
- 预估 Token 消耗：约 200–300K input tokens，50–100K output tokens
- 模型：Claude (deepseek-v4-pro 后端)

**经验教训**：
- **优势**：AI 极大加速了调试周期——在 110 个测试中定位单一失败用例并比对汇编差异，手工可能需要 30 分钟/用例，AI 辅助降至 2–5 分钟。代码模式识别（如跨调用寄存器损坏）上 AI 表现出色。
- **局限**：AI 倾向于过度工程化——多次试图实现复杂的 IR 级尾调用模式匹配，而更简单的代码生成级方案即可。对于编译器理论问题（支配边界、SSA 重命名），AI 的推理偶尔出现细节错误，需要人工验证。
- **最佳实践**：每次修改后立即跑完整测试是最有效的质量保障；AI 提出的方案应保持怀疑，优先选择最小改动量方案。

---

## 四、达成效果

### 4.1 功能测试

SysY 官方公开测试集 lv1–lv9 总计 110 个用例，覆盖：

| 等级 | 用例数 | 覆盖特性 |
|------|--------|----------|
| lv1 | 7 | 基本运算、main 函数 |
| lv3 | 28 | 变量声明、赋值、作用域 |
| lv4 | 14 | 常量声明 |
| lv5 | 7 | if/else 分支 |
| lv6 | 8 | while 循环 |
| lv7 | 12 | break/continue |
| lv8 | 12 | 函数定义与调用、短路求值 |
| lv9 | 22 | 数组、多维数组、数组参数 |

```
功能测试 (lv1–lv9):  110 / 110  ✅  (100%)
单元测试:              14 / 14   ✅  (100%)
性能测试:              13 / 30   ✅  (其余为 qemu 模拟超时)
```

### 4.2 内嵌汇编示例

语句形式：
```c
int main() {
    asm("li a0, 1");   // 插入单条汇编
    asm("fence");       // 内存屏障
    return 0;
}
```

表达式形式：
```c
int main() {
    int cycles = asm("rdcycle a0");  // 读 RISC-V cycle CSR
    return cycles;
}
```

汇编透传——生成的 `.S` 中 `asm("rdcycle a0")` 直接输出为 `  rdcycle a0`。

### 4.3 优化效果示例

以 `int sum(int n) { int s=0; while(n>0) { s=s+n; n=n-1; } return s; }` 为单基本块示例：

| 优化 | 栈帧大小 | 指令数 |
|------|---------|--------|
| 无优化（全栈分配） | 64 bytes | ~35 |
| 寄存器分配 + 帧省略 | 0 bytes（帧消除） | ~12 |

以 `int median(int arr[], int begin, int end, int pos)`（快速选择算法）为例：

| 优化 | 效果 |
|------|------|
| 无尾调用 | 逆序 10 万输入 → 栈溢出段错误 |
| 尾调用优化 | 逆序 10 万输入 → 正常运行（栈 O(1)） |

### 4.4 寄存器分配效果

对 110 个测试用例中的单基本块函数：
- **帧省略率**: 约 60% 的单基本块函数实现了帧省略（寄存器压力 ≤3）
- **叶子函数**: 不保存 `ra`，减少 1 条访存指令
- **死存储消除**: `advance()` 在最后使用后自动释放寄存器，避免不必要的写回

---

## 五、参考文献

1. Lattner, C., & Adve, V. (2004). *LLVM: A Compilation Framework for Lifelong Program Analysis & Transformation*. CGO 2004.

2. Braun, M., et al. (2013). *Simple and Efficient Construction of Static Single Assignment Form*. CC 2013.

3. Poletto, M., & Sarkar, V. (1999). *Linear Scan Register Allocation*. TOPLAS, 21(5).

4. RISC-V International. (2024). *The RISC-V Instruction Set Manual, Volume I: User-Level ISA*.

5. Cytron, R., et al. (1991). *Efficiently Computing Static Single Assignment Form and the Control Dependence Graph*. TOPLAS, 13(4).

6. Aho, A. V., Lam, M. S., Sethi, R., & Ullman, J. D. (2006). *Compilers: Principles, Techniques, and Tools* (2nd ed.). Addison-Wesley.

7. 毕昇编译器团队. (2021). *SysY 语言定义与参考编译器*. https://gitlab.eduxiji.net/pku2400013070/sysy
