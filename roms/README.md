# ROMs

| File | Size | SHA-1 | Role |
|------|------|-------|------|
| `C1541.rom` | 16 KB | `ab16f568…` | 1541 disk **drive** firmware (committed) |
| `kernal-901227-03.bin` | 8 KB | `1d503e56…` | C64 KERNAL (rev 3) — **gitignored** |
| `basic-901226-01.bin` | 8 KB | `79015323…` | C64 BASIC V2 — **gitignored** |
| `chargen-901225-01.bin` | 4 KB | `adc7c31e…` | C64 character generator — **gitignored** |

The three C64 ROMs are Commodore's copyright and are **not committed**. Fetch
them (they're widely mirrored; the VICE project's data files are one stable
source):

```sh
base="https://raw.githubusercontent.com/VICE-Team/svn-mirror/main/vice/data/C64"
for f in kernal-901227-03.bin basic-901226-01.bin chargen-901225-01.bin; do
  curl -fsSL -o "roms/$f" "$base/$f"
done
```

Then verify:

```sh
sha1sum roms/kernal-901227-03.bin   # 1d503e56df85a62fee696e7618dc5b4e781df1bb
sha1sum roms/basic-901226-01.bin    # 79015323128650c742a3694c9429aa91f355905e
sha1sum roms/chargen-901225-01.bin  # adc7c31e18c7c7413d54802ef2f4193da14711aa
```

## Disassembling a ROM

```sh
# C64 KERNAL, with its jump table and RAM-vector defaults seeded for coverage:
cargo run -p disasm -- roms/kernal-901227-03.bin --org 0xE000 \
  --code-table 0xFF81:36 --table 0xFD30:16 > kernal.asm
```
