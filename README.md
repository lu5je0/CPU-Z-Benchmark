# cpuz-bench

Rust 复刻的 CPU-Z 基准测试（CPU Benchmark），算法与官方 CPU-Z 完全一致：

- **工作负载**：1024×1024 RGBA 噪声图渲染，每像素做两次噪声采样 + 颜色混合
- **噪声算法**：OpenSimplex2 2D（Kurt Spencer, Public Domain），CPU-Z 使用的变体（拉伸常量 0.36602542 / 0.21132487，4096 项梯度表）
- **评分公式**：`score = 完成的工作单元数 / 耗时毫秒`（与 CPU-Z 一致，单线程/多线程分开测）
- **跑分流程**：先单线程跑满 10 秒，再多线程跑满 10 秒，输出两个分数

## 支持的基准模式（与 CPU-Z 下拉框对应）

| 模式 | CPU-Z 内部值 | 噪声实现 | 分数参考（i7-13700K 量级） |
|------|-------------|---------|---------------------------|
| 17.01 SSE（默认） | 0x17016400 | SSE 标量，10+7 octave | 单线程 ~800-900 |
| 19.01 SSE | 0x19016400 | SSE 标量，权重表 | 单线程 ~200-250 |
| 19.01 AVX2 | 0x19016402 | AVX2 8-lane 真 SIMD | 单线程 ~900-1000 |
| 19.01 AVX512 | 0x19016403 | 暂以 AVX2 回退 | - |

> 分数 = 完成的工作单元数 / 毫秒。17.01 直接相除，19.01 再除以 10
> （与 CPU-Z 的 sub_14002FC00 返回 `v13/v31`、sub_1400307F0 返回
> `v38/v36/10` 一致）。三种模式在本机跑出的相对分数与 CPU-Z 一致：
> 19.01-AVX2 ≈ 17.01 ≈ 5× 19.01-SSE。

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

# 指定测试时长（毫秒，默认 10000）
./target/release/cpuz-bench --duration 5000
```

## 实现结构

```
src/
  table.rs      # 4096 项梯度表初始化（cos 公式，与 CPU-Z 一致）
  noise.rs      # OpenSimplex2 单点采样（17.01 变体）
  noise1901.rs  # 19.01 变体（权重表）
  kernel.rs     # 每像素混合渲染内核
  bench.rs      # 跑分循环：线程池 + 任务派发 + QPC 计时 + 评分
  main.rs       # CLI
```

## 分数对齐说明

分数 = 单元数 / 毫秒，其中单元数 = 完成渲染的像素数。这与 CPU-Z 的评分逻辑完全一致：

- CPU-Z 17.01: `score = 单元 / ms`（sub_14002FC00 返回 `(float)v13 / v31`）
- CPU-Z 19.01: `score = 单元 / ms / 10`（sub_1400307F0 返回 `(float)((float)v38 / v36) / 10.0`）

多线程分数 = 所有线程累计单元 / ms（17.01）或 /10（19.01）。

> 注：AVX2/AVX512 模式需要支持相应指令集的 CPU；在纯 SSE 机器上会自动回落。
