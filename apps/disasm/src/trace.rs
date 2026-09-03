//! Recursive-traversal ("tracing") disassembler.
//!
//! Starts from the hardware vectors (and any user-supplied `--entry` points),
//! follows every reachable control-flow edge, and marks which bytes are code.
//! Anything never reached is emitted as data. Jump/call/branch targets that land
//! inside the image become `Lxxxx` labels.
//!
//! Limitation: purely static tracing cannot follow computed jumps (`JMP ($nnnn)`)
//! or data-driven dispatch tables — a lot of the 1541 DOS is reached that way.
//! Use `--entry 0xADDR` to seed routines the trace can't discover on its own.

use std::collections::BTreeSet;

use mos6502::{decode, AddrMode};

use crate::{branch_target, format_operand, hex_bytes, read_word};

/// True if `addr` lies within the loaded image.
fn in_rom(rom: &[u8], org: u16, addr: u16) -> bool {
    matches!(addr.checked_sub(org), Some(off) if (off as usize) < rom.len())
}

/// Read a little-endian word from the image, reproducing the 6502 `JMP (ind)`
/// page-wrap bug: the high byte is fetched from the same page as the pointer.
fn read_word_bug(rom: &[u8], org: u16, ptr: u16) -> Option<u16> {
    let lo_off = ptr.checked_sub(org)? as usize;
    let hi_ptr = (ptr & 0xFF00) | (ptr.wrapping_add(1) & 0x00FF);
    let hi_off = hi_ptr.checked_sub(org)? as usize;
    let lo = *rom.get(lo_off)? as u16;
    let hi = *rom.get(hi_off)? as u16;
    Some(lo | (hi << 8))
}

pub fn disassemble_trace(
    rom: &[u8],
    org: u16,
    extra_entries: &[u16],
    tables: &[(u16, u16)],
    code_tables: &[(u16, u16, u16)],
) {
    let n = rom.len();
    let mut is_code_start = vec![false; n];
    let mut is_code = vec![false; n]; // opcode or operand byte of a decoded instr
    let mut labels: BTreeSet<u16> = BTreeSet::new();

    // Seed the worklist from the hardware vectors and any manual entry points.
    let mut work: Vec<u16> = Vec::new();
    let seed = |work: &mut Vec<u16>, labels: &mut BTreeSet<u16>, addr: u16| {
        labels.insert(addr);
        work.push(addr);
    };

    println!("; tracing disassembly from reset/IRQ/NMI vectors\n");
    print!("; entry points:");
    for (name, v) in [("NMI", 0xFFFAu16), ("RESET", 0xFFFC), ("IRQ", 0xFFFE)] {
        if let Some(target) = read_word(rom, org, v) {
            print!(" {name}=${target:04X}");
            seed(&mut work, &mut labels, target);
        }
    }
    for &e in extra_entries {
        print!(" USER=${e:04X}");
        seed(&mut work, &mut labels, e);
    }
    // Expand user-specified pointer tables: COUNT little-endian words starting
    // at ADDR, each word treated as a code address and seeded.
    for &(addr, count) in tables {
        print!(" TABLE=${addr:04X}x{count}");
        for k in 0..count {
            let entry = addr.wrapping_add(k.wrapping_mul(2));
            if let Some(target) = read_word(rom, org, entry) {
                seed(&mut work, &mut labels, target);
            }
        }
    }
    // Expand user-specified CODE tables (e.g. the KERNAL jump table): COUNT
    // slots at ADDR spaced STRIDE bytes apart, each slot's *address* seeded as
    // code — so the `JMP`/`JMP (ind)` living there gets decoded and followed.
    for &(addr, count, stride) in code_tables {
        print!(" CODETBL=${addr:04X}x{count}/{stride}");
        for k in 0..count {
            let entry = addr.wrapping_add(k.wrapping_mul(stride));
            if in_rom(rom, org, entry) {
                seed(&mut work, &mut labels, entry);
            }
        }
    }
    println!("\n");
    println!("        * = ${org:04X}\n");

    // Helper: absolute address -> in-image offset, if it falls inside the ROM.
    let to_off = |addr: u16| -> Option<usize> {
        let off = addr.checked_sub(org)? as usize;
        (off < n).then_some(off)
    };

    // Recursive traversal.
    while let Some(addr) = work.pop() {
        let Some(mut off) = to_off(addr) else { continue };
        let mut pc = addr;

        // Follow the straight-line run until a branch/jump/return or data.
        loop {
            if off >= n || is_code_start[off] {
                break;
            }
            let op = decode(rom[off]);
            let len = op.len() as usize;
            if op.mode == AddrMode::Ill || off + len > n {
                // Undecodable here (JAM/KIL or runs off the end): stop this run.
                break;
            }

            is_code_start[off] = true;
            for k in 0..len {
                is_code[off + k] = true;
            }

            let bytes = &rom[off..off + len];
            let m = op.mnemonic;
            let mode = op.mode;

            // Record control-flow targets and decide whether to fall through.
            let mut fallthrough = true;
            match mode {
                AddrMode::Rel => {
                    // Conditional branch: queue the target, keep falling through.
                    let t = branch_target(pc, bytes[1]);
                    labels.insert(t);
                    if to_off(t).is_some() {
                        work.push(t);
                    }
                }
                _ => {}
            }
            if m == "JMP" {
                if mode == AddrMode::Abs {
                    let t = u16::from_le_bytes([bytes[1], bytes[2]]);
                    labels.insert(t);
                    if to_off(t).is_some() {
                        work.push(t);
                    }
                } else if mode == AddrMode::Ind {
                    // Indirect JMP: if the pointer itself is in ROM its value is
                    // static, so resolve it (honoring the page-wrap bug) and
                    // follow the real target.
                    let ptr = u16::from_le_bytes([bytes[1], bytes[2]]);
                    if let Some(t) = read_word_bug(rom, org, ptr) {
                        labels.insert(t);
                        if to_off(t).is_some() {
                            work.push(t);
                        }
                    }
                }
                fallthrough = false;
            } else if m == "JSR" {
                let t = u16::from_le_bytes([bytes[1], bytes[2]]);
                labels.insert(t);
                if to_off(t).is_some() {
                    work.push(t);
                }
                // JSR returns, so execution continues after it.
            } else if m == "RTS" || m == "RTI" || m == "BRK" {
                fallthrough = false;
            }

            if !fallthrough {
                break;
            }
            pc = pc.wrapping_add(len as u16);
            off += len;
        }
    }

    // ---- Emit the annotated listing ----
    let mut instr_count = 0usize;
    let mut code_bytes = 0usize;
    let mut i = 0usize;
    while i < n {
        let addr = org.wrapping_add(i as u16);

        if is_code_start[i] {
            if labels.contains(&addr) {
                println!("\nL{addr:04X}:");
            }
            let op = decode(rom[i]);
            let len = op.len() as usize;
            let bytes = &rom[i..i + len];
            let hex = hex_bytes(bytes);
            let operand = format_operand_labeled(op.mnemonic, op.mode, addr, bytes, &labels);
            println!("{addr:04X}:  {hex:<8}  {:<4} {operand}", op.mnemonic);
            instr_count += 1;
            code_bytes += len;
            i += len;
        } else {
            // Gather a data run: consecutive non-code bytes, broken at labels
            // and capped at 8 bytes per line.
            let run_start = i;
            while i < n && !is_code_start[i] {
                // Stop the run if the next address carries a label so it stays visible.
                if i > run_start && labels.contains(&org.wrapping_add(i as u16)) {
                    break;
                }
                if i - run_start == 8 {
                    break;
                }
                i += 1;
            }
            let da = org.wrapping_add(run_start as u16);
            if labels.contains(&da) {
                println!("\nL{da:04X}:");
            }
            let chunk = &rom[run_start..i];
            let hex = chunk
                .iter()
                .map(|b| format!("${b:02X}"))
                .collect::<Vec<_>>()
                .join(",");
            let ascii: String = chunk
                .iter()
                .map(|&b| if (0x20..0x7F).contains(&b) { b as char } else { '.' })
                .collect();
            println!("{da:04X}:  {:<8}  .byte {hex:<28} ; {ascii}", hex_bytes(chunk));
        }
    }

    let data_bytes = n - code_bytes;
    let pct = (code_bytes as f64 / n as f64) * 100.0;
    println!("\n; ---- summary ----");
    println!("; {instr_count} instructions, {code_bytes} code bytes, {data_bytes} data bytes");
    println!("; code coverage: {pct:.1}% (uncovered = data or code only reachable via computed jumps)");
    println!("; {} labels", labels.iter().filter(|a| to_off(**a).is_some()).count());
}

/// Like [`crate::format_operand`], but substitutes an `Lxxxx` label for
/// control-flow targets (branches, `JMP`/`JSR` absolute) that we labeled.
fn format_operand_labeled(
    mnemonic: &str,
    mode: AddrMode,
    addr: u16,
    bytes: &[u8],
    labels: &BTreeSet<u16>,
) -> String {
    let labeled = |t: u16| -> Option<String> { labels.contains(&t).then(|| format!("L{t:04X}")) };

    match mode {
        AddrMode::Rel => {
            let t = branch_target(addr, bytes[1]);
            labeled(t).unwrap_or_else(|| format!("${t:04X}"))
        }
        AddrMode::Abs if mnemonic == "JMP" || mnemonic == "JSR" => {
            let t = u16::from_le_bytes([bytes[1], bytes[2]]);
            labeled(t).unwrap_or_else(|| format!("${t:04X}"))
        }
        _ => format_operand(mode, addr, bytes),
    }
}
