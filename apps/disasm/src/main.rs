//! 6502 disassembler for Commodore ROM images.
//!
//! Two modes:
//!   * **trace** (default) — recursive-traversal disassembly. Starts from the
//!     reset/IRQ/NMI vectors (plus any `--entry` you give) and follows control
//!     flow, so code and data separate cleanly and jump/call targets get labels.
//!   * **linear** (`--linear`) — decode every byte start-to-finish. Simple, but
//!     embedded data tables decode as garbage.
//!
//! Usage:
//!   disasm [ROM_PATH] [--linear] [--org 0xC000] [--entry 0xXXXX ...]
//!
//! Defaults to `roms/C1541.rom` loaded at `$C000` (the 1541 drive ROM occupies
//! the top 16 KB of the drive CPU's 64 KB address space).

mod trace;

use std::process::ExitCode;

use mos6502::{decode, AddrMode};

/// Which disassembly strategy to run.
enum Mode {
    Trace,
    Linear,
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut path = String::from("roms/C1541.rom");
    let mut org: u16 = 0xC000;
    let mut mode = Mode::Trace;
    let mut extra_entries: Vec<u16> = Vec::new();
    let mut tables: Vec<(u16, u16)> = Vec::new();
    let mut code_tables: Vec<(u16, u16, u16)> = Vec::new();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--linear" => mode = Mode::Linear,
            "--trace" => mode = Mode::Trace,
            "--org" => match args.next().and_then(|v| parse_u16(&v)) {
                Some(v) => org = v,
                None => return usage_err("--org expects a value like 0xC000"),
            },
            "--entry" => match args.next().and_then(|v| parse_u16(&v)) {
                Some(v) => extra_entries.push(v),
                None => return usage_err("--entry expects a value like 0xEAA0"),
            },
            "--table" => match args.next().and_then(|v| parse_table(&v)) {
                Some(t) => tables.push(t),
                None => return usage_err("--table expects ADDR:COUNT, e.g. 0xFF54:16"),
            },
            "--code-table" => match args.next().and_then(|v| parse_code_table(&v)) {
                Some(t) => code_tables.push(t),
                None => return usage_err("--code-table expects ADDR:COUNT[:STRIDE], e.g. 0xFF81:36"),
            },
            "-h" | "--help" => {
                eprintln!("usage: disasm [ROM_PATH] [--linear] [--org 0xC000] [--entry 0xXXXX ...] [--table ADDR:COUNT ...]");
                return ExitCode::SUCCESS;
            }
            other => path = other.to_string(),
        }
    }

    let rom = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) => return usage_err(&format!("cannot read {path}: {e}")),
    };

    if rom.len() > 0x1_0000 || (org as usize) + rom.len() > 0x1_0000 {
        return usage_err(&format!(
            "ROM of {} bytes at ${:04X} does not fit in the 64 KB address space",
            rom.len(),
            org
        ));
    }

    println!("; {} ({} bytes) loaded at ${:04X}", path, rom.len(), org);
    match mode {
        Mode::Linear => {
            println!("; linear-sweep disassembly — data tables may decode as garbage\n");
            println!("        * = ${:04X}\n", org);
            disassemble_linear(&rom, org);
            print_vectors(&rom, org);
        }
        Mode::Trace => {
            trace::disassemble_trace(&rom, org, &extra_entries, &tables, &code_tables);
        }
    }

    ExitCode::SUCCESS
}

/// Walk the ROM linearly, decoding one instruction at a time.
fn disassemble_linear(rom: &[u8], org: u16) {
    let mut i: usize = 0;
    while i < rom.len() {
        let addr = org.wrapping_add(i as u16);
        let opcode = rom[i];
        let op = decode(opcode);
        let len = op.len() as usize;

        let avail = (rom.len() - i).min(len);
        let bytes = &rom[i..i + avail];
        let hex = hex_bytes(bytes);

        // Only truly-unknown opcodes (JAM/KIL, mode Ill) render as data; known
        // undocumented opcodes have a real mode and decode like any instruction.
        if op.mode == AddrMode::Ill || avail < len {
            println!("{addr:04X}:  {hex:<8}  .byte ${opcode:02X}");
            i += 1;
            continue;
        }

        let operand = format_operand(op.mode, addr, bytes);
        println!("{addr:04X}:  {hex:<8}  {:<4} {operand}", op.mnemonic);
        i += len;
    }
}

/// Format the operand text for a decoded instruction. `bytes` holds the full
/// instruction (opcode + operand) and `addr` is the instruction's address.
pub(crate) fn format_operand(mode: AddrMode, addr: u16, bytes: &[u8]) -> String {
    use AddrMode::*;
    let lo = bytes.get(1).copied().unwrap_or(0);
    let hi = bytes.get(2).copied().unwrap_or(0);
    let word = u16::from_le_bytes([lo, hi]);

    match mode {
        Imp | Ill => String::new(),
        Acc => "A".to_string(),
        Imm => format!("#${lo:02X}"),
        Zp => format!("${lo:02X}"),
        Zpx => format!("${lo:02X},X"),
        Zpy => format!("${lo:02X},Y"),
        Abs => format!("${word:04X}"),
        Abx => format!("${word:04X},X"),
        Aby => format!("${word:04X},Y"),
        Ind => format!("(${word:04X})"),
        Izx => format!("(${lo:02X},X)"),
        Izy => format!("(${lo:02X}),Y"),
        Rel => format!("${:04X}", branch_target(addr, lo)),
    }
}

/// Branch target of a relative instruction at `addr` with signed offset `off`.
pub(crate) fn branch_target(addr: u16, off: u8) -> u16 {
    addr.wrapping_add(2).wrapping_add((off as i8) as u16)
}

/// Render a byte slice as space-separated uppercase hex.
pub(crate) fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Read a little-endian word from the image at absolute address `addr`.
pub(crate) fn read_word(rom: &[u8], org: u16, addr: u16) -> Option<u16> {
    let start = addr.checked_sub(org)? as usize;
    let lo = *rom.get(start)?;
    let hi = *rom.get(start + 1)?;
    Some(u16::from_le_bytes([lo, hi]))
}

/// Print the 6502 hardware vectors at the top of the image ($FFFA/$FFFC/$FFFE).
fn print_vectors(rom: &[u8], org: u16) {
    println!("\n; ---- hardware vectors ----");
    for (name, v) in [("NMI", 0xFFFAu16), ("RESET", 0xFFFC), ("IRQ/BRK", 0xFFFE)] {
        match read_word(rom, org, v) {
            Some(target) => println!("; {name:<7} @ ${v:04X} -> ${target:04X}"),
            None => println!("; {name:<7} @ ${v:04X} -> (outside image)"),
        }
    }
}

fn usage_err(msg: &str) -> ExitCode {
    eprintln!("error: {msg}");
    ExitCode::FAILURE
}

/// Parse a `ADDR:COUNT` pointer-table spec (each part decimal or hex).
fn parse_table(s: &str) -> Option<(u16, u16)> {
    let (addr, count) = s.split_once(':')?;
    Some((parse_u16(addr)?, parse_u16(count)?))
}

/// Parse a `ADDR:COUNT[:STRIDE]` code-table spec; STRIDE defaults to 3 (a
/// classic `JMP nnnn` jump table like the KERNAL API vectors).
fn parse_code_table(s: &str) -> Option<(u16, u16, u16)> {
    let mut parts = s.split(':');
    let addr = parse_u16(parts.next()?)?;
    let count = parse_u16(parts.next()?)?;
    let stride = match parts.next() {
        Some(v) => parse_u16(v)?,
        None => 3,
    };
    Some((addr, count, stride))
}

/// Parse a `u16` from decimal, `0x`-prefixed, or `$`-prefixed hex.
fn parse_u16(s: &str) -> Option<u16> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u16::from_str_radix(hex, 16).ok()
    } else if let Some(hex) = s.strip_prefix('$') {
        u16::from_str_radix(hex, 16).ok()
    } else {
        s.parse().ok()
    }
}
