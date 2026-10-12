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

- 实测单线程 872~873，官方参考 872；多线程 12400~12716，官方参考 12287~12659
- 分数 = units/ms（17.01）；units/ms/10（19.01）。units = 渲染行数×1024
- **单点采样函数必须保持 `#[inline(never)]`**：CPU-Z 未内联这些调用，其 call 开销是工作量的一部分。全内联会让分数偏高 ~8%
- `noise.rs` 用 `to_int_unchecked` 截断（对齐 cvttss2si）；`as i32` 是饱和转换，更慢
- 多线程 batch（`--batch-rows`，默认 8 行）对分数影响在 1% 以内（4/8/16/32 行实测 12586~12687）；
  单线程固定 1 行/任务（与 CPU-Z 每行一个 APC 一致）。**不要为了“调度更省”而加大单线程 batch**，见下面的气泡事故

## 架构

- `table.rs` 4096 项梯度表（cos 公式，CPU-Z 位级一致，测试锁死位模式）
- `noise.rs` 17.01 / `noise1901.rs` 19.01 / `noise_avx2.rs` AVX2 引擎
- `kernel.rs` 像素混合；`bench.rs` 线程池+派发+计时+评分+进度回调
- `affinity.rs` 每 worker 绑独立逻辑核（CPU-Z 行为；多线程分数依赖它）
- 4MiB RGBA buffer 所有 worker 共享无锁，行不重叠由派发顺序保证；**不要加 Mutex**（会串行化毁分数）
- 逆向笔记：CPU-Z 2.x，函数地址 sub_1400330A0（17.01 单线程驱动）、sub_140033460（多线程驱动）、
  sub_14004DAE0/CE0（17.01 噪声 A/B）、sub_14004DEF0（像素循环）、sub_14004E170（单行 worker）

### 并发模型（踩过的坑）

- **worker 自驱动拉任务**：`PassState { next: AtomicU64, units: AtomicU64, stop }`，每个 worker 用一次 atomic RMW
  取下一个 batch（`next += batch`），busy-wait 都不需要。这等价于 CPU-Z 的 QueueUserAPC ——
  dispatcher 从不阻塞在渲染上，worker 永远拿得到下一行
- **绝对不要用 dispatcher ↔ worker 逐任务握手**（`park/unpark` 或 `Mutex+Condvar` 版本都试过）：
  只要 dispatcher 被调度器压住一个 tick，单核分数就会直接掉一半（实测出现 433 vs 872），
  而且 `pool.total_units()` 会重复累加未结算的 job。自驱动方案彻底消除了这类气泡
- 也不要用长 spin（SMT 兄弟核会被拖慢，曾致 10740→7201）
- 共享 4MiB buffer 并发写不相交行：靠 `next` 递增 + `threads * batch <= BENCH_HEIGHT` 保证
  （跨帧同一下标会写同一块内存，会 UB）
- 计时：主线程拿 `Instant` 后全员过 `Barrier` 同时起跑；`stop` 后每人不超一个 batch 就退出，
  所以 `ms` 把尾巴也算进去了，不会少算工作

### 实时分数（滑动窗口）

- `LiveWindow`：每 `--interval`（默认 200ms）存一个 `(elapsed_ms, units)`，左边界取
  “窗口宽 ≈ pass/4（250~1000ms）之外最近的样本”，分数 = 窗口内 `Δunits/Δms`
- 为啥这么做：用“从开头累积的 `units/ms`”会让数字从低往高慢慢爬（早期受 CPU 升频/缺页影响），
  用户明确说过不像 CPU-Z；滑动窗口从第一个采样就是稳态值，然后小幅抖动到定稿
- **每个 pass 最终打印/JSON 里的分数仍是整段平均 `units/ms`**（与 CPU-Z 一致），窗口只影响过程显示

## 平台

- 支持 Windows/Linux/macOS（`affinity.rs` 按 cfg 分支；libc 仅 Linux 用）
- 运行 CPU-Z 验证分数时：其下拉框版本对应 `--mode`（17.01 SSE / 19.01 SSE / 19.01 AVX2），AVX512 用 AVX2 回退
- 本机 CPU 是 16C/24T Intel 大小核：E-core 拖累多线程扩展，比较分数需固定同机
- CI/release 矩阵：`ubuntu-latest`(x86_64)、`ubuntu-24.04-arm`(aarch64)、`macos-14`(aarch64)
- **只有 x86_64 分数能跟 CPU-Z 榜单比**。实测 M5 Pro 单核 2323 / 多核 26652（NEON，同机 13700KF 是 868/12495）：
  这是长依赖链标量浮点负载，苹果核在延迟/OoO 窗口上优势极大，不代表主流基准里的 1.4~1.5x 单核差距。
  非 x86 构建会在输出里打提示（`PLATFORM_NOTE`，按 `target_arch` cfg）
- **不要为了“跨平台更可比”去开 FMA / `target-cpu=native`**：rustc 默认就是 `fp-contract=off`
  （实测：`-C target-feature=+fma` 下只生成 `vmulss`+`vaddss`），两边都是逐次 mul/add；
  一旦开了收缩/自动向量化，分数就不再是 CPU-Z 那套逐次 SSE 标量运算了

## 提交约定

- 每个小改动单独 commit（用户要求）；commit 信息用英文、`feat:`/`perf:`/`fix:`/`chore:` 前缀
- `.gitignore` 已排除逆向产物（IDA 文件、dump*.py、*.exe）——不要把 IDA 脚本或二进制提交进 git
