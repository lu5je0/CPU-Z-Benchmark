# cpuz-bench

Rust 复刻的 CPU-Z 基准测试（CPU Benchmark），算法与官方 CPU-Z 一致，带 CLI 与实时分数面板：

- **工作负载**：1024×1024 RGBA 噪声图渲染，每像素做两次噪声采样 + 颜色混合
- **噪声算法**：OpenSimplex2 2D（Kurt Spencer, Public Domain），CPU-Z 使用的变体（拉伸常量 0.36602542 / 0.21132487，4096 项梯度表）
- **评分公式**：`score = 完成的工作单元数 / 耗时毫秒`（与 CPU-Z 一致，单线程/多线程分开测）
- **跑分流程**：先单线程跑满 10 秒，再多线程跑满 10 秒，两个分数在跑的过程中实时刷新并逐渐稳定

## 实时分数

TTY 下运行时先出现单线程、再多线程，分数从第一次采样（200ms）起就是稳态值，
之后小幅抖动到定稿——不会从低往高慢慢爬：

```
cpuz-bench 0.2.1 - CPU-Z benchmark (17.01)
CPU      : 13th Gen Intel(R) Core(TM) i7-13700KF
Settings : 24 threads, 10000 ms/pass, batch 8 rows, live score

  Single Thread [████████████████████████] 100%  score    872   10.0s/10.0s
  Multi  Thread [███████░░░░░░░░░░░░░░░░]  30%  score  12711    3.0s/10.0s

=== CPU-Z Benchmark (17.01) ===
CPU (Single Thread) :      872   (8724480 px in 10004 ms)
CPU (Multi Thread)  :    12665   (126746624 px in 10005 ms, 24 threads)
```

显示的是**最近约 1/4 pass 时长的瞬时速率**（`--interval` 控制采样间隔，默认 200ms），
所以一开始就是稳态值；每个 pass 结束时打印的仍是整段平均 `units/ms`（与 CPU-Z 一致）。

非 TTY（管道/重定向）时不会画面板，只在结束时输出结果；`--json` / `-q` 同样不发面板。

## 支持的基准模式（与 CPU-Z 下拉框对应）

| 模式 | CPU-Z 内部值 | 噪声实现 | 分数参考（i7-13700KF 量级） |
|------|-------------|---------|---------------------------|
| 17.01 SSE（默认） | 0x17016400 | SSE 标量，10+7 octave | 单线程 ~872，多线程 ~12600 |
| 19.01 SSE | 0x19016400 | SSE 标量，权重表 | 单线程 ~220（约为 17.01 的 1/5） |
| 19.01 AVX2 | 0x19016402 | AVX2 8-lane 真 SIMD | 单线程 ~1140 |
| 19.01 AVX512 | 0x19016403 | 暂以 AVX2 回退 | - |

> 分数 = 完成的工作单元数 / 毫秒。17.01 直接相除，19.01 再除以 10
> （与 CPU-Z 的 sub_14002FC00 返回 `v13/v31`、sub_1400307F0 返回
> `v38/v36/10` 一致）。实测 17.01：单线程 872 对官方 872，多线程 12665 对官方
> 12287~12659，误差在 1% 以内。

## 下载预编译版本

GitHub Release 提供三个平台的单文件二进制（由 `.github/workflows/release.yml` 构建）：

```
cpuz-bench-linux-x86_64
cpuz-bench-linux-aarch64
cpuz-bench-macos-aarch64
```

```bash
# 以 linux-x86_64 为例
curl -fL -o cpuz-bench https://github.com/lu5je0/CPU-Z-Benchmark/releases/latest/download/cpuz-bench-linux-x86_64
chmod +x cpuz-bench && ./cpuz-bench
```

dotfiles 里封装了自动下载 + 缓存 + 运行的入口：`q-cpuz-benchmark`
（`bin/common/q-cpuz-benchmark`，用法 `q-cpuz-benchmark [--duration 5000] ...`）。

## 构建

```bash
cargo build --release
```

## 运行

```bash
# 默认 17.01 SSE，跑满 10s 单线程 + 10s 多线程
./target/release/cpuz-bench

# 指定模式
./target/release/cpuz-bench --mode 19.01-avx2

# 只跑单线程 / 只跑多线程
./target/release/cpuz-bench --single-only
./target/release/cpuz-bench --multi-only

# 指定测试时长（毫秒，默认 10000）与实时刷新间隔（默认 200ms）
./target/release/cpuz-bench --duration 5000 --interval 100

# 机器可读输出（适合脚本/CI）
./target/release/cpuz-bench --json

# 只要两行分数（不发面板）
./target/release/cpuz-bench -q
```

完整选项见 `cpuz-bench --help`。

## 实现结构

```
src/
  table.rs      # 4096 项梯度表初始化（cos 公式，与 CPU-Z 一致）
  noise.rs      # OpenSimplex2 单点采样（17.01 变体）
  noise1901.rs  # 19.01 变体（权重表）
  noise_avx2.rs # 19.01 AVX2 8-lane 引擎
  kernel.rs     # 每像素混合渲染内核
  bench.rs      # 跑分循环：线程池 + 任务拉取 + 计时 + 评分 + 实时进度
  affinity.rs   # 每 worker 绑独立逻辑核
  main.rs       # CLI + 实时分数面板
.github/workflows/
  ci.yml        # push/PR 上跑测试与冒烟跑分
  release.yml   # 打 tag 时构建三平台二进制并发布 Release
```

## 分数对齐说明

分数 = 单元数 / 毫秒，其中单元数 = 完成渲染的像素数。这与 CPU-Z 的评分逻辑完全一致：

- CPU-Z 17.01: `score = 单元 / ms`（sub_14002FC00 返回 `(float)v13 / v31`）
- CPU-Z 19.01: `score = 单元 / ms / 10`（sub_1400307F0 返回 `v38/v36/10`）

多线程分数 = 所有线程累计单元 / ms（17.01）或 /10（19.01）。

> 注：AVX2/AVX512 模式需要支持相应指令集的 CPU；在纯 SSE 机器上会自动回落。
