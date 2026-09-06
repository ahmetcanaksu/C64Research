//! A tiny live debugger driven from stdin, so you can inspect a running (or
//! stuck) machine without stopping it. Type a command in the terminal; the
//! answer prints there. Handled once per frame in the main loop.

use c64::C64;
use mos6502::{decode, AddrMode};

/// Handle one debugger command line against the machine. Returns text to print.
pub fn command(c64: &mut C64, line: &str) -> String {
    let mut it = line.split_whitespace();
    let cmd = it.next().unwrap_or("");
    let arg = |it: &mut std::str::SplitWhitespace, def: u16| -> u16 {
        it.next().map(parse_num).unwrap_or(Some(def)).unwrap_or(def)
    };

    match cmd {
        "" => String::new(),
        "help" | "?" => HELP.to_string(),
        "cpu" | "r" => cpu(c64),
        "vic" => vic(c64),
        "irq" => irq(c64),
        "spr" | "sprites" => sprites(c64),
        "bus" => bus(c64),
        "mem" | "m" => {
            let mut it = line.split_whitespace();
            it.next();
            let addr = arg(&mut it, 0);
            let len = arg(&mut it, 64);
            mem(c64, addr, len)
        }
        "dis" | "d" => {
            let mut it = line.split_whitespace();
            it.next();
            let addr = it.next().and_then(parse_num).unwrap_or(c64.cpu.pc);
            dis(c64, addr, 16)
        }
        "stuck" => stuck(c64),
        "poke" => {
            let mut it = line.split_whitespace();
            it.next();
            match (it.next().and_then(parse_num), it.next().and_then(parse_num)) {
                (Some(a), Some(v)) => {
                    c64.board.ram[a as usize] = v as u8;
                    format!("poked ${a:04X} = ${:02X}", v as u8)
                }
                _ => "usage: poke <addr> <val>".into(),
            }
        }
        "key" => {
            let mut it = line.split_whitespace();
            it.next();
            match it.next().and_then(parse_num) {
                Some(code) => {
                    // Hold it for ~8 frames so the keyboard scan catches it.
                    format!("holding matrix code {code} for a moment (use for testing)")
                        + &{
                            for _ in 0..8 {
                                c64.board.key_matrix = [0; 8];
                                c64.board.set_key(code as u8, true);
                                let end = c64.cpu.cycles.wrapping_add(19_700);
                                while c64.cpu.cycles < end {
                                    c64.step();
                                }
                            }
                            c64.board.key_matrix = [0; 8];
                            format!("  -> PC now ${:04X}", c64.cpu.pc)
                        }
                }
                None => "usage: key <matrix-code>  (e.g. 56=1, 59=2, 60=space)".into(),
            }
        }
        other => format!("unknown command '{other}' — type 'help'"),
    }
}

const HELP: &str = "\
debug commands (type in this terminal while the emulator runs):
  cpu | r          CPU registers, flags, PC
  vic              VIC-II mode, raster, colours, IRQ
  irq              interrupt state (who is asserting, is the CPU masked)
  spr | sprites    sprite registers
  bus              serial (IEC) bus lines
  mem <a> [len]    hex dump (through banking), e.g.  mem 0400 100
  dis [a]          disassemble from <a> (default PC)
  stuck            sample the PC to see if it is looping, and where
  poke <a> <v>     write a RAM byte
  key <code>       hold a keyboard matrix code briefly (56=1 59=2 60=space)
  help | ?         this list
numbers are hex by default; prefix with # for decimal";

fn cpu(c: &C64) -> String {
    let p = &c.cpu;
    let f = |on: bool, ch: char| if on { ch } else { ch.to_ascii_lowercase() };
    format!(
        "PC:{:04X} A:{:02X} X:{:02X} Y:{:02X} SP:{:02X}  [{}{}{}{}{}{}]  I(masked):{}  cyc:{}",
        p.pc,
        p.a,
        p.x,
        p.y,
        p.sp,
        f(p.n, 'N'),
        f(p.v, 'V'),
        f(p.d, 'D'),
        f(p.i, 'I'),
        f(p.z, 'Z'),
        f(p.c, 'C'),
        p.i as u8,
        p.cycles
    )
}

fn vic(c: &mut C64) -> String {
    let r = c.board.vic.regs;
    let ecm = r[0x11] & 0x40 != 0;
    let bmm = r[0x11] & 0x20 != 0;
    let mcm = r[0x16] & 0x10 != 0;
    let mode = match (bmm, mcm, ecm) {
        (true, true, _) => "MC-BITMAP",
        (true, false, _) => "BITMAP",
        (false, true, _) => "MC-TEXT",
        (false, false, true) => "ECM-TEXT",
        _ => "TEXT",
    };
    let cmp = r[0x12] as u16 | (((r[0x11] & 0x80) as u16) << 1);
    format!(
        "mode:{mode} raster:{} d011:{:02X} d016:{:02X} d018:{:02X} border:{:X} bg:{:X}\n\
         raster-cmp:{cmp} d019(latch):{:02X} d01a(enable):{:02X} irq:{}",
        c.board.vic.raster(),
        r[0x11],
        r[0x16],
        r[0x18],
        r[0x20] & 15,
        r[0x21] & 15,
        r[0x19] & 0x0F,
        r[0x1A],
        c.board.vic.irq_asserted()
    )
}

fn irq(c: &mut C64) -> String {
    let vic = c.board.vic.irq_asserted();
    let cia1 = c.board.cia1.irq_asserted();
    let cia2 = c.board.cia2.irq_asserted();
    let masked = c.cpu.i as u8;
    let (v314, v315) = (c.board.ram[0x0314], c.board.ram[0x0315]);
    let (rst_lo, rst_hi) = (c.board.peek(0xFFFE), c.board.peek(0xFFFF));
    format!(
        "CPU I-flag (IRQ masked): {masked}\nasserting -> VIC:{vic} CIA1:{cia1} CIA2(NMI):{cia2}\n\
         $0314 IRQ vec -> ${v315:02X}{v314:02X}   $FFFE -> ${rst_hi:02X}{rst_lo:02X}"
    )
}

fn sprites(c: &C64) -> String {
    let r = c.board.vic.regs;
    let mut s = format!(
        "enable:{:02X} x-exp:{:02X} y-exp:{:02X} mc:{:02X} pri:{:02X} mc0:{:X} mc1:{:X}\n",
        r[0x15], r[0x1D], r[0x17], r[0x1C], r[0x1B], r[0x25] & 15, r[0x26] & 15
    );
    for i in 0..8 {
        if r[0x15] & (1 << i) != 0 {
            let x = r[i * 2] as u16 | (((r[0x10] >> i) & 1) as u16) << 8;
            s += &format!("  spr{i}: x={x} y={} col:{:X}\n", r[1 + i * 2], r[0x27 + i] & 15);
        }
    }
    s
}

fn bus(c: &C64) -> String {
    let b = &c.board.iec;
    format!("ATN:{} CLK:{} DATA:{}  (1 = released/high)", b.atn() as u8, b.clk() as u8, b.data() as u8)
}

fn mem(c: &mut C64, addr: u16, len: u16) -> String {
    let mut s = String::new();
    let mut a = addr;
    let mut left = len;
    while left > 0 {
        let n = left.min(16);
        let mut hex = String::new();
        let mut asc = String::new();
        for i in 0..n {
            let b = c.board.peek(a.wrapping_add(i));
            hex += &format!("{b:02X} ");
            asc.push(if (0x20..0x7F).contains(&b) { b as char } else { '.' });
        }
        s += &format!("{a:04X}: {hex:<48} {asc}\n");
        a = a.wrapping_add(n);
        left -= n;
    }
    s
}

fn dis(c: &mut C64, addr: u16, count: usize) -> String {
    let mut s = String::new();
    let mut a = addr;
    for _ in 0..count {
        let op = decode(c.board.peek(a));
        let len = op.len() as u16;
        let b1 = c.board.peek(a.wrapping_add(1));
        let b2 = c.board.peek(a.wrapping_add(2));
        let operand = format_operand(op.mode, a, b1, b2);
        let mark = if a == c.cpu.pc { "->" } else { "  " };
        s += &format!("{mark}{a:04X}: {:<4} {operand}\n", op.mnemonic);
        a = a.wrapping_add(len);
    }
    s
}

fn stuck(c: &mut C64) -> String {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    let (mut lo, mut hi) = (0xFFFFu16, 0u16);
    // Note: this advances the machine ~a second's worth to sample it.
    for _ in 0..200_000u32 {
        let pc = c.cpu.pc;
        seen.insert(pc);
        lo = lo.min(pc);
        hi = hi.max(pc);
        c.step();
    }
    format!(
        "PC over 200k steps: {} distinct in ${lo:04X}..${hi:04X}  {}",
        seen.len(),
        if seen.len() < 500 { "<< LOOPING (try 'dis' at the low address)" } else { "(running freely)" }
    )
}

fn format_operand(mode: AddrMode, addr: u16, b1: u8, b2: u8) -> String {
    use AddrMode::*;
    let w = u16::from_le_bytes([b1, b2]);
    match mode {
        Imp | Ill => String::new(),
        Acc => "A".into(),
        Imm => format!("#${b1:02X}"),
        Zp => format!("${b1:02X}"),
        Zpx => format!("${b1:02X},X"),
        Zpy => format!("${b1:02X},Y"),
        Abs => format!("${w:04X}"),
        Abx => format!("${w:04X},X"),
        Aby => format!("${w:04X},Y"),
        Ind => format!("(${w:04X})"),
        Izx => format!("(${b1:02X},X)"),
        Izy => format!("(${b1:02X}),Y"),
        Rel => format!("${:04X}", addr.wrapping_add(2).wrapping_add(b1 as i8 as u16)),
    }
}

/// Parse a number: hex by default, `#` prefix for decimal, `$`/`0x` accepted.
fn parse_num(s: &str) -> Option<u16> {
    let s = s.trim();
    if let Some(d) = s.strip_prefix('#') {
        d.parse().ok()
    } else if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix('$')) {
        u16::from_str_radix(h, 16).ok()
    } else {
        u16::from_str_radix(s, 16).ok()
    }
}
