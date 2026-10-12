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
cpuz-bench 0.4.1 - CPU-Z benchmark (17.01)
CPU      : 13th Gen Intel(R) Core(TM) i7-13700KF
Settings : 24 threads, 10000 ms/pass, batch 8 rows, live score

  Single Thread [████████████████████████] 100%  score    872   10.0s/10.0s
  Multi  Thread [███████░░░░░░░░░░░░░░░░]  30%  score  12711    3.0s/10.0s

=== CPU-Z Benchmark (17.01) ===
CPU (Single Thread) :      872   (8724480 px in 10004 ms)
CPU (Multi Thread)  :    12665   (126746624 px in 10005 ms, 24 threads)
Multi / Single      :    14.52x
```

显示的是**最近约 1/4 pass 时长的瞬时速率**（`--interval` 控制重绘间隔，默认 100ms），
所以一开始就是稳态值；每个 pass 结束时打印的仍是整段平均 `units/ms`（与 CPU-Z 一致）。

非 TTY（管道/重定向）时不会画面板，只在结束时输出结果；`--json` / `-q` 同样不发面板。

## 跨平台分数可比性（重要）

**只有 x86_64 的分数能跟 CPU-Z 榜单对比。** 17.01 是一套 x86 SSE 标量工作负载，
在 ARM 上同一份源码映射到 NEON，跑出来的是“这套算法在 NEON 上多快”，
不是“CPU-Z 会不会给这个分”。非 x86 构建会在输出里打一行提示。

实测（同一份代码，各自原生构建，20s 默认跑）：

| 平台 | 单线程 | 多线程 | 每像素 cycle（按各自频率估） |
|------|-------|-------|------------------------|
| i7-13700KF (x86 SSE) | 868 | 12495 | ~6100 @5.3GHz |
| 官方 `cpuz_arm64.exe` 内核（本机原生执行） | 1674 | 18368 | — |
| Apple M5 Pro（标量，v0.2.3+） | 1851 | ~21500 | — |

M5 Pro 的 2323 **不是“比 13700KF 快 2.7 倍”**：主流基准里两者单核差距约 1.4~1.5 倍。
原因是这套负载是**长依赖链 + 每采样一次不可预测分支 + 3 次数据相关查表**的标量浮点测试：
苹果核的 FP/L1 延迟更低（~3 cycle vs ~4/5）、重排序窗口大得多（能重叠每次采样里 3 次独立查表）、
又没有 SMT 兄弟核争用；而 x86 侧还背着 cpu-z 原版的 `cvttss2si`+`comiss` 手动 floor、
随机分支训错等固有开销。这类串行延迟型负载正是苹果提升、x86 吃亏的地方。

关于 CPU-Z 自己的 Windows ARM64 版（`cpuz_arm64.exe`）：它的 17.01 内核是**纯标量**的
（没有任何带向量寄存器的浮点指令，`fmul`/`fadd`/`fabs` 全在 `s` 寄存器上），而且**就是我们 x64 逆向的同一套
算法**——10+10+7+7 = 34 次采样/像素、同一组常量（含 3.3 和 0.9）、同一套 `v15`/`v16` 混色公式。
计分单位也一样是 **pixels/ms**（一个 1024×1024 frame 的 work 计数器正好是 1048576），参考分表
（13 项，含 X Elite 820/9165）就存在这个单位下，所以 ARM64 分数本来就与 x64 同量纲。

不过 CPU-Z 的 ARM64 构建由 **MSVC** 编译，同一份内核它比我们用 LLVM 编译慢（本机 M5 Pro 15 核：
官方内核单核 1674 / 多核 15T 18368 px/ms，我们单核 1851 / 多核 ~21874，均我们更快），
且我们执行的指令数其实更多、只是调度更好。换句话说 **CPU-Z ARM64 的机台分数会被它的编译器再压一道**：
这是构建产物差异，不是工作量差异（见 AGENTS.md 的 ARM64 保真度一节）。
想要 CPU-Z 的真实分数就用 `scripts/official_arm64_kernel.c` 直接跑官方内核。

> 之前引用的 [CPU-reZ](https://github.com/DannielYu219/CPU-reZ) 重写并**不是**官方算法：
> 它只做 2×10 = 20 次采样/像素（少了 noise_b 的两个 7 次循环），混色也不同，
> 还把分数按 `rows/ms × 256` 重新缩放过。它自报的 M5 Pro headline 1554 由此而来，
> 既不是官方数字也不是官方量纲。要拿官方真值，用
> [`scripts/official_arm64_kernel.c`](scripts/official_arm64_kernel.c) 在本机直接跑官方内核。
所以 aarch64 构建里我们用 `.cargo/config.toml` 关掉了 LLVM 的 SLP/循环向量化：
不关的话内核会被自动打包成 4-wide NEON（实测 143 条向量 FP 指令），分数虚高。
`scripts/check_scalar_kernel.py` 在 CI 里守这个不变量，CI 的 `arm64` job 还会在真 ARM 机器上做
“标量 vs 允许向量化”的 A/B。

> 注：x86_64 构建**不**加这两个 flag：那边 LLVM 会把相互独立的链打包成 SSE（61 条 packed 指令），
> 且分数恰好与官方 CPU-Z 一致（872 对 872）；强行关掉会变成 912，反而偏离官方。

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

GitHub Release 提供四个平台的单文件二进制（由 `.github/workflows/release.yml` 构建）：

```
cpuz-bench-linux-x86_64
cpuz-bench-linux-aarch64
cpuz-bench-macos-aarch64
cpuz-bench-android-aarch64     # Termux（NDK 交叉编译，链 bionic）
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

# 指定测试时长（毫秒，默认 10000）与实时刷新间隔（默认 100ms）
./target/release/cpuz-bench --duration 5000 --interval 100

# 机器可读输出（适合脚本/CI）
./target/release/cpuz-bench --json

# 只要两行分数（不发面板）
./target/release/cpuz-bench -q
```

完整选项见 `cpuz-bench --help`。

### ARM64：默认用官方内核计分

在 aarch64 上（Apple Silicon / ARM Linux），如果手上没官方 `cpuz_arm64.exe`，跑出来的是我们自己的
LLVM 构建，它比 CPUID 的 MSVC 构建快，因此**不是 CPU-Z 会显示的分**。所以 CLI 在 aarch64 上会优先找
官方 exe 并用它计分，找不到才回退到内置引擎（并在开头打提示）：

```bash
# 默认：按顺序找 --official 的路径 -> $CPUZ_ARM64_EXE -> 缓存路径
#   ${XDG_CACHE_HOME:-~/.cache}/cpuz-benchmark/cpuz_arm64.exe
./target/release/cpuz-bench

# 显式指定
export CPUZ_ARM64_EXE=/path/to/cpuz_arm64.exe
./target/release/cpuz-bench                     # Source: official cpuz_arm64.exe v1.06 kernel
./target/release/cpuz-bench --official /path/to/cpuz_arm64.exe

# 顺便跑一遍我们自己的引擎，看差多少
./target/release/cpuz-bench --compare
#   This crate (LLVM)   :     1793 / +14.6%   and    21080 / +21.1%

# 强制只用内置引擎
./target/release/cpuz-bench --no-official
```

官方 exe 直接从本仓库的 release 拿：`cpuz_arm64.exe` 跟三平台二进制一起发布（从 CPUID 原始源
<https://download.cpuid.com/cpu-z/arm64/> 下载后原样转发，见 [NOTICE](NOTICE)：该文件版权属 CPUID，
不适用本项目的 MIT 许可）。v1.05 和 v1.06 都支持（用常量池签名定位，不依赖固定地址）。
`q-cpuz-benchmark` 封装会自动把它放到缓存目录；想跳过就走 `--no-official`。

### x86_64：默认同样用官方内核计分

x86_64 上 CLI 默认也是官方内核优先：按 `--official <path>` -> `$CPUZ_X64_EXE` -> 缓存
`${XDG_CACHE_HOME:-~/.cache}/cpuz-benchmark/cpuz_x64.exe` 的顺序查找；找不到就回退到内置引擎
（内置引擎本来就与官方发布分一致，回退时会打提示）：

```bash
# 默认：官方内核优先，找不到就内置引擎
./target/release/cpuz-bench
./target/release/cpuz-bench --official /path/to/cpuz_x64.exe
CPUZ_X64_EXE=/path/to/cpuz_x64.exe ./target/release/cpuz-bench

# 强制只用内置引擎
./target/release/cpuz-bench --no-official
```

官方 exe 同样随 Release 发布：从 <https://download.cpuid.com/cpu-z/> 的
`cpu-z_3.01-en.zip` 里原样转发 `cpuz_x64.exe`（见 [NOTICE](NOTICE)）。
已对 CPU-Z 2.10 / 2.15 / 2.18 / 3.01 验证（用 row worker 参数块前导签名定位，不依赖固定地址）：
本机 13700KF 上官方内核单线程 865~872 px/ms，多线程 ~12500~12700，与本项目内置引擎一致。
目前只映射了 17.01 内核；`--mode 19.01-*` 会自动回退到内置引擎。

> 官方 x64 内核与我们的 Rust 移植在像素级并不逐字节相同（两者都是正确的，只是 LLVM 与 MSVC
> 的标量浮点指令顺序不同，1 ulp 差异经 `fract(· × 43758.547)` 放大成个别像素的色差），
> 分数才是对齐目标。

### Android / Termux

Android（aarch64）是独立 `target_os`，但底层就是 bionic/Linux，所以支持方式和其他 aarch64 一样：

```bash
# 1) 直接下预编译产物（NDK 交叉编译，动态链 bionic，Termux 里可直接跑）
curl -fL -o cpuz-bench \
  https://github.com/lu5je0/CPU-Z-Benchmark/releases/latest/download/cpuz-bench-android-aarch64
chmod +x cpuz-bench && ./cpuz-bench --duration 3000

# 2) 或者自己在 Termux 里编译（pkg install rust 之后，无需 NDK）
pkg install rust
cargo build --release --bin cpuz-bench
./target/release/cpuz-bench
```

**Termux 上同样能直接跑 CPU-Z 的官方 ARM64 内核**，和 Apple Silicon 的 `--official` 是同一套做法：
`cpuz_arm64.exe` 的 17.01 内核是纯标量 AArch64、无 import、除参数块和梯度表外无外部数据，
所以 `src/official.rs` 把它按段摆进一块匿名内存就能原生调用——Android 和 macOS 的指令集、
调用约定完全一致：

```bash
# 从 CPU-Z 官网下载 cpuz-arm64_1.06.zip，解出 cpuz_arm64.exe 放到 Termux 里
./cpuz-bench --official ~/cpuz_arm64.exe     # Source: official cpuz_arm64.exe v1.06 kernel
./cpuz-bench --official ~/cpuz_arm64.exe --compare   # 对比我们 LLVM 构建快多少
```

前提是这台机器的 SELinux 策略允许 `execmem`（`mmap` RW → `mprotect` RX，正是 JIT/QEMU 用的同一条路）。
Termux 的目标 SDK 足够低，原生 Termux 里 JIT 是可用的；但如果某个 ROM 把匿名可执行内存也封了，
`mprotect(PROT_EXEC) failed` 会直接打出来并自动回退到内置引擎，不会崩。

注意点：

- **分数只在同机同架构内可比。** 这是标量 AArch64 工作负载，不是 CPU-Z 给 x86 的那张榜。
- `src/affinity.rs` 在 Android 上也走 `sched_setaffinity`，每个 worker 绑一个逻辑核；
  大小核手机上不绑的话调度器会把几个 worker 塞到同一个核上，多线程分会崩。
- **单线程 pass 在 Android 上绑“最快的可达核”，不是 CPU 0**：手机内核把最快的簇编在最后
  （骁龙 8E5 的 cpu0~5 是 3.63GHz 性能核，cpu6/7 才是 4.74GHz prime），照搬桌面那套绑 CPU 0
  会实打实少记 ~25% 的单核分。读 `cpufreq/cpuinfo_max_freq` 挑最高的，并先过一遍
  `sched_getaffinity`：Android 的 cpuset cgroup 可能把 prime 簇对 app 整个藏掉，
  那种情况软件无解（绑不过去），分数就只能是性能核的水平。其他平台仍然保持 CPU-Z 的 CPU 0
  （x86/Apple Silicon 已验证的分数都是在那上面测的）。
- 内置引擎的 aarch64 构建沿用 `.cargo/config.toml` 里的标量配置（Termux 的
  `aarch64-linux-android` 目标已加入），否则 LLVM 的 SLP 会把它向量化、单核虚高到 ~2300。
- CPU 名识别：`/proc/cpuinfo` 在部分厂商 ROM 上被裁剪，会再试 `getprop ro.soc.model` /
  `ro.board.platform` / `ro.product.board` / `ro.hardware`。

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
  official.rs     # aarch64：映射并调用官方 cpuz_arm64.exe 的 17.01 内核
  official_x64.rs # x86_64：映射并调用官方 cpuz_x64.exe 的 17.01 内核
  main.rs       # CLI + 实时分数面板
.github/workflows/
  ci.yml        # push/PR 上跑测试与冒烟跑分
  release.yml   # 打 tag 时构建四平台二进制并发布 Release
```

## 分数对齐说明

分数 = 单元数 / 毫秒，其中单元数 = 完成渲染的像素数。这与 CPU-Z 的评分逻辑完全一致：

- CPU-Z 17.01: `score = 单元 / ms`（sub_14002FC00 返回 `(float)v13 / v31`）
- CPU-Z 19.01: `score = 单元 / ms / 10`（sub_1400307F0 返回 `v38/v36/10`）

多线程分数 = 所有线程累计单元 / ms（17.01）或 /10（19.01）。

> 注：AVX2/AVX512 模式需要支持相应指令集的 CPU；在纯 SSE 机器上会自动回落。
