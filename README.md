# MiniOS (RISC-V64 + Rust)

运行在 RISC-V64 S 态的最简内核：启动后通过 **OpenSBI** 控制台打印信息，
安装 trap 入口并开启 **S 态时钟中断**，创建多个内核线程，并用时钟中断做
**抢占式轮转（Round-Robin）调度**，最后优雅关机。

## 启动流程

```
QEMU virt ──加载──> OpenSBI (M 态, 0x80000000)
                        │ 跳转 (a0=hartid, a1=DTB)
                        ▼
              MiniOS _start (S 态, 0x80200000)
                        │ 清 .bss / 设内核栈
                        ▼
                    rust_main
                        │ 安装 stvec / 创建任务 / 开 STIE / set_timer
                        ▼
                   task::run ──> __restore ──sret──> task_alpha
                                                         │
                     ┌──────────── 时钟中断 ─────────────┘
                     ▼
                  __alltraps (保存现场) ──> trap_handler
                     ▲                          │
                     │                          ▼
                 __restore ◄─── on_timer_tick（时间片到了就换任务）
                     │
                     └── sret 到「下一个任务」继续跑
```

## 功能

- 通过 OpenSBI legacy console 打印（`println!` 宏）
- Trap 基础设施：`stvec` + 完整寄存器现场保存/恢复 + `sret`
- S 态时钟中断：SBI Time 扩展 `set_timer`，周期性 tick
- **内核线程**：每个任务独立内核栈 + 独立 `TrapContext`
- **抢占式轮转调度**：时钟中断驱动，任务无需主动让出 CPU
- 异常处理：panic 时打印 `scause` / `stval` / `sepc`
- SBI 扩展探测（Base `probe_extension`）与优雅关机（SRST）

> 演示用：发生 8 次任务切换后自动关机，方便 `make run` 自动结束。
> 想常驻运行，删掉 `src/task.rs::on_timer_tick` 里的关机分支即可。

运行输出示例：

```
[task 0] 'alpha' ready, entry=0x802000ee, stack_top=0x802070e0
[task 1] 'beta'  ready, entry=0x802001e2, stack_top=0x8020b0e0
[task 2] 'gamma' ready, entry=0x80200168, stack_top=0x8020f0e0
[sched  0] boot -> alpha (id=0, entry=0x802000ee)   (first task)
        [alpha] heartbeat #1
        ...
[sched  1] alpha -> beta
        [ beta] heartbeat #1
        ...
```

## 调度器是怎么工作的（重点）

核心只有一句话：**`trap_handler` 返回“下一次 `sret` 该从哪份现场恢复”。**

1. 任务运行时被时钟中断打断，硬件跳到 `__alltraps`；
2. `__alltraps` 在当前任务的**内核栈**上压出一份 `TrapContext`（保存全部寄存器）；
3. 它把这份现场的地址通过 `a0` 传给 `trap_handler`；
4. `trap_handler` 调用 `task::on_timer_tick`：
   - 时间片没用完 → 返回**当前**现场地址；
   - 时间片用完 → 记下当前现场、挑下一个任务、返回**下一个任务**的现场地址；
5. 汇编 `__restore` 用这个地址重设 `sp`，从对应现场恢复寄存器并 `sret`。

于是“一次普通的中断返回”就变成了“一次任务切换”。切换时并不需要额外保存
寄存器，因为 `__alltraps` 已经替我们保存好了。

关键文件对照：

| 概念 | 位置 |
|---|---|
| 寄存器现场 `TrapContext`、汇编 `__alltraps` / `__restore` | `src/trap.rs` |
| 任务元数据、内核栈、轮转调度 `on_timer_tick` | `src/task.rs` |
| 演示任务 `task_alpha/beta/gamma`、启动流程 | `src/main.rs` |

## 环境要求（本项目均已在 MSYS2 ucrt64 中具备）

| 组件 | 说明 |
|---|---|
| QEMU | `qemu-system-riscv64` + 自带 OpenSBI 固件 |
| Rust | stable + target `riscv64gc-unknown-none-elf` |
| C-RISCV 工具链 | `riscv64-unknown-elf-objcopy` / `objdump` / `size` |
| GDB（可选） | `gdb-multiarch`，用于 `make run-debug` |
| 构建 | `mingw32-make` |

## 使用

```bash
# 在 MSYS2 终端中
mingw32-make build      # 编译并生成 minios.bin
mingw32-make run        # 在 QEMU 中运行（切换 8 次后自动关机）
mingw32-make asm        # 反汇编内核
mingw32-make size       # 查看各段大小
mingw32-make run-debug  # 启动 QEMU 并暂停等待 gdb 连接
mingw32-make clean
```

也可以手动执行 QEMU：

```bash
qemu-system-riscv64 -machine virt -m 128M -smp 1 -nographic \
  -bios C:/msys64/ucrt64/share/qemu/opensbi-riscv64-generic-fw_dynamic.bin \
  -kernel minios.bin
```

## 目录结构

```
├── Cargo.toml          # no_std 裸机包配置
├── .cargo/config.toml  # 默认 riscv64 target + rust-lld 链接参数
├── linker.ld           # 链接到 0x80200000，预留 32 KiB 内核栈
├── Makefile            # build / run / run-debug / asm / size / clean
└── src/
    ├── main.rs         # _start 汇编入口 + rust_main + print!/println! + 演示任务
    ├── sbi.rs          # SBI 封装：console_putchar / set_timer / time / shutdown
    ├── trap.rs         # TrapContext + __alltraps/__restore + trap_handler
    └── task.rs         # 任务元数据 + 内核栈 + 轮转调度器 on_timer_tick
```

## 学习路线建议

按下面的顺序读代码，每一步都只依赖前一步：

1. `main.rs::_start`：清 `.bss`、设栈、`call rust_main` —— 裸机第一条指令。
2. `sbi.rs`：`ecall` 进 M 态做特权操作，理解 S 态 / M 态分工。
3. `trap.rs` 的 `__alltraps` / `__restore`：理解“现场保存 / 恢复”。
4. `trap.rs` 的 `trap_handler`：理解 `scause` 如何区分中断与异常。
5. `task.rs` 的 `spawn` / `on_timer_tick`：理解任务栈、时间片、轮转。
6. `task.rs::run`：理解“用 `__restore` 启动第一个任务”的技巧。

## 扩展方向

- 给任务加上 `Exited` 状态与 `exit()`，实现任务结束 / 回收
- 用 `yield_now()`（已在 `task.rs` 预留）实现协作式调度，与抢占式对比
- 每个任务一个独立页表（Sv39），为跑用户态程序（U 态 + `ecall` 系统调用）铺路
- 解析 DTB 获取物理内存布局，实现页帧 / 堆分配器，从而**动态**创建任务栈
- 把打印方式换成直接驱动 16550 UART（`0x10000000` 内存映射），减少 SBI 依赖
- 加入优先级 / 睡眠队列，把“轮转”升级成更真实的调度器
