# AGENTS.md

Rust 复刻 CPU-Z 基准测试（OpenSimplex2 噪声渲染）。算法从 `cpuz_x64.exe` 逆向而来，目标是跑分与官方一致。

## 关键命令

```bash
cargo build --release          # 必须 release：分数依赖优化（debug 慢 ~20 倍）
cargo test --release           # 单元测试（表值、噪声有限性）
cargo run --release --bin micro  # 每像素 ns 微基准（三引擎对比）
cargo run --release -- --duration 3000 --threads 24  # 快速验证分数
```

## 分数基准（16C/24T 机器，17.01 默认模式）

- 单线程目标 ~810（当前 802-817），多线程目标 ~11000（当前 ~10740）
- 分数 = units/ms（17.01）；units/ms/10（19.01）。units = 渲染行数×1024
- **单点采样函数必须保持 `#[inline(never)]`**：CPU-Z 未内联这些调用，其 call 开销是工作量的一部分。全内联会让分数偏高 ~8%（曾出现 876 vs 目标 810）
- `noise.rs` 用 `to_int_unchecked` 截断（对齐 cvttss2si）；`as i32` 是饱和转换，更慢
- worker 同步用 `park/unpark` 或 `yield_now`（dispatch 侧），**不要长 spin**：SMT 兄弟核会被拖慢，曾致 10740→7201

## 架构

- `table.rs` 4096 项梯度表（cos 公式，CPU-Z 位级一致，测试锁死位模式）
- `noise.rs` 17.01 / `noise1901.rs` 19.01 / `noise_avx2.rs` AVX2 引擎
- `kernel.rs` 像素混合；`bench.rs` 线程池+派发+计时+评分
- `affinity.rs` 每 worker 绑独立逻辑核（CPU-Z 行为；多线程分数依赖它）
- 4MiB RGBA buffer 所有 worker 共享无锁，行不重叠由派发顺序保证；**不要加 Mutex**（会串行化毁分数）
- 逆向笔记：CPU-Z 2.x，函数地址 sub_14002FC00（17.01 单线程）、sub_14004A340/540（17.01 噪声 A/B）、sub_140030570（19.01 像素循环）、sub_14032A8D0（AVX2 噪声 A）

## 平台

- 支持 Windows/Linux（`affinity.rs` 按 cfg 分支；libc 仅 Linux 用）
- 运行 CPU-Z 验证分数时：其下拉框版本对应 `--mode`（17.01 SSE / 19.01 SSE / 19.01 AVX2），AVX512 用 AVX2 回退
- 本机 CPU 是 16C/24T Intel 大小核：E-core 拖累多线程扩展，比较分数需固定同机

## 提交约定

- 每个小改动单独 commit（用户要求）；commit 信息用英文、`feat:`/`perf:`/`chore:` 前缀
- `.gitignore` 已排除逆向产物（IDA 文件、dump*.py、*.exe）——不要把 IDA 脚本或二进制提交进 git
