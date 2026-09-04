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
- **Runs real software.** Loads `.prg` and `.d64` images. There's a test that
  boots and renders a real shareware game (Quarx) — it exercises the
  undocumented opcodes and VIC behaviour that synthetic fixtures never touch, and
  it skips unless you drop the (non-redistributable) `.prg` into `prg/`.
- **VIC-II video** — text, multicolor, bitmap and multicolor-bitmap modes, 8
  sprites, and **raster interrupts**, rendered per-scanline.
- **SID sound** — 3 voices with ADSR envelopes, played through `cpal`.
- **A working 1541 disk drive** — the real drive firmware boots to its DOS idle
  loop (its own 6502 + two VIA chips).
- **The serial (IEC) bus** — three open-collector lines shared wired-OR, with
  CIA2 wired in and a device that speaks the real protocol. `LOAD"$",8` and
  `LOAD"*",8,1` work through the **genuine KERNAL**, a bit at a time, EOI and
  all — no load shortcuts. The drive's **command/error channel** works too, so
  `OPEN 15,8,15` reports real DOS status (`62,FILE NOT FOUND,00,00`).
- **Sprite collisions** — `$D01E`/`$D01F` with the read-to-clear behaviour and
  the collision interrupt.
- **The cartridge port** — 8 KB and 16 KB layouts with EXROM/GAME banking, and
  `CBM80` autostart: a cartridge takes the machine over at reset and BASIC never
  runs. A 16 KB one displaces BASIC ROM outright.
- **A test harness** ([`crates/harness`](crates/harness)) — boot a real machine
  headlessly, type at it, wait for text, read the screen back.
- **A 6502 disassembler** with control-flow tracing and jump-table seeding.
- **A friendly emulator** ([`apps/emu`](apps/emu)) — minifb window, host-layout
  keyboard translation, clipboard paste, RUN/STOP, and a **headless script mode**
  for driving it from a terminal or CI.

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
cargo test                                   # run everything (127 tests)
C64_REQUIRE_ROMS=1 cargo test                # ...and fail rather than skip if ROMs are missing
cargo run -p emu --release                   # boot to the READY. prompt
cargo run -p emu --release -- game.prg       # boot + load + RUN a program
cargo run -p emu --release -- disk.d64       # attach a drive, LOAD over the bus
cargo run -p emu --release -- disk.d64 NAME  # ...and ask for a specific file
cargo run -p emu --release -- disk.d64 --sideload   # skip the bus, inject the bytes
cargo run -p emu --release -- cart.bin       # plug in a cartridge and reset
```

Given a `.d64`, the emulator now hangs a **drive off the serial bus** and lets the
KERNAL fetch the file itself — which is authentically slow, because a 1541 is. Pass
`--sideload` for the old shortcut that copies the bytes straight into RAM.

Emulator keys: type normally (layout-aware), **Return**, arrows/Home/Del edit,
**Ctrl+V** or **Cmd+V** paste, **PageUp** = RESTORE, **Ctrl+C** = RUN/STOP,
**Esc** quits.

Disassemble a ROM:

```sh
cargo run -p disasm -- roms/kernal-901227-03.bin --org 0xE000 \
  --code-table 0xFF81:36 --table 0xFD30:16 > kernal.asm
```

Make a bootable test disk (the ROMs and real game disks can't be committed, so
this builds one from scratch — a formatted image holding a small BASIC program):

```sh
cargo run -p d64 --example make-test-disk -- test.d64
cargo run -p emu --release -- test.d64      # LOAD over the bus, then type RUN
```

Headless script mode — no window, no audio: type something, run, print the
screen as text. Useful for checking the keyboard and loading paths from a
terminal or CI:

```sh
cargo run -p emu --release -- --headless --type='print 1+1\r'
cargo run -p emu --release -- disk.d64 --headless --frames=900
```

Headless screenshot:

```sh
cargo run -p emu --example screenshot -- game.prg 120 shot.ppm
```

## Repository map

| Crate | What it is |
|-------|-----------|
| [`crates/mos6502`](crates/mos6502) | 6502/6510 CPU — opcode tables + cycle-stepped executor (`no_std`) |
| [`crates/vic2`](crates/vic2) | VIC-II video — modes, sprites, raster IRQ, collisions, per-scanline renderer |
| [`crates/sid`](crates/sid) | 6581 SID sound — 3 voices + ADSR |
| [`crates/cia6526`](crates/cia6526) | 6526 CIA — ports, timers, interrupts (keyboard/IRQ) |
| [`crates/via6522`](crates/via6522) | 6522 VIA — used by the 1541 drive |
| [`crates/c64`](crates/c64) | the C64 machine: 6510 + PLA banking + RAM + ROMs + 2 CIAs + VIC + SID |
| [`crates/c1541`](crates/c1541) | the 1541 drive machine + hand-ported DOS routines |
| [`crates/kernal`](crates/kernal) | **the KERNAL translated to readable Rust** — study material |
| [`crates/iec`](crates/iec) | the serial (IEC) bus + a device that speaks its protocol (`no_std`) |
| [`crates/harness`](crates/harness) | drive a headless C64 from a test: boot, type, read the screen |
| [`crates/d64`](crates/d64) | `.d64` disk-image reader, and a drive that serves it over the bus |
| [`apps/emu`](apps/emu) | windowed emulator (video + sound + keyboard) |
| [`apps/disasm`](apps/disasm) | 6502 disassembler |
| [`tools/`](tools) | small research helpers (jump-table scanner, etc.) |

## Roadmap

**Done** — 6510 CPU (official + illegal opcodes, Klaus-verified) · tracing
disassembler · VIC-II (all display modes, sprites, raster interrupts, sprite
collisions) · SID · two CIAs · two VIAs · PLA banking · a C64 that boots the real
KERNAL + BASIC to `READY.` · a 1541 that boots its own DOS · the serial (IEC) bus
with `LOAD` from a `.d64` · the drive's command/error channel · the cartridge
port with `CBM80` autostart · the KERNAL boot chain translated to Rust · a
headless test harness.

**Next** — put the **real 1541 firmware** on that bus. The drive already boots
its DOS; what it lacks is a read channel: VIA1 for the serial lines (where the
ATN acknowledge is *hardware*, not software), the VIA2 disk controller, and GCR
track synthesis from a `.d64`. Then `SAVE`, and `.crt` cartridges with bank
switching.

**Later** — VIC-II border and cycle-exact (bad-line) timing · the SID analog
filter · CIA time-of-day · `no_std` bring-up on a real MCU · a debugger/monitor.

See **[`docs/roadmap.md`](docs/roadmap.md)** for the detailed version — each item
with enough context to pick up cold, plus the known rough edges. And
[`docs/kernal-boot.md`](docs/kernal-boot.md) for a narrated walk-through of the
boot process.

## Notes on ROMs

The C64 ROMs (KERNAL/BASIC/CHARGEN) are Commodore's copyright and are **not**
committed — fetch them as shown above. The 1541 drive ROM (`roms/C1541.rom`) is
embedded at build time by the `c1541` crate.

## Working on it

[`CLAUDE.md`](CLAUDE.md) has the build and test commands, the conventions, and —
most usefully — a list of **hardware gotchas that have already cost a debugging
session each**: which direction the serial bus inverts, why an idle C64 holds CLK
low, why bit 7 of a screen code isn't part of the character, why RETURN has to be
edge-triggered. Read it before changing anything.

Two house rules worth knowing up front:

- `cargo clippy --all-targets` is a gate and sits at **zero warnings**.
- **Don't run `cargo fmt`** — 28 files differ from default rustfmt, so it would
  bury your change in a 115-hunk repo-wide diff. Match the surrounding style.

Tests live in [`crates/harness`](crates/harness) and drive the real ROMs
headlessly — boot the machine, type at it, read the screen back. The interesting
bugs here are in the *seams* between chips, and those are only visible from
outside the whole machine.

## Status

Early but real: the machine boots, loads from disk over an emulated serial bus,
and runs software. **127 tests**, `clippy` clean. It is a learning project under
active development, not a finished product. Contributions and questions in the
spirit of "how does this actually work?" are very welcome.
