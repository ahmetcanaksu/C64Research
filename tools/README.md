# Research helper tools

Small, ad-hoc scripts used while reverse-engineering the ROMs. They complement
the Rust crates — quick to hack on, not part of the emulator build.

| Tool | What it does |
|------|--------------|
| `scan_pointer_tables.py` | Finds runs of little-endian words that point into ROM — i.e. jump/dispatch tables the tracer can't follow on its own. |

## `scan_pointer_tables.py`

Find candidate jump tables, then feed the interesting ones to the disassembler:

```sh
# 1. locate tables
python tools/scan_pointer_tables.py roms/C1541.rom --org 0xC000 --min 6

# 2. disassemble the routines a table dispatches to
cargo run -p disasm -- roms/C1541.rom --table 0xE780:17
```

Requires only Python 3 (standard library).

## Fetching the CPU test vectors

The Klaus Dormann 6502 functional test is GPL and not vendored. Fetch it once so
`cargo test -p mos6502 --test klaus` runs (it skips if absent):

```sh
curl -L -o test-data/6502_functional_test.bin \
  https://github.com/Klaus2m5/6502_65C02_functional_tests/raw/master/bin_files/6502_functional_test.bin
```
