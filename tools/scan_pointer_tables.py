#!/usr/bin/env python3
"""Scan a 6502 ROM image for candidate pointer/jump tables.

A "pointer table" here is a run of consecutive little-endian 16-bit words that
all point into the ROM's own address range. These are the dispatch tables that
the tracing disassembler (`apps/disasm`) cannot follow on its own — feed the
ones you care about back in with `disasm --table ADDR:COUNT`.

Usage:
    python tools/scan_pointer_tables.py roms/C1541.rom --org 0xC000 --min 6

Example:
    $ python tools/scan_pointer_tables.py roms/C1541.rom
    table @ $E780: 17 pointers  e.g. -> $EA60 $EAEA $EAEA $EAEA
    ...
    # then, to disassemble the routines it points at:
    $ cargo run -p disasm -- roms/C1541.rom --table 0xE780:17
"""

import argparse


def word(rom: bytes, off: int) -> int:
    return rom[off] | (rom[off + 1] << 8)


def scan(rom: bytes, org: int, min_run: int):
    lo, hi = org, org + len(rom) - 1
    tables = []
    i = 0
    while i < len(rom) - 1:
        j, run = i, 0
        while j < len(rom) - 1 and lo <= word(rom, j) <= hi:
            run += 1
            j += 2
        if run >= min_run:
            tables.append((org + i, run))
            i = j  # don't re-report the same table from an interior offset
        else:
            i += 1
    tables.sort(key=lambda t: -t[1])
    return tables


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("rom", help="path to the raw ROM image")
    ap.add_argument("--org", default="0xC000", help="load address (hex or decimal)")
    ap.add_argument("--min", type=int, default=6, help="minimum run length to report")
    ap.add_argument("--top", type=int, default=12, help="how many tables to print")
    args = ap.parse_args()

    org = int(args.org, 0)
    rom = open(args.rom, "rb").read()
    for addr, run in scan(rom, org, args.min)[: args.top]:
        preview = " ".join(
            f"${word(rom, addr - org + 2 * k):04X}" for k in range(min(run, 4))
        )
        print(f"table @ ${addr:04X}: {run} pointers  e.g. -> {preview}")


if __name__ == "__main__":
    main()
