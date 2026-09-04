//! MOS 6502 / 6510 opcode tables and helpers.
//!
//! `no_std` and allocation-free on purpose: this same decode table is used by
//! the ROM disassembler today and by the cycle-accurate CPU executor later,
//! including on the target MCU. All string formatting lives in downstream
//! (`std`) tools, not here.

#![no_std]

pub mod cpu;
pub use cpu::{Bus, Cpu};

/// Addressing mode of a 6502 instruction. Determines how the operand bytes are
/// interpreted and how long the instruction is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddrMode {
    /// No operand, e.g. `NOP`, `RTS`.
    Imp,
    /// Operates on the accumulator, e.g. `ASL A`.
    Acc,
    /// Immediate, `#$nn`.
    Imm,
    /// Zero page, `$nn`.
    Zp,
    /// Zero page, X-indexed: `$nn,X`.
    Zpx,
    /// Zero page, Y-indexed: `$nn,Y`.
    Zpy,
    /// Absolute, `$nnnn`.
    Abs,
    /// Absolute, X-indexed: `$nnnn,X`.
    Abx,
    /// Absolute, Y-indexed: `$nnnn,Y`.
    Aby,
    /// Indirect, `($nnnn)` — only used by `JMP`.
    Ind,
    /// Indexed indirect, `($nn,X)`.
    Izx,
    /// Indirect indexed, `($nn),Y`.
    Izy,
    /// Relative branch, signed 8-bit offset from the following instruction.
    Rel,
    /// Illegal / undocumented opcode — rendered as raw data.
    Ill,
}

/// A decoded opcode: its mnemonic, addressing mode, and whether it is an
/// undocumented ("illegal") instruction.
#[derive(Debug, Clone, Copy)]
pub struct Op {
    pub mnemonic: &'static str,
    pub mode: AddrMode,
    pub illegal: bool,
}

impl Op {
    const fn new(mnemonic: &'static str, mode: AddrMode) -> Self {
        Op { mnemonic, mode, illegal: false }
    }

    /// An undocumented opcode with a known effect and addressing mode.
    const fn new_ill(mnemonic: &'static str, mode: AddrMode) -> Self {
        Op { mnemonic, mode, illegal: true }
    }

    /// Placeholder for an undocumented opcode we don't model (rendered as data).
    const fn illegal() -> Self {
        Op { mnemonic: "???", mode: AddrMode::Ill, illegal: true }
    }

    /// Total instruction length in bytes (opcode + operand).
    ///
    /// Not a collection length — an opcode is never "empty" — so there is
    /// deliberately no `is_empty` to go with it.
    #[allow(clippy::len_without_is_empty)]
    pub const fn len(&self) -> u8 {
        instr_len(self.mode)
    }

    /// True if this opcode is not a documented 6502 instruction.
    pub const fn is_illegal(&self) -> bool {
        self.illegal
    }
}

/// Instruction length in bytes for a given addressing mode.
pub const fn instr_len(mode: AddrMode) -> u8 {
    use AddrMode::*;
    match mode {
        Imp | Acc | Ill => 1,
        Imm | Zp | Zpx | Zpy | Izx | Izy | Rel => 2,
        Abs | Abx | Aby | Ind => 3,
    }
}

/// Decode a single opcode byte into its [`Op`].
pub const fn decode(opcode: u8) -> Op {
    OPCODES[opcode as usize]
}

/// The full 256-entry opcode table. Undocumented opcodes are left as
/// [`Op::illegal`] and should be rendered as data by a disassembler.
pub const OPCODES: [Op; 256] = build_table();

const fn build_table() -> [Op; 256] {
    use AddrMode::*;
    let mut t = [Op::illegal(); 256];

    // 0x00
    t[0x00] = Op::new("BRK", Imp);
    t[0x01] = Op::new("ORA", Izx);
    t[0x05] = Op::new("ORA", Zp);
    t[0x06] = Op::new("ASL", Zp);
    t[0x08] = Op::new("PHP", Imp);
    t[0x09] = Op::new("ORA", Imm);
    t[0x0A] = Op::new("ASL", Acc);
    t[0x0D] = Op::new("ORA", Abs);
    t[0x0E] = Op::new("ASL", Abs);
    // 0x10
    t[0x10] = Op::new("BPL", Rel);
    t[0x11] = Op::new("ORA", Izy);
    t[0x15] = Op::new("ORA", Zpx);
    t[0x16] = Op::new("ASL", Zpx);
    t[0x18] = Op::new("CLC", Imp);
    t[0x19] = Op::new("ORA", Aby);
    t[0x1D] = Op::new("ORA", Abx);
    t[0x1E] = Op::new("ASL", Abx);
    // 0x20
    t[0x20] = Op::new("JSR", Abs);
    t[0x21] = Op::new("AND", Izx);
    t[0x24] = Op::new("BIT", Zp);
    t[0x25] = Op::new("AND", Zp);
    t[0x26] = Op::new("ROL", Zp);
    t[0x28] = Op::new("PLP", Imp);
    t[0x29] = Op::new("AND", Imm);
    t[0x2A] = Op::new("ROL", Acc);
    t[0x2C] = Op::new("BIT", Abs);
    t[0x2D] = Op::new("AND", Abs);
    t[0x2E] = Op::new("ROL", Abs);
    // 0x30
    t[0x30] = Op::new("BMI", Rel);
    t[0x31] = Op::new("AND", Izy);
    t[0x35] = Op::new("AND", Zpx);
    t[0x36] = Op::new("ROL", Zpx);
    t[0x38] = Op::new("SEC", Imp);
    t[0x39] = Op::new("AND", Aby);
    t[0x3D] = Op::new("AND", Abx);
    t[0x3E] = Op::new("ROL", Abx);
    // 0x40
    t[0x40] = Op::new("RTI", Imp);
    t[0x41] = Op::new("EOR", Izx);
    t[0x45] = Op::new("EOR", Zp);
    t[0x46] = Op::new("LSR", Zp);
    t[0x48] = Op::new("PHA", Imp);
    t[0x49] = Op::new("EOR", Imm);
    t[0x4A] = Op::new("LSR", Acc);
    t[0x4C] = Op::new("JMP", Abs);
    t[0x4D] = Op::new("EOR", Abs);
    t[0x4E] = Op::new("LSR", Abs);
    // 0x50
    t[0x50] = Op::new("BVC", Rel);
    t[0x51] = Op::new("EOR", Izy);
    t[0x55] = Op::new("EOR", Zpx);
    t[0x56] = Op::new("LSR", Zpx);
    t[0x58] = Op::new("CLI", Imp);
    t[0x59] = Op::new("EOR", Aby);
    t[0x5D] = Op::new("EOR", Abx);
    t[0x5E] = Op::new("LSR", Abx);
    // 0x60
    t[0x60] = Op::new("RTS", Imp);
    t[0x61] = Op::new("ADC", Izx);
    t[0x65] = Op::new("ADC", Zp);
    t[0x66] = Op::new("ROR", Zp);
    t[0x68] = Op::new("PLA", Imp);
    t[0x69] = Op::new("ADC", Imm);
    t[0x6A] = Op::new("ROR", Acc);
    t[0x6C] = Op::new("JMP", Ind);
    t[0x6D] = Op::new("ADC", Abs);
    t[0x6E] = Op::new("ROR", Abs);
    // 0x70
    t[0x70] = Op::new("BVS", Rel);
    t[0x71] = Op::new("ADC", Izy);
    t[0x75] = Op::new("ADC", Zpx);
    t[0x76] = Op::new("ROR", Zpx);
    t[0x78] = Op::new("SEI", Imp);
    t[0x79] = Op::new("ADC", Aby);
    t[0x7D] = Op::new("ADC", Abx);
    t[0x7E] = Op::new("ROR", Abx);
    // 0x80
    t[0x81] = Op::new("STA", Izx);
    t[0x84] = Op::new("STY", Zp);
    t[0x85] = Op::new("STA", Zp);
    t[0x86] = Op::new("STX", Zp);
    t[0x88] = Op::new("DEY", Imp);
    t[0x8A] = Op::new("TXA", Imp);
    t[0x8C] = Op::new("STY", Abs);
    t[0x8D] = Op::new("STA", Abs);
    t[0x8E] = Op::new("STX", Abs);
    // 0x90
    t[0x90] = Op::new("BCC", Rel);
    t[0x91] = Op::new("STA", Izy);
    t[0x94] = Op::new("STY", Zpx);
    t[0x95] = Op::new("STA", Zpx);
    t[0x96] = Op::new("STX", Zpy);
    t[0x98] = Op::new("TYA", Imp);
    t[0x99] = Op::new("STA", Aby);
    t[0x9A] = Op::new("TXS", Imp);
    t[0x9D] = Op::new("STA", Abx);
    // 0xA0
    t[0xA0] = Op::new("LDY", Imm);
    t[0xA1] = Op::new("LDA", Izx);
    t[0xA2] = Op::new("LDX", Imm);
    t[0xA4] = Op::new("LDY", Zp);
    t[0xA5] = Op::new("LDA", Zp);
    t[0xA6] = Op::new("LDX", Zp);
    t[0xA8] = Op::new("TAY", Imp);
    t[0xA9] = Op::new("LDA", Imm);
    t[0xAA] = Op::new("TAX", Imp);
    t[0xAC] = Op::new("LDY", Abs);
    t[0xAD] = Op::new("LDA", Abs);
    t[0xAE] = Op::new("LDX", Abs);
    // 0xB0
    t[0xB0] = Op::new("BCS", Rel);
    t[0xB1] = Op::new("LDA", Izy);
    t[0xB4] = Op::new("LDY", Zpx);
    t[0xB5] = Op::new("LDA", Zpx);
    t[0xB6] = Op::new("LDX", Zpy);
    t[0xB8] = Op::new("CLV", Imp);
    t[0xB9] = Op::new("LDA", Aby);
    t[0xBA] = Op::new("TSX", Imp);
    t[0xBC] = Op::new("LDY", Abx);
    t[0xBD] = Op::new("LDA", Abx);
    t[0xBE] = Op::new("LDX", Aby);
    // 0xC0
    t[0xC0] = Op::new("CPY", Imm);
    t[0xC1] = Op::new("CMP", Izx);
    t[0xC4] = Op::new("CPY", Zp);
    t[0xC5] = Op::new("CMP", Zp);
    t[0xC6] = Op::new("DEC", Zp);
    t[0xC8] = Op::new("INY", Imp);
    t[0xC9] = Op::new("CMP", Imm);
    t[0xCA] = Op::new("DEX", Imp);
    t[0xCC] = Op::new("CPY", Abs);
    t[0xCD] = Op::new("CMP", Abs);
    t[0xCE] = Op::new("DEC", Abs);
    // 0xD0
    t[0xD0] = Op::new("BNE", Rel);
    t[0xD1] = Op::new("CMP", Izy);
    t[0xD5] = Op::new("CMP", Zpx);
    t[0xD6] = Op::new("DEC", Zpx);
    t[0xD8] = Op::new("CLD", Imp);
    t[0xD9] = Op::new("CMP", Aby);
    t[0xDD] = Op::new("CMP", Abx);
    t[0xDE] = Op::new("DEC", Abx);
    // 0xE0
    t[0xE0] = Op::new("CPX", Imm);
    t[0xE1] = Op::new("SBC", Izx);
    t[0xE4] = Op::new("CPX", Zp);
    t[0xE5] = Op::new("SBC", Zp);
    t[0xE6] = Op::new("INC", Zp);
    t[0xE8] = Op::new("INX", Imp);
    t[0xE9] = Op::new("SBC", Imm);
    t[0xEA] = Op::new("NOP", Imp);
    t[0xEC] = Op::new("CPX", Abs);
    t[0xED] = Op::new("SBC", Abs);
    t[0xEE] = Op::new("INC", Abs);
    // 0xF0
    t[0xF0] = Op::new("BEQ", Rel);
    t[0xF1] = Op::new("SBC", Izy);
    t[0xF5] = Op::new("SBC", Zpx);
    t[0xF6] = Op::new("INC", Zpx);
    t[0xF8] = Op::new("SED", Imp);
    t[0xF9] = Op::new("SBC", Aby);
    t[0xFD] = Op::new("SBC", Abx);
    t[0xFE] = Op::new("INC", Abx);

    // ---- undocumented ("illegal") opcodes ----
    // These are the stable ones real C64 software relies on. Unstable
    // "magic constant" opcodes (SHA/SHX/SHY/TAS/LAS/ANE) get common
    // approximations; the JAM/KIL opcodes are left as data.

    // Multi-byte NOPs — MUST have the right length so the instruction stream
    // stays aligned even though they do nothing.
    let nop_imp = [0x1Au8, 0x3A, 0x5A, 0x7A, 0xDA, 0xFA];
    let nop_imm = [0x80u8, 0x82, 0x89, 0xC2, 0xE2];
    let nop_zp = [0x04u8, 0x44, 0x64];
    let nop_zpx = [0x14u8, 0x34, 0x54, 0x74, 0xD4, 0xF4];
    let nop_abx = [0x1Cu8, 0x3C, 0x5C, 0x7C, 0xDC, 0xFC];
    let mut k = 0;
    while k < nop_imp.len() {
        t[nop_imp[k] as usize] = Op::new_ill("NOP", Imp);
        k += 1;
    }
    k = 0;
    while k < nop_imm.len() {
        t[nop_imm[k] as usize] = Op::new_ill("NOP", Imm);
        k += 1;
    }
    k = 0;
    while k < nop_zp.len() {
        t[nop_zp[k] as usize] = Op::new_ill("NOP", Zp);
        k += 1;
    }
    k = 0;
    while k < nop_zpx.len() {
        t[nop_zpx[k] as usize] = Op::new_ill("NOP", Zpx);
        k += 1;
    }
    k = 0;
    while k < nop_abx.len() {
        t[nop_abx[k] as usize] = Op::new_ill("NOP", Abx);
        k += 1;
    }
    t[0x0C] = Op::new_ill("NOP", Abs);

    // LAX = LDA + LDX.
    t[0xA7] = Op::new_ill("LAX", Zp);
    t[0xB7] = Op::new_ill("LAX", Zpy);
    t[0xAF] = Op::new_ill("LAX", Abs);
    t[0xBF] = Op::new_ill("LAX", Aby);
    t[0xA3] = Op::new_ill("LAX", Izx);
    t[0xB3] = Op::new_ill("LAX", Izy);
    t[0xAB] = Op::new_ill("LAX", Imm); // unstable; A=X=imm approximation

    // SAX = store A & X.
    t[0x87] = Op::new_ill("SAX", Zp);
    t[0x97] = Op::new_ill("SAX", Zpy);
    t[0x8F] = Op::new_ill("SAX", Abs);
    t[0x83] = Op::new_ill("SAX", Izx);

    // Read-modify-write + ALU combos.
    let rmw: [(&str, [u8; 7]); 6] = [
        ("SLO", [0x07, 0x17, 0x0F, 0x1F, 0x1B, 0x03, 0x13]),
        ("RLA", [0x27, 0x37, 0x2F, 0x3F, 0x3B, 0x23, 0x33]),
        ("SRE", [0x47, 0x57, 0x4F, 0x5F, 0x5B, 0x43, 0x53]),
        ("RRA", [0x67, 0x77, 0x6F, 0x7F, 0x7B, 0x63, 0x73]),
        ("DCP", [0xC7, 0xD7, 0xCF, 0xDF, 0xDB, 0xC3, 0xD3]),
        ("ISC", [0xE7, 0xF7, 0xEF, 0xFF, 0xFB, 0xE3, 0xF3]),
    ];
    let rmw_modes = [Zp, Zpx, Abs, Abx, Aby, Izx, Izy];
    let mut i = 0;
    while i < rmw.len() {
        let (name, ops) = rmw[i];
        let mut j = 0;
        while j < 7 {
            t[ops[j] as usize] = Op::new_ill(name, rmw_modes[j]);
            j += 1;
        }
        i += 1;
    }

    // Immediate-mode combos.
    t[0x0B] = Op::new_ill("ANC", Imm);
    t[0x2B] = Op::new_ill("ANC", Imm);
    t[0x4B] = Op::new_ill("ALR", Imm);
    t[0x6B] = Op::new_ill("ARR", Imm);
    t[0xCB] = Op::new_ill("SBX", Imm);
    t[0xEB] = Op::new_ill("SBC", Imm); // alias of SBC #imm

    // Unstable "magic" opcodes (approximated).
    t[0x9F] = Op::new_ill("SHA", Aby);
    t[0x93] = Op::new_ill("SHA", Izy);
    t[0x9E] = Op::new_ill("SHX", Aby);
    t[0x9C] = Op::new_ill("SHY", Abx);
    t[0x9B] = Op::new_ill("TAS", Aby);
    t[0xBB] = Op::new_ill("LAS", Aby);
    t[0x8B] = Op::new_ill("ANE", Imm);

    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_opcodes_decode() {
        assert_eq!(decode(0xA9).mnemonic, "LDA");
        assert_eq!(decode(0xA9).mode, AddrMode::Imm);
        assert_eq!(decode(0x20).mnemonic, "JSR");
        assert_eq!(decode(0x6C).mode, AddrMode::Ind);
        assert_eq!(decode(0x00).mnemonic, "BRK");
    }

    #[test]
    fn lengths_are_correct() {
        assert_eq!(decode(0xEA).len(), 1); // NOP, implied
        assert_eq!(decode(0xA9).len(), 2); // LDA #imm
        assert_eq!(decode(0x4C).len(), 3); // JMP abs
    }

    #[test]
    fn official_opcode_count_is_151() {
        let n = OPCODES.iter().filter(|o| !o.is_illegal()).count();
        assert_eq!(n, 151);
    }
}
