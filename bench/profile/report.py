#!/usr/bin/env python3
"""Summarise samples written by sampler.c: the functions where the time was spent (self), and
the functions on the sampled stacks to a depth of four (inclusive).

    python3 bench/profile/report.py out.txt [more.txt ...] [--main] [--top N]

Several files (several runs of a short program) are added together; each is symbolised with
its own memory map, since addresses differ between runs. `--main` counts only samples taken on
the main thread. Symbols come from `llvm-symbolizer` (LLVM 19's, or any on the PATH);
functions in libraries without symbols are shown as `?? (library+offset)`. A sample in a
system call stub is shown as the stub; its caller is in the samples' stack-pointer words.
"""

import collections
import shutil
import subprocess
import sys


def symbolizer() -> str:
    for name in ("llvm-symbolizer-19", "llvm-symbolizer"):
        if shutil.which(name):
            return name
    sys.exit("llvm-symbolizer not found")


def read(path: str, main_only: bool):
    maps, samples = [], []
    for line in open(path):
        if line.startswith("M "):
            parts = line[2:].split()
            if len(parts) >= 6 and parts[5].startswith("/"):
                lo, hi = (int(x, 16) for x in parts[0].split("-"))
                maps.append((lo, hi, int(parts[2], 16), parts[5]))
        elif line.startswith("S "):
            fields = line[2:].split()
            if main_only and fields[0] != "1":
                continue
            samples.append([int(x, 16) for x in fields[1:5]])
    base = {}
    for lo, _, offset, path in maps:
        if offset == 0:
            base.setdefault(path, lo)

    def locate(address: int):
        for lo, hi, _, path in maps:
            if lo <= address < hi:
                return path, address - base.get(path, lo)
        return None

    return [[locate(a) if a else None for a in sample] for sample in samples]


def main() -> None:
    args = sys.argv[1:]
    main_only = "--main" in args
    top = 40
    if "--top" in args:
        top = int(args[args.index("--top") + 1])
    files = [a for i, a in enumerate(args) if not a.startswith("--") and (i == 0 or args[i - 1] != "--top")]
    stacks = [stack for f in files for stack in read(f, main_only)]
    wanted = collections.defaultdict(set)
    for stack in stacks:
        for frame in stack:
            if frame:
                wanted[frame[0]].add(frame[1])
    names = {}
    tool = symbolizer()
    for path, addresses in wanted.items():
        addresses = sorted(addresses)
        query = "\n".join(hex(a) for a in addresses)
        out = subprocess.run(
            [tool, "--obj", path, "--functions=linkage", "--no-inlines", "--demangle"],
            input=query, capture_output=True, text=True,
        ).stdout.split("\n\n")
        for address, block in zip(addresses, out):
            name = block.strip().split("\n")[0] if block.strip() else "??"
            if name == "??":
                name = f"?? ({path.rsplit('/', 1)[-1]}+{address:x})"
            names[(path, address)] = name[:100]

    def name(frame):
        return names.get(frame, "??") if frame else "?? (no file)"

    total = len(stacks)
    if total == 0:
        sys.exit("no samples")
    own = collections.Counter(name(stack[0]) for stack in stacks)
    inclusive = collections.Counter()
    for stack in stacks:
        for n in {name(frame) for frame in stack if frame}:
            inclusive[n] += 1
    print(f"{total} samples")
    print("--- self ---")
    for n, c in own.most_common(top):
        print(f"{100 * c / total:5.1f}%  {n}")
    print("--- inclusive, to a depth of four ---")
    for n, c in inclusive.most_common(top):
        print(f"{100 * c / total:5.1f}%  {n}")


if __name__ == "__main__":
    main()
