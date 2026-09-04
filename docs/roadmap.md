# Roadmap

Where this project is, what comes next, and enough detail on each item to pick it
up cold. For the narrated walk-through of the boot process see
[`kernal-boot.md`](kernal-boot.md).

The ordering principle: **this repo optimises for understanding, not coverage.**
An item earns its place by teaching something about the machine, or by unblocking
something that does. "Make more software run" is not on its own a reason.

---

## Status at a glance

| | |
|---|---|
| Tests | 127 passing, 0 failing, 1 loud skip (needs a non-redistributable `.prg`) |
| `cargo clippy --all-targets` | clean — keep it that way |
| Boots | real KERNAL + BASIC to `READY.`; real 1541 DOS to its idle loop |
| Loads | `.prg` (side-load), `.d64` over the emulated serial bus, 8K/16K cartridges |

---

## Done

- **6502/6510 CPU** — all 151 official opcodes plus the stable undocumented
  ones, BCD, verified against the Klaus Dormann functional test.
- **VIC-II** — text, multicolour, bitmap, multicolour-bitmap, 8 sprites, raster
  interrupts, per-scanline rendering, and sprite collisions (`$D01E`/`$D01F`).
- **SID** — 3 voices, ADSR, master volume; played through `cpal`.
- **CIA ×2** — ports, Timer A/B, ICR interrupt logic, keyboard matrix.
- **VIA ×2** — ports, T1/T2, IFR/IER.
- **PLA banking** — LORAM/HIRAM/CHAREN, write-hits-RAM/read-hits-ROM, and the
  cartridge port's EXROM/GAME layouts.
- **Serial (IEC) bus** — three open-collector lines resolved wired-OR, CIA2
  wired in, and a device that speaks the real protocol including EOI.
- **Disk** — `.d64` reading, the `$` directory synthesised as a BASIC program,
  and the command/error channel (secondary address 15) with real DOS status.
- **Cartridges** — 8K/16K with `CBM80` autostart.
- **KERNAL as Rust** — the boot chain, the 60 Hz IRQ, `SCNKEY`, and the BASIC
  cold-start seam, each beside its original 6502 listing.
- **Tooling** — tracing disassembler, `crates/harness` for headless tests, and a
  `--headless` script mode for the emulator.

---

## Next

### 1. Put the real 1541 firmware on the bus

**The headline item.** `crates/c1541` already boots the genuine DOS ROM into its
idle loop, and `crates/iec` already carries bytes between a C64 and a device. The
missing piece is everything in between: the drive has no way to *reach* the bus
and no disk to read.

Doing this replaces a simulation with the real thing, and the payoff is
conceptual — `LOAD"*",8,1` would be executed by two 6502s talking to each other
over three wires, with no host-side shortcut anywhere in the path.

There are three separable pieces. Do them in this order; each is independently
verifiable.

#### 1a. VIA1 → the serial bus (the drive's side of the handshake)

Wire `c1541`'s VIA1 (`$1800`) to an `iec::Bus`, mirroring what
`crates/c64/src/lib.rs` does for CIA2.

Port B bit assignments (`$1800`):

| Bit | Direction | Meaning |
|-----|-----------|---------|
| 0 | in | DATA |
| 1 | out | DATA (inverted: 1 = pull low) |
| 2 | in | CLK |
| 3 | out | CLK (inverted) |
| 4 | out | **ATNA** — attention acknowledge |
| 5-6 | in | device-address jumpers (8, 9, 10, 11) |
| 7 | in | ATN |

`CA1` is wired to ATN and raises a VIA interrupt on its edge, which is how the
DOS notices it is being addressed.

**The part that will bite you: the ATN acknowledge is hardware, not software.**
The 1541 has a gate on the board that drives DATA low from
`ATNA XOR (not ATN)` — so a drive answers attention within nanoseconds, before
its CPU has executed a single instruction. `crates/iec`'s virtual device fakes
this by pulling DATA on the ATN edge (see `Device::attention`), and the real
drive must reproduce it in the VIA→bus adapter rather than waiting for the DOS.
Model it as: DATA is pulled low when `ATNA != atn_level`, combined (wired-OR)
with the DOS's own DATA-out bit.

Also honour the address jumpers, so a second drive can be device 9 — the bus
already supports four slots (`iec::MAX_DEVICES`) and nothing has exercised more
than one.

*Verify:* boot the drive with a C64 on the same bus and assert the drive's DOS
takes its ATN interrupt and pulls DATA low when the C64 sends `LISTEN 8`. Compare
against `iec::Device` doing the same thing — that is what it is there for.

#### 1b. VIA2 → the disk controller

VIA2 (`$1C00`) is the read/write channel. Port B is the mechanism, port A is the
data byte:

| Bit (PB) | Meaning |
|-----|---------|
| 0-1 | stepper motor phase (step in/out by rotating the two bits) |
| 2 | drive motor on |
| 3 | LED |
| 4 | write-protect sense (in) |
| 5-6 | density / bit rate zone |
| 7 | SYNC detected (in) |

Port A is the byte under the head; `CA1` signals **byte-ready**. The DOS spins
waiting for SYNC, then reads bytes.

This is the biggest single chunk of work in the project. It needs a rotating-disk
model: a head position, a current track, and a byte stream that advances with
time.

#### 1c. GCR encoding, so there is something to read

A `.d64` stores decoded 256-byte sectors. A real drive reads **GCR**: a 4-to-5
bit encoding chosen so no run of more than two zero bits appears, which is what
lets the drive recover its clock from the data. A track is a ring of sync marks,
sector headers and data blocks.

So `crates/d64` (or a new `crates/gcr`) needs to *synthesise* a GCR track image
from the sectors, and eventually decode the other way for writing:

- 4 bits → 5 bits via the standard GCR table
- 256 data bytes + checksum → 325 GCR bytes
- sector header: sync (≥5 × `$FF`), `$08`, checksum, sector, track, ID2, ID1
- data block: sync, `$07`, 256 bytes, checksum
- gaps between blocks
- 4 speed zones (tracks 1-17, 18-24, 25-30, 31-35) with different bit rates

*Verify:* the drive's DOS reads track 18 sector 1 through its own job queue and
the directory appears in drive RAM. Then the real end-to-end test: `LOAD"$",8`
with the virtual device *unplugged*, served entirely by the firmware.

**Effort:** large. 1a is a day; 1b and 1c together are the substantial part.

---

### 2. `SAVE`, and writing a `.d64` back

Everything is read-only today. `iec::Storage::write` exists and
`d64::DiskDrive` drops the bytes.

Needed: allocate sectors from the BAM, follow and extend a sector chain, write a
directory entry, update the BAM's free counts, and support the DOS commands that
currently answer `26,WRITE PROTECT ON` (`N:` format, `S:` scratch, `R:` rename,
`C:` copy).

*Verify:* `SAVE"X",8` then `LOAD"X",8` round-trips through the bus; and the
resulting image still parses with `d64::Disk::dir()`. A blank-disk `N:` followed
by a save and a `$` listing showing `664 BLOCKS FREE` minus the file is a good
end-to-end check.

**Effort:** medium. Self-contained and well-specified.

---

### 3. `.crt` cartridge images

Only raw 8K/16K binaries load today. The `.crt` container has a 64-byte header
(`C64 CARTRIDGE   `), a hardware-type field, and `CHIP` packets each with a load
address and bank number.

Worth doing mostly for the **bank-switching** types — Ocean, Magic Desk, System 3
and friends switch banks by writing to `$DE00`/`$DF00`. That means modelling the
I/O1/I/O2 windows, which currently return 0 (`Board::io_read`'s `_ => 0` arm).
Bank switching is where cartridges stop being "extra ROM" and become interesting.

**Effort:** small for plain 8K/16K; medium once bank switching is in.

---

## Later

### VIC-II: border, and cycle-exact timing

- **Border.** The renderer draws only the 320×200 display area, so
  `border_rgb()` exists but nothing shows it. Real programs open the border and
  draw in it. This changes the framebuffer dimensions to 403×284 (PAL), which
  touches the emulator, the screenshot example and every render test — do it as
  one deliberate change, not incrementally.
- **Bad lines and cycle-exact timing.** Every 8th raster line the VIC steals
  40-43 cycles from the CPU to fetch character data. Programs time raster
  effects against this. Modelling it means the VIC has to be able to stall the
  CPU, which is a real change to the `step()` contract in both machines.
- **Sprite/background priority** (`$D01B`) — currently sprites always draw in
  front. Note collisions are already independent of priority, which is correct.

### SID: the analog filter

`$D415`-`$D417` are parsed and ignored; voices pass through unfiltered. A
12 dB/octave state-variable filter with low/band/high-pass modes gets most of the
character. Hard to unit-test meaningfully — the honest test is a spectrum check
on a swept input, asserting attenuation above/below cutoff rather than exact
sample values.

Also unimplemented: ring modulation and oscillator sync (control bits 1 and 2 are
decoded but do nothing), and the combined waveforms.

### CIA: time-of-day clock, and the shift registers

The TOD clock (`$DC08`-`$DC0B`) is a stub that stores what you write. Real TOD is
BCD, latches on reading the hours register and unlatches on tenths — a classic
source of hangs in programs that read it in the wrong order. Small, well-defined,
genuinely testable.

The CIA and VIA shift registers are both stubbed. The CIA's matters for the
user-port; the VIA's for fast loaders.

### `no_std` bring-up on real hardware

The end goal. Every chip crate is already `no_std` and allocation-free, and
`crates/c1541` embeds its ROM with `include_bytes!` so it needs no filesystem.

What is not yet done: no target actually builds for a microcontroller. First step
is a CI job that runs `cargo build --target thumbv6m-none-eabi -p mos6502 -p vic2
-p sid -p cia6526 -p via6522 -p iec` to *prove* the `no_std` claim rather than
assuming it. `crates/c64` holds a 64 KB RAM array plus three ROM arrays as
`struct` fields — about 84 KB — which will not fit an RP2040's 264 KB alongside a
framebuffer without thought.

### A debugger / monitor

Registers, disassembly at the PC, memory dump, breakpoints, single-step. The
disassembler already exists (`apps/disasm`) and `Board::peek` reads through the
real banking, so the pieces are there. The natural home is the emulator, or a
`--monitor` mode on the headless runner.

---

## Known rough edges

Small, unglamorous, and each one would save someone an hour:

- **`rustfmt` is not clean** — 28 files differ from default formatting, most
  never touched recently. Do **not** run `cargo fmt` casually; it would produce a
  115-hunk diff across the repo unrelated to whatever you were doing. Either
  leave it, or land a `rustfmt.toml` + whole-repo reformat as its own commit.
- **The 1541's `ram_test.rs`** is a hand-port of the drive's RAM test that runs
  standalone; it is not wired into the boot path it mirrors.
- **`iec::Device` timing constants are floors, not measurements.** They are
  chosen to outlast the KERNAL's polling loops, not to match a 1541. Fast loaders
  bit-bang their own protocol and will not work against them.
- **No tape.** `PRESS PLAY ON TAPE` is reachable (device 1) with nothing behind
  it. A datasette is a small, self-contained project.
- **`prg/64quarx-shareware.prg`** is not redistributable, so
  `loads_and_runs_a_real_game` skips. It is the only test that exercises a real
  commercial program's use of undocumented opcodes.

---

## How to work on this

Read [`../CLAUDE.md`](../CLAUDE.md) first — it has the build/test commands, the
conventions, and a list of hardware gotchas that have already cost time once.

The short version: write the test through `crates/harness` so it drives the real
ROMs and reads the screen, because the bugs in this project live in the seams
between chips rather than inside them.
