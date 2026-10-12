// Ground-truth timing of CPU-Z's own ARM64 benchmark kernel.
//
// Why this exists
// ---------------
// x86_64 scores can be validated against CPU-Z's published leaderboard, but the
// Windows ARM64 build (`cpuz_arm64.exe`) has its own reference table and no
// online leaderboard. The only way to know what CPU-Z *itself* would score on a
// given Apple Silicon machine is to run CPU-Z's own code on it.
//
// The 17.01 noise kernel in `cpuz_arm64.exe` is pure scalar AArch64 with no
// imports, no vector-register FP and no external data references: it only reads
// a small parameter block and one 4096-float gradient table. That makes it
// loadable and callable directly from a normal process, which is what this
// harness does. It maps the .text section of the user's own copy of
// `cpuz_arm64.exe` (never redistributed) and calls:
//
//   hash    @ VA 0x1400123f8   (v1.06)   gradient hash, leaf
//   simplex @ VA 0x140012538   (v1.06)   one OpenSimplex2 sample
//   tile    @ VA 0x1400126e8   (v1.06)   renders exactly one row
//
// Layout recovered by disassembly (v1.06; v1.05 is shifted, see --va-base):
//   ctx:            +0x38 = pointer to the 4096-float sin/cos table
//   param block:    +0x18 ctx, +0x20 dst, +0x28 x0, +0x2c y0,
//                   +0x30 width, +0x34 row, +0x3c work(out), +0x40 active(out)
//
// The returned work counter is the pixel count of the row (width), i.e. one
// 1024x1024 frame reports 1048576 units, so the score unit is pixels/ms -- the
// same unit as the x86_64 leaderboard, *not* a normalised "rows/ms x 256".
//
// Build / run:
//   clang -O2 -o official_arm64_kernel scripts/official_arm64_kernel.c -lm
//   ./official_arm64_kernel /path/to/cpuz_arm64.exe [dump.raw]
//
// The dumped frame is byte-comparable against this project's own renderer when
// both are fed the same gradient table (see AGENTS.md, "ARM64 fidelity").
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <time.h>
#include <sys/mman.h>

#define W 1024
#define H 1024

// .text of cpuz_arm64.exe v1.06 (PE32+ ARM64) and the kernel entry points.
#define TEXT_FILE_OFF 0x400u
#define TEXT_SIZE     0x238400u
#define TEXT_VA       0x140001000ull
#define TILE_VA       0x1400126e8ull

struct param {
    uint8_t  pad0[0x18];
    void    *ctx;    // +0x18
    uint8_t *dst;    // +0x20
    int32_t  x0;     // +0x28
    int32_t  y0;     // +0x2c
    int32_t  width;  // +0x30
    int32_t  row;    // +0x34
    int32_t  unk38;  // +0x38
    uint32_t work;   // +0x3c
    uint32_t active; // +0x40
    uint8_t  pad1[0x48 - 0x44];
};

struct ctx { uint8_t pad[0x38]; const float *tab; };

typedef void (*tile_fn)(void *, struct param *);

static uint64_t ns_now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (uint64_t)t.tv_sec * 1000000000ull + (uint64_t)t.tv_nsec;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: %s cpuz_arm64.exe [dump.raw]\n", argv[0]);
        return 2;
    }
    FILE *f = fopen(argv[1], "rb");
    if (!f) { perror(argv[1]); return 1; }

    uint8_t *code = mmap(NULL, TEXT_SIZE, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (code == MAP_FAILED) { perror("mmap"); return 1; }
    if (fseek(f, TEXT_FILE_OFF, SEEK_SET) != 0 ||
        fread(code, 1, TEXT_SIZE, f) != TEXT_SIZE) {
        fprintf(stderr, "short read: not the expected cpuz_arm64.exe build?\n");
        return 1;
    }
    fclose(f);
    if (mprotect(code, TEXT_SIZE, PROT_READ | PROT_EXEC) != 0) { perror("mprotect"); return 1; }
    uint8_t *base = code - TEXT_VA;                 // base + VA == code + offset
    tile_fn tile = (tile_fn)(void *)(base + TILE_VA);

    // Table exactly as 0x14001236c: tab[i] = math(i * 2pi * (1/1024) * 4),
    // 2pi is the float 0x40c90fda (the literal 6.283185f rounds to it).
    static float tab[4096];
    for (int i = 0; i < 4096; i++) {
        float x = (float)i * 6.283185f * 0.0009765625f;
        tab[i] = (float)cos((double)(x * 4.0f));
    }
    struct ctx ctx;
    memset(&ctx, 0, sizeof ctx);
    ctx.tab = tab;

    uint8_t *buf = calloc((size_t)W * H * 4, 1);
    if (!buf) { perror("calloc"); return 1; }
    struct param p;
    memset(&p, 0, sizeof p);
    p.ctx = &ctx; p.dst = buf; p.x0 = 0; p.y0 = 0; p.width = W;

    for (int r = 0; r < H; r++) { p.row = r; p.work = 0; tile(NULL, &p); }

    double best = 1e30;
    uint32_t total = 0;
    for (int pass = 0; pass < 3; pass++) {
        uint64_t t0 = ns_now();
        for (int r = 0; r < H; r++) { p.row = r; p.work = 0; tile(NULL, &p); }
        uint64_t t1 = ns_now();
        total = p.work * H;
        double ms = (double)(t1 - t0) / 1e6;
        if (ms < best) best = ms;
    }

    printf("official cpuz_arm64.exe 17.01 kernel, 1024x1024 frame\n");
    printf("  work counter/frame : %u (width x height)\n", total);
    printf("  best frame time    : %.1f ms\n", best);
    printf("  score              : %.0f px/ms   (%.4f rows/ms)\n",
           (double)W * H / best, (double)W * H / best / W);
    printf("  first pixels       : (%d,%d,%d,%d) (%d,%d,%d,%d)\n",
           buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7]);

    if (argc > 2) {
        FILE *o = fopen(argv[2], "wb");
        if (!o) { perror(argv[2]); return 1; }
        fwrite(buf, 1, (size_t)W * H * 4, o);
        fclose(o);
    }
    return 0;
}
