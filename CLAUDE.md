# Working notes for Claude Code

A Commodore 64 built from scratch in Rust, one chip at a time, as a research
project. Read this before changing anything; it will save you rediscovering
things the hard way.

## What this project optimises for

**Understanding, then real hardware. Not coverage or speed.**

VICE already emulates the C64 with world-class accuracy — there is no point
racing it. The goals, in order:

1. **Understand the machine down to the silicon.** Every chip written from
   scratch and documented. The KERNAL isn't just run, it's *translated* into
   readable Rust beside its original 6502 listing (`crates/kernal`).
2. **Be a learning playground.** Clear code over clever code.
3. **Reach real hardware.** The chip crates are `no_std` and allocation-free so
   the same code can eventually run on a microcontroller.

Practical consequences when you write code here:

- A doc comment explaining *why the hardware behaves this way* is worth more than
  the code it sits above. Cite ROM addresses (`$ED40`), register names (`$D01E`),
  datasheet bit meanings.
- Tests should teach. A test named
  `multicolour_bit_pair_01_is_not_foreground` documents a real VIC quirk;
  `test_sprite_2` documents nothing.
- Prefer the faithful mechanism over the shortcut, and when you do take a
  shortcut, say so in the docs and say what the real thing does.
- Don't add abstraction for its own sake. Chips are small structs with
  `read`/`write`/`tick`.

## Commands

```sh
cargo test                     # everything (127 tests)
cargo test -p harness          # the end-to-end tests (needs ROMs)
cargo clippy --all-targets     # MUST stay at zero warnings
C64_REQUIRE_ROMS=1 cargo test   # fail instead of skip if ROMs are absent

cargo run -p emu --release                          # boot to READY.
cargo run -p emu --release -- disk.d64               # drive on the serial bus
cargo run -p emu --release -- --headless --type='print 1+1\r'
cargo run -p d64 --example make-test-disk -- test.d64
```

### `cargo clippy` is a gate

It was failing on a hard error for a long time and nobody noticed. It is at
**zero warnings** now — keep it there.

### Do NOT run `cargo fmt`

28 files differ from default rustfmt, most of them untouched in ages. Running it
produces a 115-hunk diff across the whole repo that will bury your actual change.
Match the surrounding style by hand. If you want the repo formatted, do it as its
own dedicated commit with a `rustfmt.toml`.

## ROMs are not in the repo

The three C64 ROMs are Commodore's copyright. `roms/README.md` has the fetch
command and the SHA-1s. The 1541 drive ROM *is* committed and embedded with
`include_bytes!`.

Tests that need them **skip loudly** via `harness::skip_without_roms!`, and
`C64_REQUIRE_ROMS=1` turns the skip into a failure. This matters: skips count as
passes, and three tests skipped invisibly for a while, so "all green" was
covering less than it claimed. Never add a silently-skipping test.

## Layout

| Crate | What |
|---|---|
| `crates/mos6502` | 6502/6510 CPU — opcode tables + cycle-stepped executor |
| `crates/vic2` | VIC-II — modes, sprites, raster IRQ, collisions, renderer |
| `crates/sid` | 6581 SID — 3 voices + ADSR |
| `crates/cia6526` | 6526 CIA — ports, timers, interrupts |
| `crates/via6522` | 6522 VIA — used by the 1541 (with CA1 for ATN) |
| `crates/iec` | the serial bus + a device that speaks its protocol |
| `crates/d64` | `.d64` reading, and a drive that serves it over the bus |
| `crates/gcr` | GCR track synthesis — decoded sectors → the drive's on-disk bitstream |
| `crates/c64` | the C64: CPU + PLA banking + RAM + ROMs + CIAs + VIC + SID |
| `crates/c1541` | the 1541 drive machine (its own 6502 + 2 VIAs + the disk under the head, `disk.rs`) |
| `crates/kernal` | **the KERNAL translated to Rust** — reference, not used by the emulator |
| `crates/harness` | drive a headless C64 from a test |
| `apps/emu` | windowed emulator + headless script mode |
| `apps/disasm` | 6502 disassembler with control-flow tracing |

Chip crates are `no_std` and allocation-free. `c64` and `c1541` are too — they
take ROMs as byte slices so a `std` host loads the files. `d64` is
`no_std + alloc`. `harness` and the apps are `std`.

`crates/kernal` is **study material**, deliberately not wired into the emulator.
When you find real KERNAL behaviour the emulator depends on, consider whether the
Rust translation should reflect it too (this is how the missing `CLKLO` at the end
of `IOINIT` was found).

## Writing tests

The bugs in this project live in the **seams** between chips, not inside them —
a keystroke that never becomes a matrix scan, a handshake where each side waits
for the other. Those are invisible to a test that only inspects registers. So
drive the real ROMs and read the screen, like a person would:

```rust
use harness::{d64, Harness};

let disk = d64::Disk::new(d64::fixtures::basic_program_image()).unwrap();
let mut h = Harness::with_disk(disk).unwrap();
h.boot();
h.type_text("load\"*\",8,1\rrun\r");
assert!(h.wait_for("HELLO FROM DISK", 3000), "{}", h.screen());
```

Always put `h.screen()` in the failure message. A screen dump tells you what
happened; `assertion failed: false` does not.

**`run_until` samples once per frame (19,700 cycles). Use `run_until_step` for
anything on the serial bus** — an ATN pulse lasts a few dozen cycles and
`run_until` will miss it entirely and report it never happened.

`d64::fixtures` has synthetic disk images so tests never need a real one:
`synthetic_image()` (three dummy bytes, for byte-exact transfer tests) and
`basic_program_image()` (a runnable `10 PRINT"HELLO FROM DISK"`).

## Hardware gotchas that have already cost time

Each of these was a real debugging session. Don't pay twice.

**The serial bus inverts in one direction only.** CIA2's `$DD00` *output* bits
(3/4/5 = ATN/CLK/DATA) are inverted by 7406 buffers — writing `1` pulls the line
**low**. The *input* bits (6/7 = CLK/DATA in) are **not** inverted: `1` means the
line is high. `crates/iec` therefore refuses to use Commodore's "true = pulled
low" language and talks only about `released`/`pulled_low`. Keep it that way.

**An idle C64 holds CLK low.** `IOINIT`'s last instruction is `JMP $EE8E`, a
tail-call into CLKLO. Because it is a `JMP` and not a `JSR` it is easy to read
past. So "idle bus" does *not* mean all three lines high.

**While ATN is asserted, every device answers.** An unaddressed device keeps
acknowledging command bytes and only drops off on the ATN *release* edge — on a
real 1541 that acknowledge is a hardware gate, not software. This is why
`DEVICE NOT PRESENT` for a missing drive surfaces at the first *data* byte, not
at the LISTEN that addressed it.

**EOI is signalled by silence, timed.** A talker says "last byte" by *not*
pulling CLK low for ~256 µs; the listener times it with CIA1 Timer B. There is no
flag bit.

**After the bus turns around, the talker must hold CLK low long enough to be
seen.** The KERNAL waits at `$EDD6` for CLK to go low. Release it too early and
both sides wait forever — see `TURNAROUND_HOLD` in `crates/iec/src/device.rs`.

**Screen codes are not ASCII, and bit 7 is reverse video.** `A` is 1, `@` is 0.
A directory header is printed in reverse so every code in it has bit 7 set — mask
it before comparing, or correct output reads as absent. `harness::screen` does.

**Control characters may never reach a character callback.** minifb's macOS
backend drops every code point below 32 (and `$7F..$A0`) before the callback;
Windows passes them through. So RETURN/Backspace must be handled as *keys*, not
characters. `harness::keyboard::is_printable` filters uniformly so there is one
code path.

**RETURN must be edge-triggered, not held.** A held RETURN fires on every frame
it is down; if characters are still queued it submits the line half-typed. Do
that to `LOAD"*",8,1` and you get a bare `LOAD`, which defaults to device 1 —
the machine asks you to `PRESS PLAY ON TAPE` and never touches the disk. Nothing
about that symptom points at the keyboard.

**Typed keys need a press *and* a release.** The KERNAL debounces on the last key
index in `$00CB`, so a key held with no gap swallows the next character. See
`Typist::HOLD_FRAMES` / `GAP_FRAMES`.

**Writes always land in RAM; reads see banked ROM.** This asymmetry is not a
detail, it is how `RAMTAS` finds the top of RAM. It also means an 8K cartridge
costs you 8K of BASIC RAM (`38911` becomes `30719`) with nobody coding the
subtraction.

**Reading `$D01E`/`$D01F` clears them.** That is the whole interface — the VIC
only latches a fresh collision interrupt once the register is back to zero, so a
game reads it to acknowledge and re-arm. A peek that didn't clear would wedge
collision interrupts permanently.

**In multicolour modes, bit pair `%01` draws a colour but is not foreground.** So
a sprite passes through it with no collision.

**The 1541's BYTE-READY is the CPU's SO pin, not an interrupt.** The drive reads a
GCR byte with `BVC *` / `CLV` / `LDA $1C01` (see `$F53D`): the read head's
byte-ready line is wired to the 6502 SO pin, which sets V asynchronously. So the
disk controller sets `cpu.v = true` when a fresh byte arrives — that, not any IRQ,
is what advances the DOS read loop. See `Controller::tick` in `crates/c1541`.

**BYTE-READY is suppressed over a sync mark.** SYNC (VIA2 PB7, active **low**) is
asserted while the head is over a run of one-bits (`$FF`); during it the byte
counter is held, which is what byte-aligns the first data byte *after* the mark.
Model: PB7 low while over two consecutive `$FF`, and no byte-ready pulse there.
Miss this and the DOS reads sync bytes as data. The DOS finds sync with a
timeout (`$F556` arms VIA1 T1), so *no* sync marks → error 21, not a hang.

**The stepper steps inward on phase decrement.** Rotating VIA2 PB0-1 down one
(`(phase-1)&3`) moves the head toward higher track numbers (disk centre), up one
toward the rim. Get it backwards and a seek from track 1 to 18 just bumps the rim
stop forever. A raw `$80` READ job does **not** seek — it reads whatever track the
head is on; the DOS's file layer seeks first through a separate path.

**A sector header stores its two ID bytes reversed from the BAM.** `gcr` writes
them `[id[1], id[0]]`; the DOS reads them back into `$16/$17` in disk order
`[id[0], id[1]]` and checks against the master ID at `$12/$13` (per drive, indexed
by `$3E`). A raw job needs that master ID pre-loaded — a real access initialises
it from the BAM (`$F410` copies `$16/$17` → `$12/$13`). Mismatch is error `$0B`.

**BASIC relinks `$0801`/`$0802` when it returns to `READY.`** If you assert on
bytes a `,1` load put there, watch for them *as they arrive* (`run_until_step`),
because they are overwritten shortly after.

## When you find real hardware behaviour

Verify it against the ROM rather than trusting memory — that is what the
disassembler is for:

```sh
cargo run -p disasm -- roms/kernal-901227-03.bin --org 0xE000 \
  --code-table 0xFF81:36 --table 0xFD30:16 > kernal.asm
```

Then write it down: in the code's doc comment, in `docs/kernal-boot.md` if it is
part of the boot, and here if it is a trap someone else would fall into.

## Git

Branch before committing; `bootstrap-emulator` is the default branch. Commit only
when asked. `*.d64`, `*.crt`, `roms/*.bin`, `test-data/` and `prg/` are ignored —
don't commit disk images or ROMs.

## Roadmap

[`docs/roadmap.md`](docs/roadmap.md) — detailed, with enough context on each item
to pick it up cold. The next big one is putting the real 1541 firmware on the
serial bus (VIA1 wiring, the VIA2 disk controller, and GCR track synthesis).
