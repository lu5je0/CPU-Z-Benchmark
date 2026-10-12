# AGENTS.md

Rust 复刻 CPU-Z 基准测试（OpenSimplex2 噪声渲染）。算法从 `cpuz_x64.exe` 逆向而来，目标是跑分与官方一致。

## 关键命令

```bash
cargo build --release          # 必须 release：分数依赖优化（debug 慢 ~20 倍）
cargo test --release           # 单元测试（表值、噪声有限性）
cargo run --release --bin micro  # 每像素 ns 微基准（三引擎对比）
cargo run --release -- --duration 3000 --threads 24  # 快速验证分数
cargo run --release -- --duration 3000 -q --json     # 脚本/CI 用输出
cargo run --release -- --official /path/to/cpuz_x64.exe -q  # x86_64 官方内核（默认找到就用）
cargo run --release -- --no-official -q              # 强制内置引擎
```

发布：`git tag vX.Y.Z && git push origin vX.Y.Z` 触发 `.github/workflows/release.yml`，
构建 3 个平台产物并发布 Release（资产名 `cpuz-bench-<linux|macos>-<x86_64|aarch64>`）。
`workflow_dispatch` 也能手动跑（只上传 artifact，不发 Release）。

## 分数基准（16C/24T Intel 大小核，17.01 默认模式）

- 实测单线程 872~873，官方参考 872；多线程 12400~12716，官方参考 12287~12659
- **x86_64 上 CLI 默认优先官方 `cpuz_x64.exe` 内核**（真值来源；本机官方内核单线程 865~873 /
  多线程 ~12500~12700，与内置引擎一致）。没 exe 时回退内置引擎并打提示，`--no-official` 强制内置
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
- `official.rs`（aarch64）/ `official_x64.rs`（x86_64）：映射并调用 CPUID 自家二进制里的 17.01 内核计分
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

- `LiveWindow`：每 `--interval`（默认 100ms）存一个 `(elapsed_ms, units)`，左边界取
  “窗口宽 ≈ pass/4（250~1000ms）之外最近的样本”，分数 = 窗口内 `Δunits/Δms`
- 为啥这么做：用“从开头累积的 `units/ms`”会让数字从低往高慢慢爬（早期受 CPU 升频/缺页影响），
  用户明确说过不像 CPU-Z；滑动窗口从第一个采样就是稳态值，然后小幅抖动到定稿
- **每个 pass 最终打印/JSON 里的分数仍是整段平均 `units/ms`**（与 CPU-Z 一致），窗口只影响过程显示

## 平台

- 支持 Windows/Linux/macOS/Android(Termux)（`affinity.rs` 按 cfg 分支；libc 用于 Linux **和 Android**）
- **Android 是独立 `target_os`（bionic），不是 `linux`**：任何 `#[cfg(target_os = "linux")]`
  都要想一下 `target_os = "android"`（affinity 已改成 `any(linux, android)`）。libc crate 在
  android 上同样导出 `sched_setaffinity` / `cpu_set_t` / `CPU_SET`，所以 pinning 代码不用分叉
- **Termux 上可以直接跑官方 ARM64 内核**：`cpuz_arm64.exe` 的 17.01 内核是纯标量 AArch64、
  无 import，`src/official.rs` 的 mmap + `mprotect(RX)` 在 Android 上同样成立（和 macOS 一样）。
  唯一变数是 SELinux 的 execmem（某些 ROM 会封匿名可执行内存）——被拒时 `mprotect(PROT_EXEC) failed`
  会打出来并回退内置引擎，不会崩
- **单线程必须绑最快核，不能照搬 CPU-Z 的 CPU 0**：`bench.rs` 原来和桌面一样把单线程 worker 钉在
  逻辑 CPU 0，但手机内核把最快的簇编在最后（骁龙 8E5：cpu0~5 = 3.63GHz 性能核，cpu6/7 = 4.74GHz
  prime）。`affinity::cpus_fastest_first()` 读 `cpufreq/cpuinfo_max_freq` 从快到慢排序，并先用
  `sched_getaffinity` 过滤可达集（Android 的 cpuset cgroup 可能把 prime 簇对 app 藏掉，那软件无解；
  绑不可达的核会 EINVAL）。
  - `official.rs`（aarch64 官方内核）现在**每个 worker 都绑**：1T → 最快核，多线程 → 前 n 个核。
    之前它完全不绑核，完全靠调度器——实测骁龙 8E5 上单核只有 582（=3.63GHz 性能核的数值），
    绑到 prime 应该 ~760。
  - 内置引擎的单线程只有 Android 用最快核，其他平台保持 CPU 0（x86/Apple Silicon 已验证的 872/1674
    都是在 CPU 0 上测的）。
  - macOS 上 `cpus_fastest_first()` 返回空（Mach 亲和性是 hint，本来就没实现）→ 仍然不绑，
    M5 Pro 的 1674/18368 不变。
  - `official_x64.rs` 仍不绑核（那边未绑核的 865~879 / ~12500~12700 已对拍官方）。
- 运行 CPU-Z 验证分数时：其下拉框版本对应 `--mode`（17.01 SSE / 19.01 SSE / 19.01 AVX2），AVX512 用 AVX2 回退
- 本机 CPU 是 16C/24T Intel 大小核：E-core 拖累多线程扩展，比较分数需固定同机
- CI/release 矩阵：`ubuntu-latest`(x86_64)、`ubuntu-24.04-arm`(aarch64)、`macos-14`(aarch64)、
  `android-aarch64`（`aarch64-linux-android`，在 ubuntu-latest 上用 NDK r27c 交叉编译，
  链 `aarch64-linux-android24-clang`；产物动态链 bionic，Termux 可直接跑）
- **只有 x86_64 分数能跟 CPU-Z 榜单比**。实测 M5 Pro 单核 2323 / 多核 26652（NEON，同机 13700KF 是 868/12495）：
  这是长依赖链标量浮点负载，苹果核在延迟/OoO 窗口上优势极大，不代表主流基准里的 1.4~1.5x 单核差距。
  非 x86 构建会在输出里打提示（`PLATFORM_NOTE`，按 `target_arch` cfg）
- **不要为了“跨平台更可比”去开 FMA / `target-cpu=native`**：rustc 默认就是 `fp-contract=off`
  （实测：`-C target-feature=+fma` 下只生成 `vmulss`+`vaddss`），两边都是逐次 mul/add；
  一旦开了收缩/自动向量化，分数就不再是 CPU-Z 那套逐次 SSE 标量运算了
- **aarch64 必须标量**：官方 `cpuz_arm64.exe` v1.06 整个二进制没有任何向量寄存器浮点指令（已用 capstone 全扫），
  但 aarch64 基线自带 NEON，LLVM 会把独立的 hash/像素混合 SLP 成 4-wide（旧产物里 143 条向量 FP，单核虚高到 2323）。
  `.cargo/config.toml` 里只对三个 aarch64 target 关 SLP/循环向量化（`aarch64-unknown-linux-gnu` /
  `aarch64-apple-darwin` / `aarch64-linux-android`）；**x86_64 故意不加**：
  那边 LLVM 打成 SSE（61 条 packed）但分数与官方一致（872 对 872），强关反而变 912
- 守门脚本：`python3 scripts/check_scalar_kernel.py <binary>`（aarch64 违规 exit 1，x86-64 仅报告）
- **别用 `objdump --disassemble=<symbol>` 做这种检查**：mangled 名匹配不上时它输出为空，
  `grep -c` 返回 0 —— 我因此得到过“x86 是纯标量”的假阴性。要用全量 `-d` + 函数名跟踪
- ARM64 官方的计分链（参考）：计分单位是 **pixels/ms**（一个 1024×1024 frame 的 work 计数器是 1048576），
  参考分表（13 项，X Elite 820/9165）就在这个单位下；两次 10s pass + `Sleep(10)`，4MiB buffer，`CBenchCPU_v17`。
  见下面的“ARM64 保真度”一节。

## x86_64 官方内核（官方 `cpuz_x64.exe`）

x86_64 上 CLI 默认也是官方内核优先（`--official <path>` → `$CPUZ_X64_EXE` →
`${XDG_CACHE_HOME:-~/.cache}/cpuz-benchmark/cpuz_x64.exe`），找不到才回退内置引擎并打提示；
`--no-official` 强制内置。release 会 vendor `cpuz_x64.exe`（从 CPUID 的 `cpu-z_<ver>-en.zip`
里取出），`q-cpuz-benchmark` 也会自动放进缓存。**只映射了 17.01**，`--mode 19.01-*` 自动回退。

`src/official_x64.rs` 的做法：

- 自己解析 PE：按段 VA 把 `.text`/`.rdata`/`.data` 摆进一块匿名映射再 `mprotect(RX)`。
  RIP 相对引用因此全部有效；2.10~3.01 这些构建没有 `.reloc`，但内核调用链只用 PC 相对寻址，
  映射基址无所谓（不是 Windows loader，没有 import 解析）
- **row worker 用签名定位，不写死地址**：`8b 42 34 44 8b 4a 2c 44 8b 42 28`
  （`mov eax,[rdx+34]` / `mov r9d,[rdx+2c]` / `mov r8d,[rdx+28]`），在 2.10 / 2.15 / 2.18 / 3.01
  的 `.text` 里都唯一，函数都是同一段 65 字节直线代码
- 参数块（与 ARM64 相同布局）：`+18 ctx, +20 dst, +28 x0, +2c y0, +30 width, +34 row`，
  输出 `+3c work(out), +40 active(out)`；**ctx 的梯度表指针在 `+30`**（ARM64 是 `+38`）
- work 返回 `width`，所以一帧 1048576 units，score = pixels/ms
- **调用约定**：内核是 MSVC 的 Win64 ABI（rcx/rdx/r8/r9），我们从 Rust 的 `extern "C"`
  （SysV：rdi/rsi/rdx/rcx/…）进 row worker。worker 只读 `rdx` 的参数块、不用别的入参寄存器，
  所以直接声明成 3 参数的 `extern "C"` 调用（第 3 个 SysV 参数正好落在 rdx）；它不碰 shadow space，
  而 Win64 会保留 rdi/rsi 等非易失寄存器，跨 ABI 调用是安全的
- 与我们的 Rust 移植**不做逐字节对拍**：喂同一张梯度表时两边帧仍有差异（LLVM 与 MSVC 的标量
  运算顺序差 ~1 ulp，经 `fract(· × 43758.547)` 放大成个别像素色差）。x64 的验收标准是分数一致
  （ARM 那边的“零梯度表逐字节相同”只验证了结构，不代表逐位等价）
- 实测（本机 13700KF）：官方 2.10 / 2.15 / 2.18 / 3.01 内核单线程 865~879 px/ms、多线程 ~12500~12700，
  与内置引擎同档；`--compare` 可比差值

## ARM64 保真度（官方 `cpuz_arm64.exe`）

用 `scripts/official_arm64_kernel.c` 把官方二进制里的内核机器码在本机原生跑（官方内核是纯标量、无 import、
无外部数据引用，只读一个参数块和 4096 项梯度表，所以可以直接 mmap + 调用；仓库不分发官方二进制，
运行时由用户自备 `cpuz_arm64.exe`）。

已由反汇编 + 原生执行确认的事实（v1.05 与 v1.06 都一样）：

- **官方 ARM64 内核 = 我们 x64 逆向的同一套算法**：外层 `#0xa`/`#0xa`/`#0x7`/`#0x7` 四个循环
  （34 次采样/像素，即 noise_a 10+10 与 noise_b 7+7），常量池 1.1 / 1/1024 / 2.2 / 3.3 / 40 /
  1.2 / 1.6 / 0.6 / 0.8 / 0.7 / 0.4 / 0.2 / 0.1 / 0.9 / 0.3 / 255 逐项相同，混色就是我们那套
  `v15 = n2*0.6+0.5`、`v16 = clamp01(n1+n2)`、`v20/v21/v24`。全 `s` 寄存器标量。
- **计分单位是 pixels/ms**：`tile` 每个 row 返回 `width`，一帧 1024×1024 的 work 计数器正好 1048576。
  不是 rows/ms，也没有 `×256`。
- **CPU-reZ 的重写不是官方算法**：它只做 2×10 = 20 次采样/像素（没有 noise_b 的两个 7 次循环），
  混色是 `v*0.4+0.2/0.3/0.1`，而且按 `rows/ms × 256` 自造缩放。它自报的 M5 Pro headline 1554 与其
  scalar 3.72 rows/ms 都是那个体系的产物，不能当官方基准。（它 spec 写的目标文件 5,203,688 字节 = v1.05，
  但 v1.05 的内核实测也是 10/10/7/7。）
- **算法等价的硬证据**：把梯度表整个设成 0（两边喂同一张表）后，官方内核与我们的 `render_rows`
  渲染出的 1024×1024 帧**逐字节完全相同（0/4194304 差异）**。所以差异只在梯度表里 sin/cos 的 ulp——
  表值乘 43758.547 后 frac 会把 1 ulp 放大成约 ±10 的像素色差（正常运行时 98% 像素差 1~15，这是预期的）。
- **分数差是构建产物差异，不是工作量差异**：本机 M5 Pro（15 核），官方内核单核 626.6 ms/帧
  （**1674 px/ms**）、多核 15T 57.1 ms/帧（**18368 px/ms**，ratio 10.98）；我们的 release 标量版
  单核 ~1833、多核 ~21874（+9.5% / +19%）。我们每采样执行 ~375 条指令（官方 ~316 条），
  却更快，即 IPC 更高——MSVC 的 AArch64 调度比 LLVM 差。`opt-level=1` 仍快 ~8%。
  **没有任何合法手段能把我们的构建拉到 ±1%**，除非手写 aarch64 汇编复刻 MSVC 的指令序列。
  要 CPU-Z 的真实分数，就用这个 harness 跑官方内核，而不是用我们的构建去逼近。

复现：

```sh
# aarch64 上 CLI 默认就走官方内核（找不到 exe 才回退并打提示）
cpuz-bench --duration 3000              # Source: official cpuz_arm64.exe v1.06 kernel
cpuz-bench --duration 3000 --compare    # 顺带跑我们自己的引擎并打差值
cpuz-bench --no-official                # 强制内置引擎

# 单独的对拍工具（C，不依赖本 crate）
clang -O2 -o /tmp/oak scripts/official_arm64_kernel.c -lm -lpthread
/tmp/oak /path/to/cpuz_arm64.exe -            # 单核 + 全核分数（第 3 个参数可指定线程数）
/tmp/oak /path/to/cpuz_arm64.exe /tmp/official.raw 1   # 单核，并 dump 一帧到文件
./target/release/cpuz-bench --no-official --duration 3000 -q   # 我们这边同口径对比
```

官方 exe 的查找顺序：`--official <path>` → `$CPUZ_ARM64_EXE` →
`${XDG_CACHE_HOME:-~/.cache}/cpuz-benchmark/cpuz_arm64.exe`（`q-cpuz-benchmark` 会把它放那里）。
`src/official.rs` 用常量池签名定位内核，不依赖固定地址，v1.05 / v1.06 都支持。

## 提交约定

- 每个小改动单独 commit（用户要求）；commit 信息用英文、`feat:`/`perf:`/`fix:`/`chore:` 前缀
- `.gitignore` 已排除逆向产物（IDA 文件、dump*.py、*.exe）——不要把 IDA 脚本或二进制提交进 git
