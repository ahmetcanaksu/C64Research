# C64Research

A Commodore 64 — CPU, video, sound, disk drive — **built from scratch in Rust,
one chip at a time, as a research project.**

> **This is not another emulator clone.** VICE and friends already emulate the
> C64 with world-class accuracy. This repo has a different goal: to *understand*
> the machine deeply and to build toward **real C64-compatible hardware**. It
> optimizes for **readability and learning**, not for being the fastest or most
> feature-complete emulator.

## Why this exists

Three goals, in order:

1. **Understand the C64 down to the silicon.** Every chip is written from
   scratch and documented. The KERNAL isn't just *run* — it's *translated* into
   readable, tested Rust you can study line-by-line next to the original 6502
   (see [`crates/kernal`](crates/kernal) and [`docs/kernal-boot.md`](docs/kernal-boot.md)).
2. **Be a learning playground.** Clear code over clever code. Each component is a
   small crate with tests that explain what the hardware does and why.
3. **Reach real hardware.** The chip crates are `no_std` and allocation-free so
   the same code can eventually run on a fast microcontroller (RP2040 / Teensy /
   ESP32) — a stepping stone to a homebrew, fully-compatible C64.

If you want to *play* C64 games, use VICE. If you want to *learn how a C64
actually works* — or build one — this is for you.

## What works today

- **6510 CPU** — all 151 official opcodes **plus** the stable undocumented ones,
  BCD mode, verified against the **Klaus Dormann functional test**.
- **Boots the real ROMs.** The genuine KERNAL + BASIC boot to the `READY.`
  prompt (`38911 BASIC BYTES FREE` and all).
- **Runs real software.** Loads `.prg` and `.d64` images; a real shareware game
  (Quarx) boots, runs, and renders.
- **VIC-II video** — text, multicolor, bitmap and multicolor-bitmap modes, 8
  sprites, and **raster interrupts**, rendered per-scanline.
- **SID sound** — 3 voices with ADSR envelopes, played through `cpal`.
- **A working 1541 disk drive** — the real drive firmware boots to its DOS idle
  loop (its own 6502 + two VIA chips).
- **A 6502 disassembler** with control-flow tracing and jump-table seeding.
- **A friendly emulator** ([`apps/emu`](apps/emu)) — minifb window, host-layout
  keyboard translation, clipboard paste, and RUN/STOP.

## Quick start

You need the three C64 ROMs (Commodore's copyright — not included). Fetch them:

```sh
base="https://raw.githubusercontent.com/VICE-Team/svn-mirror/main/vice/data/C64"
for f in kernal-901227-03.bin basic-901226-01.bin chargen-901225-01.bin; do
  curl -fsSL -o "roms/$f" "$base/$f"
done
```

Then:

```sh
cargo test                                   # run everything (56 tests)
cargo run -p emu --release                   # boot to the READY. prompt
cargo run -p emu --release -- game.prg       # boot + load + RUN a program
cargo run -p emu --release -- disk.d64 "*"   # load the first PRG off a disk
```

Emulator keys: type normally (layout-aware), **Ctrl+V** paste, arrows/Home/Del
edit, **PageUp** = RESTORE, **Ctrl+C** = RUN/STOP, **Esc** quits.

Disassemble a ROM:

```sh
cargo run -p disasm -- roms/kernal-901227-03.bin --org 0xE000 \
  --code-table 0xFF81:36 --table 0xFD30:16 > kernal.asm
```

Headless screenshot:

```sh
cargo run -p emu --example screenshot -- game.prg 120 shot.ppm
```

## Repository map

| Crate | What it is |
|-------|-----------|
| [`crates/mos6502`](crates/mos6502) | 6502/6510 CPU — opcode tables + cycle-stepped executor (`no_std`) |
| [`crates/vic2`](crates/vic2) | VIC-II video — modes, sprites, raster IRQ, per-scanline renderer |
| [`crates/sid`](crates/sid) | 6581 SID sound — 3 voices + ADSR |
| [`crates/cia6526`](crates/cia6526) | 6526 CIA — ports, timers, interrupts (keyboard/IRQ) |
| [`crates/via6522`](crates/via6522) | 6522 VIA — used by the 1541 drive |
| [`crates/c64`](crates/c64) | the C64 machine: 6510 + PLA banking + RAM + ROMs + 2 CIAs + VIC + SID |
| [`crates/c1541`](crates/c1541) | the 1541 drive machine + hand-ported DOS routines |
| [`crates/kernal`](crates/kernal) | **the KERNAL translated to readable Rust** — study material |
| [`crates/d64`](crates/d64) | `.d64` disk-image reader |
| [`apps/emu`](apps/emu) | windowed emulator (video + sound + keyboard) |
| [`apps/disasm`](apps/disasm) | 6502 disassembler |
| [`tools/`](tools) | small research helpers (jump-table scanner, etc.) |

## Roadmap

**Done**
- 6502/6510 CPU (official + illegal opcodes, Klaus-verified)
- Tracing disassembler
- 1541 drive that boots its DOS
- C64 that boots the real KERNAL + BASIC to `READY.`
- Program loading (`.prg` / `.d64`)
- VIC-II (all display modes, sprites, raster interrupts)
- SID sound
- KERNAL boot chain translated to Rust reference code

**Next**
- **1541 ↔ C64 serial (IEC) link** — connect the working drive to the C64 so
  `LOAD"*",8,1` reads a real disk over the emulated serial bus.

**Later**
- VIC-II border rendering, sprite collisions, cycle-exact timing
- SID analog filter
- `no_std` bring-up on a real MCU
- A debugger UI (monitor: registers, disassembly, breakpoints)

See [`docs/kernal-boot.md`](docs/kernal-boot.md) for a full narrated walk-through
of the boot process and a more detailed roadmap.

## Notes on ROMs

The C64 ROMs (KERNAL/BASIC/CHARGEN) are Commodore's copyright and are **not**
committed — fetch them as shown above. The 1541 drive ROM (`roms/C1541.rom`) is
embedded at build time by the `c1541` crate.

## Status

Early but real: the machine boots and runs software. It is a learning project
under active development, not a finished product. Contributions and questions in
the spirit of "how does this actually work?" are very welcome.
