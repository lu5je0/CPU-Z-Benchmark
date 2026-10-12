# AGENTS.md

Rust 复刻 CPU-Z 基准测试（OpenSimplex2 噪声渲染）。算法从 `cpuz_x64.exe` 逆向而来，目标是跑分与官方一致。

## 关键命令

```bash
cargo build --release          # 必须 release：分数依赖优化（debug 慢 ~20 倍）
cargo test --release           # 单元测试（表值、噪声有限性）
cargo run --release --bin micro  # 每像素 ns 微基准（三引擎对比）
cargo run --release -- --duration 3000 --threads 24  # 快速验证分数
cargo run --release -- --duration 3000 -q --json     # 脚本/CI 用输出
```

发布：`git tag vX.Y.Z && git push origin vX.Y.Z` 触发 `.github/workflows/release.yml`，
构建 3 个平台产物并发布 Release（资产名 `cpuz-bench-<linux|macos>-<x86_64|aarch64>`）。
`workflow_dispatch` 也能手动跑（只上传 artifact，不发 Release）。

## 分数基准（16C/24T Intel 大小核，17.01 默认模式）

- 实测单线程 869~872，官方参考 872；多线程 12490~12574，官方参考 12287~12659
- 分数 = units/ms（17.01）；units/ms/10（19.01）。units = 渲染行数×1024
- **单点采样函数必须保持 `#[inline(never)]`**：CPU-Z 未内联这些调用，其 call 开销是工作量的一部分。全内联会让分数偏高 ~8%
- `noise.rs` 用 `to_int_unchecked` 截断（对齐 cvttss2si）；`as i32` 是饱和转换，更慢
- **batch rows 会明显影响多线程分数**（`--batch-rows`，单线程默认 8 行、多线程默认 16 行）：
  128 行/任务时 MT 掉到 ~11800：dispatch 尾巴（最后一次派发的任务要等它跑完才算结束）
  按比例摊薄分数；4 行/任务则因派发太频繁、worker 被饿到而掉到 ~11700。16 行是实测最稳的点

## 架构

- `table.rs` 4096 项梯度表（cos 公式，CPU-Z 位级一致，测试锁死位模式）
- `noise.rs` 17.01 / `noise1901.rs` 19.01 / `noise_avx2.rs` AVX2 引擎
- `kernel.rs` 像素混合；`bench.rs` 线程池+派发+计时+评分+进度回调
- `affinity.rs` 每 worker 绑独立逻辑核（CPU-Z 行为；多线程分数依赖它）
- 4MiB RGBA buffer 所有 worker 共享无锁，行不重叠由派发顺序保证；**不要加 Mutex**（会串行化毁分数）
- 逆向笔记：CPU-Z 2.x，函数地址 sub_1400330A0（17.01 单线程驱动）、sub_140033460（多线程驱动）、
  sub_14004DAE0/CE0（17.01 噪声 A/B）、sub_14004DEF0（像素循环）、sub_14004E170（单行 worker）

### 并发模型（踩过的坑）

- worker 用**每线程 `Mutex` + `Condvar`** 交接任务：dispatch 方 `wait_idle`/`dispatch_wait` 阻塞等待，
  worker 完成时 `notify_all`。等价于 CPU-Z 的 alertable `WaitForSingleObjectEx`
- **不要用 `park/unpark`**：`Worker::new` 里 `std::thread::current()` 拿到的是创建者（主线程）的句柄，
  `dispatch()` 实际唤醒的是主线程，worker 永远在 park → `wait_idle()` 死循环。修复前该 bug 一直在
- 不要长 spin（SMT 兄弟核会被拖慢，曾致 10740→7201）；多线程派发在全忙时才 `yield_now`

### 实时分数

- `bench::run(opts, &mut on_progress)`：两个 pass 都在跑的过程中按 `--interval`（默认 200ms）
  回调 `Progress { pass, done, score, units, elapsed_ms, duration_ms, threads }`
- 实时值就是最终评分式子 `units_done / elapsed_ms`，所以会单调收敛到最终分数
- 进度回调在 dispatcher 线程里同步执行；`main.rs` 只在 TTY 下画面板（两行原地刷新），
  管道/`--json`/`-q` 时完全不画，避免污染输出

## 平台

- 支持 Windows/Linux/macOS（`affinity.rs` 按 cfg 分支；libc 仅 Linux 用）
- 运行 CPU-Z 验证分数时：其下拉框版本对应 `--mode`（17.01 SSE / 19.01 SSE / 19.01 AVX2），AVX512 用 AVX2 回退
- 本机 CPU 是 16C/24T Intel 大小核：E-core 拖累多线程扩展，比较分数需固定同机
- CI/release 矩阵：`ubuntu-latest`(x86_64)、`ubuntu-24.04-arm`(aarch64)、`macos-14`(aarch64)

## 提交约定

- 每个小改动单独 commit（用户要求）；commit 信息用英文、`feat:`/`perf:`/`fix:`/`chore:` 前缀
- `.gitignore` 已排除逆向产物（IDA 文件、dump*.py、*.exe）——不要把 IDA 脚本或二进制提交进 git
