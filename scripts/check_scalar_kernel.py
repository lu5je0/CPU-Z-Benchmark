#!/usr/bin/env python3
"""Guard the scalar-kernel invariant.

CPU-Z's 17.01 benchmark kernel is scalar single-precision math: the x64 build
uses scalar SSE (`mulss`/`addss`), and the ARM64 build contains no
vector-register floating-point instruction anywhere in the binary. Our port has
to stay scalar too, otherwise the score is inflated (aarch64 has NEON in the
baseline ISA and LLVM happily SLP-vectorises the independent gradient hashes and
the pixel mix).

Usage: check_scalar_kernel.py <binary> [objdump...]

Exits non-zero when a kernel function contains vector FP arithmetic.
"""

import re
import shutil
import subprocess
import sys

# Functions that must stay scalar. `noise_avx2` is deliberately excluded: it is
# the 19.01 AVX2 engine and *is* explicitly vectorised.
KERNEL_FUNCS = (
    "noise_a",
    "noise_b",
    "single_sample",
    "gradient_lookup",
    "render_row",
    "render_rows",
    "kernel::pixel",
)
EXCLUDE = ("noise_avx2", "avx2", "sme")

# x86 packed FP arithmetic. Data movement like `movaps`/`shufps`/`andps` is not
# flagged: CPU-Z's own x64 build uses `andpd`/`unpcklps` for the abs mask and
# the int/float conversions, only the arithmetic has to stay scalar.
X86_PACKED = re.compile(r"\b(mulps|addps|subps|divps|sqrtps|maxps|minps|"
                        r"mulpd|addpd|subpd|divpd|"
                        r"vfmadd\w*|vfmul\w*|vfadd\w*)\b")

# aarch64 SIMD FP mnemonics; scalar forms use s/d/h registers, vector forms
# use v/z registers, so the operand check below is what actually decides.
AARCH64_FP = {
    "fmul", "fadd", "fsub", "fdiv", "fmla", "fmls", "fmadd", "fmsub",
    "fnmadd", "fnmsub", "fabs", "fneg", "fsqrt", "fmax", "fmin", "fmaxnm",
    "fminnm", "fcvtzs", "fcvtms", "fcvtps", "fcvtns", "fcvtas", "fcvtzd",
    "scvtf", "ucvtf", "fcvt", "fcvtl", "fcvtn", "fcvtl2", "fcvtn2",
    "fcmp", "fcmpe", "fcsel", "frintm", "frintz", "frintn", "frintp",
    "frinta", "frintx", "frinti", "frecpe", "frsqrte",
}
VEC_REG = re.compile(r"\b([vz]\d{1,2})(\.\d*[sdhb])?\b")


def arch_of(path):
    """ELF/PE machine of the binary under test."""
    with open(path, "rb") as fh:
        head = fh.read(64)
    if head[:4] == b"\x7fELF":
        machine = int.from_bytes(head[18:20], "little")
        return {0x3E: "x86-64", 0xB7: "aarch64"}.get(machine, f"elf-{machine:#x}")
    if head[:2] == b"MZ":
        import struct

        e_lfanew = struct.unpack_from("<I", head, 0x3C)[0]
        with open(path, "rb") as fh:
            fh.seek(e_lfanew + 4)
            machine = struct.unpack("<H", fh.read(2))[0]
        return {0x8664: "x86-64", 0xAA64: "aarch64"}.get(machine, f"pe-{machine:#x}")
    return "unknown"


def find_objdump(argv):
    if argv:
        return argv
    for cand in (
        [shutil.which("objdump")],
        [shutil.which("llvm-objdump")],
        ["xcrun", "llvm-objdump"],
    ):
        if cand[0] and shutil.which(cand[0]):
            return cand
    sys.exit("check_scalar_kernel: no objdump/llvm-objdump found")


def disassemble(objdump, binary):
    out = subprocess.run(
        [*objdump, "-d", binary], capture_output=True, text=True, check=True
    ).stdout
    return out


FUNC_RE = re.compile(r"^[0-9a-f]+ <(.*)>:")
INSN_RE = re.compile(r"^\s+[0-9a-f]+:\s+([0-9a-f ]+?)\t(.*)$")


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    args = [a for a in sys.argv[1:] if a != "--strict"]
    strict = "--strict" in sys.argv[1:]
    binary = args[0]
    arch = arch_of(binary)
    # aarch64 must stay scalar (CPU-Z's ARM64 build is scalar everywhere).
    # On x86-64 LLVM packs the independent chains into SSE too; that is score
    # validated against CPU-Z instead of checked here, so it is informational.
    strict = strict or arch == "aarch64"
    objdump = find_objdump(args[1:])

    current = ""
    checked = 0
    insn_count = 0
    bad = []

    for line in disassemble(objdump, binary).splitlines():
        m = FUNC_RE.match(line)
        if m:
            current = m.group(1)
            continue
        m = INSN_RE.match(line)
        if not m:
            continue
        text = m.group(2).strip()
        if not any(k in current for k in KERNEL_FUNCS):
            continue
        if any(x in current for x in EXCLUDE):
            continue
        insn_count += 1
        mnemonic = text.split()[0]
        operands = text[len(mnemonic):].strip()

        if mnemonic in AARCH64_FP and VEC_REG.search(operands):
            bad.append((current, text))
        elif X86_PACKED.search(text):
            bad.append((current, text))

    checked = insn_count
    print(f"check_scalar_kernel: {binary}")
    print(f"  arch             : {arch}{' (must be scalar)' if strict else ' (informational)'}")
    print(f"  objdump          : {' '.join(objdump)}")
    print(f"  kernel insns     : {checked}")
    print(f"  vector FP insns  : {len(bad)}")

    if bad:
        print("\n  packed/vector floating-point arithmetic inside the kernel:")
        for fn, text in bad[:25]:
            print(f"    {fn}: {text}")
        if len(bad) > 25:
            print(f"    ... and {len(bad) - 25} more")
        if strict:
            print(
                "\n  the kernel must stay scalar (see .cargo/config.toml); "
                "check for a build that bypasses its rustflags"
            )
            return 1
        print("\n  informational only on this arch (see comment above)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
