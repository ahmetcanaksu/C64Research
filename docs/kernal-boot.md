# The C64 KERNAL Boot Process — annotated, and project roadmap

This document walks through exactly what a Commodore 64 does from the moment it
powers on until the `READY.` prompt, then lays out where this project is and
where it's going. Every routine described here has a readable, tested Rust
translation in `crates/kernal/` — the addresses in headings link the prose to
that code and to the disassembly you can regenerate with:

```sh
cargo run -p disasm -- roms/kernal-901227-03.bin --org 0xE000 \
  --code-table 0xFF81:36 --table 0xFD30:16 > kernal.asm
```

---

## 1. The big picture

When the 6510 is reset it loads the **reset vector** at `$FFFC/$FFFD`, which
points at **`$FCE2`** — the KERNAL's "main". That routine is astonishingly
short. In pseudocode:

```
RESET():
    set up stack, disable interrupts, clear decimal mode
    if cartridge_present():        # "CBM80" signature at $8004
        jmp (cartridge cold-start vector at $8000)
    ioinit()                       # bring up CIA / SID / VIC / 6510 port
    ramtas()                       # test, clear, and size RAM
    restor()                       # install the RAM I/O vectors
    cint()                         # init VIC-II + screen editor, clear screen
    enable interrupts
    jmp (BASIC cold-start vector at $A000)   # -> BASIC takes over
```

That's the whole thing. Everything below is just *what each of those five calls
does*.

```
              reset vector $FFFC
                     │
                     ▼
                 RESET $FCE2
                     │
        ┌────────────┴───────────────┐
   cartridge?  yes ──► JMP ($8000)   no
                     │
   ioinit $FDA3 ─► ramtas $FD50 ─► restor $FD15 ─► cint $FF5B/$E518
                     │
                 CLI; JMP ($A000)
                     │
                     ▼
               BASIC cold start $E394  →  "READY."
```

---

## 2. Cartridge detection — `$FD02`

Before doing anything else the KERNAL checks whether a cartridge wants to take
over the machine. Cartridges map into `$8000-$9FFF`, and an autostart cartridge
places a five-byte signature at **`$8004`**:

| bytes | `C3 C2 CD 38 30` |
|-------|------------------|
| meaning | `'C'|$80, 'B'|$80, 'M'|$80, '8', '0'` — i.e. **"CBM80"** |

The routine compares those five bytes against a copy stored in the KERNAL ROM at
`$FD10`. The trick is in how it reports the result: if every byte matches, the
loop falls out with the **Z flag set**; on the first mismatch it branches out
with **Z clear**. Back in RESET, `BNE` skips the cartridge path when Z is
clear (no match), so:

- **match** → `JMP ($8000)`: the cartridge's own cold-start address (the word at
  `$8000`) runs, and the KERNAL boot never continues.
- **no match** → normal boot proceeds.

> Rust: `reset::cartridge_present`, `reset::CBM80`.

---

## 3. IOINIT — initialize the I/O chips — `$FDA3`

Brings all the I/O hardware to a known state and starts the heartbeat interrupt:

- **Both CIAs** (`$DC00`, `$DD00`): clear all interrupt enables, stop the
  timers, set the ports to a sane direction. CIA1 port A is made all-outputs
  because it drives the keyboard matrix columns.
- **SID** (`$D418`): volume to 0.
- **VIC bank** (CIA2 port A `$DD00`): `$07` selects VIC bank 0 (`$0000-$3FFF`)
  and sets the serial-bus lines.
- **6510 processor port** (`$00`/`$01`): direction `$2F`, data `$E7` — this is
  the banking latch that makes BASIC ROM, KERNAL ROM, and I/O all visible.
- **CIA1 Timer A** (`$DC04/$DC05`): loaded with the PAL or NTSC period (chosen
  from the `$02A6` flag) and started in continuous mode with its interrupt
  enabled. This is the **~60 Hz IRQ** that from now on drives the jiffy clock,
  the cursor, and the keyboard scan (see §9).

> Rust: `reset::ioinit`.

---

## 4. RAMTAS — test, clear, and size RAM — `$FD50`

The C64's power-on memory routine:

1. **Clear** the low pages `$0002-$0101`, `$0200-$02FF`, `$0300-$03FF` (the OS
   work areas — but not the 6510 port at `$0000/$0001`).
2. Set the cassette-buffer pointer to `$033C`.
3. **Size RAM**: starting at `$0400`, write `$55` to each byte and read it back,
   then `$AA` and read it back, restoring the original byte afterwards. The first
   location where the pattern *doesn't* read back is the top of RAM.

Point 3 relies on a subtle hardware fact this project models faithfully: **a
write always lands in RAM, but a read in a banked ROM region returns the ROM.**
At this point BASIC ROM is banked in at `$A000`, so the write to `$A000` goes
into the RAM underneath while the read returns a BASIC ROM byte — the mismatch
that marks `$A000` as the top of RAM. RAMTAS records that in `MEMSIZ`
(`$0283/$0284`), and sets the start-of-BASIC page (`$0800`) and the screen page
(`$0400`).

> Rust: `ramtas::ramtas`; the write-hits-RAM / read-hits-ROM rule lives in
> `C64Mem`.

---

## 5. RESTOR / VECTOR — the indirection layer — `$FD15`

The C64 routes its interrupt handlers and I/O calls through a **table of 16
pointers in RAM at `$0314-$0333`** (IRQ, BRK, NMI, OPEN, CLOSE, CHKIN, CHKOUT,
CLRCHN, BASIN, BSOUT, STOP, GETIN, CLALL, USRCMD, LOAD, SAVE). RESTOR copies the
KERNAL's default targets into that table; VECTOR is the shared routine that can
copy the table either direction (install your own, or read the current ones out).

This is *why* a naive trace of the KERNAL only reaches a fraction of it — most
routines are entered indirectly through these RAM slots, so a disassembler has to
be told about the defaults (`--table 0xFD30:16`) to follow them. It's also the
hook programs use to take over the IRQ or redirect character output.

The first default vector is `$EA31` — the IRQ handler in §9.

> Rust: `vectors::restor`, `vectors::vector`, `reset::DEFAULT_VECTORS`.

---

## 6. CINT — initialize the screen — `$E518`

Brings up the VIC-II and the screen editor:

1. **`vic_init_from_table` (`$E5A0`)** copies 47 bytes from a ROM table at
   `$ECB9` straight into the VIC-II registers `$D000-$D02E`. That single copy
   sets the screen on, 25 rows / 40 columns, screen memory at `$0400`, character
   data at `$1000`, the **border to light blue (`$0E`)** and the **background to
   blue (`$06`)** — the classic C64 look.
2. Set the editor's variables: text colour (light blue), the keyboard-decode
   vector (`$EB48`), keyboard-buffer size (10), cursor blink timing.
3. Build the **screen line-link table** at `$D9-$F2`: the high byte of each of
   the 25 screen lines' start address (`$0400`, `$0428`, …, `+$28` per line),
   with bit 7 marking a real line start. The editor uses this to know where each
   row lives and which rows are wrapped continuations.
4. **Clear the screen**: fill the 1000 screen cells with the space code (`$20`)
   and the 1000 colour cells (`$D800`) with the current colour, and home the
   cursor.

> Rust: `reset::cint`, `reset::vic_init_from_table`, `reset::VIC_INIT_TABLE`.

---

## 7. Hand-off to BASIC — `JMP ($A000)`

With the hardware and screen up, RESET does `CLI` (interrupts on — the 60 Hz IRQ
now fires) and `JMP ($A000)`. `$A000` is inside BASIC ROM and holds BASIC's
**cold-start** vector, `$E394`. From there BASIC prints the `**** COMMODORE 64
BASIC V2 ****` banner and `READY.`, then enters its input loop. The KERNAL's job
is done; it now only runs as a library of routines BASIC calls, plus the IRQ.

> Rust: `reset::reset` returns `$E394`, the hand-off target. The `$E394` seam
> itself — init BASIC vectors/RAM, print the banner and `38911 BASIC BYTES FREE`,
> then jump to the READY/main entry — is ported in `basic::cold_start`. It stops
> at the KERNAL/BASIC-ROM boundary; the interpreter itself is not reimplemented.

---

## 8. After boot: the 60 Hz IRQ — `$EA31`

Once interrupts are enabled, CIA1 Timer A fires ~60 times a second and (through
the `$0314` vector) runs the default IRQ handler at **`$EA31`**, which each tick:

1. **`UDTIM` (`$F69B`)** — advance the 24-bit jiffy clock `TIME` (`$A0-$A2`),
   wrapping after 24 hours, and take one debounced sample of the STOP key into
   `STKEY` (`$91`).
2. **Blink the cursor** — every ~20 ticks, toggle the reverse-video bit of the
   character under the cursor (saving/restoring the real character and colour).
3. **Cassette motor** — update the tape motor line from the 6510 port.
4. **`SCNKEY` (`$EA87`)** — scan the keyboard.
5. Restore the saved registers and `RTI`.

**SCNKEY** is the interesting one. It drives CIA1 port A one row at a time
(a walking zero: `$FE`, `$FD`, `$FB`, …), reads the eight column bits back on
port B, and builds a 0-63 **key index** for whichever key is held. Keys whose
unshifted code is a modifier (SHIFT = `$01`, C= = `$02`, CTRL = `$04`) instead
set bits in `$028D`. The modifier bits then select one of **four 65-byte decode
tables** — unshifted (`$EB81`), shifted (`$EBC2`), Commodore (`$EC03`), control
(`$EC78`) — and the key index looks up the final PETSCII code, which is pushed
into the keyboard buffer at `$0277` (SHIFT+C= toggles the character set instead).

> Rust: `irq::irq_handler`, `irq::scnkey`, and the four `irq::*_KEYS` tables.

---

## 9. Memory map quick reference (C64, ROMs+I/O banked in)

```
$0000-$0001  6510 processor port (DDR / data — banking latch)
$0002-$00FF  zero page (KERNAL/BASIC work area)
$0100-$01FF  CPU stack
$0200-$03FF  OS buffers, vectors ($0314-$0333), tables
$0400-$07FF  default screen RAM (1000 cells + sprite pointers)
$0800-$9FFF  BASIC program + variables (RAM)
$A000-$BFFF  BASIC ROM        (reads; writes go to RAM beneath)
$C000-$CFFF  RAM
$D000-$D3FF  VIC-II registers
$D400-$D7FF  SID registers
$D800-$DBFF  colour RAM (nibbles)
$DC00-$DCFF  CIA #1 (keyboard, joystick, IRQ timer)
$DD00-$DDFF  CIA #2 (serial bus, user port, NMI, VIC bank)
$E000-$FFFF  KERNAL ROM       (reads; writes go to RAM beneath)
```

---

## 10. Project roadmap

### Done
- **`mos6502`** — cycle-stepped 6502/6510 CPU, all 151 official opcodes + BCD.
  Verified against the Klaus Dormann functional test.
- **`via6522`** — 6522 VIA (ports, timers, interrupts).
- **`c1541`** — a *bootable* 1541 drive (CPU + RAM + ROM + 2 VIAs) that runs the
  real drive firmware into its DOS idle loop; plus a hand-ported RAM test.
- **`disasm`** — linear + tracing 6502 disassembler with jump-table seeding.
- **`kernal`** — this document's subject: readable Rust translations of the boot
  chain (RESET, cartridge check, IOINIT, RAMTAS, RESTOR/VECTOR, CINT), the 60 Hz
  IRQ (handler + UDTIM + SCNKEY), and the BASIC cold-start seam (`$E394`, up to
  the interpreter boundary), each beside its original 6502 listing.

- **`cia6526`** — the C64's twin-CIA (ports, Timer A/B, ICR interrupt logic;
  TOD/serial stubbed).
- **`vic2`** — VIC-II: registers + raster counter + a standard text-mode
  renderer (screen RAM + colour RAM + character ROM → RGB framebuffer).
- **`c64`** machine — 6510 + PLA banking + RAM + ROMs + 2 CIAs + VIC-II, wired
  like `c1541`. **Boots the real KERNAL + BASIC to `READY.`** (integration test
  `boots_to_ready_prompt`), keyboard matrix included so you can type.
- **`sid`** — MOS 6581 SID: 3 voices (triangle/sawtooth/pulse/noise) with ADSR
  envelopes, mixed and scaled by master volume. Wired into the `c64` machine at
  $D400. (Analog filter not modeled yet.)
- **`apps/emu`** — a [minifb](../README.md) window that blits the VIC-II
  framebuffer, plays the SID through **cpal**, and feeds the host keyboard in via
  a **character-translation layer** (host layout → C64 keys, so symbols like `+`
  work on any layout). **Ctrl+V pastes** clipboard text into the C64. Prints an
  info banner (CPU/video/SID/audio device/keyboard) on start.
  `cargo run -p emu --release`.

### Next
- VIC-II raster interrupts, sprites, bitmap/multicolour modes.
- The SID analog filter ($D415-$D417).

### Later
- Port the C64 chips to `no_std` on a fast MCU (RP2040 / Teensy / ESP32).
- Serial IEC bus between the C64 and the 1541 (load a `.d64`).
- SID audio.
