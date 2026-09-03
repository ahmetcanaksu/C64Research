//! Cycle-stepped MOS 6502 / 6510 CPU core.
//!
//! Executes all 151 official opcodes, including BCD (decimal) mode `ADC`/`SBC`
//! per Bruce Clark's canonical NMOS algorithm. The core is `no_std` and talks to
//! the outside world only through the [`Bus`] trait, so the same executor drives
//! the full C64 machine, the 1541 drive, or a flat test RAM — and later the
//! target MCU. Cycle counts include the common page-cross and branch-taken
//! penalties.
//!
//! Not yet modeled: the handful of undocumented opcodes (run as NOP), and the
//! exact cycle behavior of a few edge cases. Verified against the Klaus Dormann
//! 6502 functional test — see `tests/klaus.rs`.

use crate::{AddrMode, OPCODES};

/// The system bus the CPU reads and writes. `read` takes `&mut self` because
/// hardware reads (I/O registers) can have side effects.
pub trait Bus {
    fn read(&mut self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, val: u8);
}

/// 6502 register file and status flags.
#[derive(Debug, Clone, Copy)]
pub struct Cpu {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub sp: u8,
    pub pc: u16,
    // Status flags, unpacked for clarity.
    pub c: bool,
    pub z: bool,
    pub i: bool,
    pub d: bool,
    pub v: bool,
    pub n: bool,
    /// Total cycles executed since construction.
    pub cycles: u64,
}

impl Default for Cpu {
    fn default() -> Self {
        Cpu {
            a: 0,
            x: 0,
            y: 0,
            sp: 0xFD,
            pc: 0,
            c: false,
            z: false,
            i: true,
            d: false,
            v: false,
            n: false,
            cycles: 0,
        }
    }
}

const STACK_BASE: u16 = 0x0100;
const VEC_NMI: u16 = 0xFFFA;
const VEC_RESET: u16 = 0xFFFC;
const VEC_IRQ: u16 = 0xFFFE;

impl Cpu {
    pub fn new() -> Self {
        Cpu::default()
    }

    /// Power-on / reset: load PC from the reset vector, set SP=$FD and I=1.
    pub fn reset(&mut self, bus: &mut impl Bus) {
        self.pc = self.read16(bus, VEC_RESET);
        self.sp = 0xFD;
        self.i = true;
        self.cycles = self.cycles.wrapping_add(7);
    }

    /// Maskable interrupt. Serviced only when the I flag is clear.
    pub fn irq(&mut self, bus: &mut impl Bus) {
        if self.i {
            return;
        }
        self.interrupt(bus, VEC_IRQ, false);
    }

    /// Non-maskable interrupt.
    pub fn nmi(&mut self, bus: &mut impl Bus) {
        self.interrupt(bus, VEC_NMI, false);
    }

    fn interrupt(&mut self, bus: &mut impl Bus, vector: u16, brk: bool) {
        let pc = self.pc;
        self.push(bus, (pc >> 8) as u8);
        self.push(bus, pc as u8);
        self.push(bus, self.status(brk));
        self.i = true;
        self.pc = self.read16(bus, vector);
        self.cycles = self.cycles.wrapping_add(7);
    }

    /// Execute one instruction; return the cycles it took.
    pub fn step(&mut self, bus: &mut impl Bus) -> u8 {
        let opcode = self.fetch8(bus);
        let mode = OPCODES[opcode as usize].mode;
        let cyc = self.execute(bus, opcode, mode);
        self.cycles = self.cycles.wrapping_add(cyc as u64);
        cyc
    }

    // ---- status packing ----

    fn status(&self, brk: bool) -> u8 {
        (self.c as u8)
            | (self.z as u8) << 1
            | (self.i as u8) << 2
            | (self.d as u8) << 3
            | (brk as u8) << 4
            | 1 << 5
            | (self.v as u8) << 6
            | (self.n as u8) << 7
    }

    fn set_status(&mut self, p: u8) {
        self.c = p & 0x01 != 0;
        self.z = p & 0x02 != 0;
        self.i = p & 0x04 != 0;
        self.d = p & 0x08 != 0;
        self.v = p & 0x40 != 0;
        self.n = p & 0x80 != 0;
    }

    // ---- primitive bus/pc/stack helpers ----

    fn fetch8(&mut self, bus: &mut impl Bus) -> u8 {
        let b = bus.read(self.pc);
        self.pc = self.pc.wrapping_add(1);
        b
    }

    fn fetch16(&mut self, bus: &mut impl Bus) -> u16 {
        let lo = self.fetch8(bus) as u16;
        let hi = self.fetch8(bus) as u16;
        lo | (hi << 8)
    }

    fn read16(&self, bus: &mut impl Bus, addr: u16) -> u16 {
        let lo = bus.read(addr) as u16;
        let hi = bus.read(addr.wrapping_add(1)) as u16;
        lo | (hi << 8)
    }

    /// Read two bytes with the 6502 page-wrap bug (used only by `JMP (ind)`):
    /// the high byte comes from the same page as the pointer.
    fn read16_bug(&self, bus: &mut impl Bus, addr: u16) -> u16 {
        let lo = bus.read(addr) as u16;
        let hi_addr = (addr & 0xFF00) | (addr.wrapping_add(1) & 0x00FF);
        let hi = bus.read(hi_addr) as u16;
        lo | (hi << 8)
    }

    fn push(&mut self, bus: &mut impl Bus, val: u8) {
        bus.write(STACK_BASE | self.sp as u16, val);
        self.sp = self.sp.wrapping_sub(1);
    }

    fn pull(&mut self, bus: &mut impl Bus) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        bus.read(STACK_BASE | self.sp as u16)
    }

    fn set_zn(&mut self, val: u8) {
        self.z = val == 0;
        self.n = val & 0x80 != 0;
    }

    // ---- addressing ----

    /// Resolve the effective address for `mode`, advancing PC past the operand.
    /// Returns `(addr, page_crossed)`. Not used for Imp/Acc/Rel.
    fn operand(&mut self, bus: &mut impl Bus, mode: AddrMode) -> (u16, bool) {
        use AddrMode::*;
        match mode {
            Imm => {
                let a = self.pc;
                self.pc = self.pc.wrapping_add(1);
                (a, false)
            }
            Zp => (self.fetch8(bus) as u16, false),
            Zpx => ((self.fetch8(bus).wrapping_add(self.x)) as u16, false),
            Zpy => ((self.fetch8(bus).wrapping_add(self.y)) as u16, false),
            Abs => (self.fetch16(bus), false),
            Abx => {
                let base = self.fetch16(bus);
                let a = base.wrapping_add(self.x as u16);
                (a, page_crossed(base, a))
            }
            Aby => {
                let base = self.fetch16(bus);
                let a = base.wrapping_add(self.y as u16);
                (a, page_crossed(base, a))
            }
            Ind => {
                let ptr = self.fetch16(bus);
                (self.read16_bug(bus, ptr), false)
            }
            Izx => {
                let zp = self.fetch8(bus).wrapping_add(self.x);
                let lo = bus.read(zp as u16) as u16;
                let hi = bus.read(zp.wrapping_add(1) as u16) as u16;
                (lo | (hi << 8), false)
            }
            Izy => {
                let zp = self.fetch8(bus);
                let lo = bus.read(zp as u16) as u16;
                let hi = bus.read(zp.wrapping_add(1) as u16) as u16;
                let base = lo | (hi << 8);
                let a = base.wrapping_add(self.y as u16);
                (a, page_crossed(base, a))
            }
            Imp | Acc | Rel | Ill => (0, false),
        }
    }

    // ---- the instruction dispatcher ----

    fn execute(&mut self, bus: &mut impl Bus, opcode: u8, mode: AddrMode) -> u8 {
        let mnem = OPCODES[opcode as usize].mnemonic;

        match mnem {
            // ---- loads ----
            "LDA" => {
                let (a, x) = self.operand(bus, mode);
                let v = bus.read(a);
                self.a = v;
                self.set_zn(v);
                read_cost(mode, x)
            }
            "LDX" => {
                let (a, x) = self.operand(bus, mode);
                let v = bus.read(a);
                self.x = v;
                self.set_zn(v);
                read_cost(mode, x)
            }
            "LDY" => {
                let (a, x) = self.operand(bus, mode);
                let v = bus.read(a);
                self.y = v;
                self.set_zn(v);
                read_cost(mode, x)
            }
            // ---- stores ----
            "STA" => {
                let (a, _) = self.operand(bus, mode);
                bus.write(a, self.a);
                store_cost(mode)
            }
            "STX" => {
                let (a, _) = self.operand(bus, mode);
                bus.write(a, self.x);
                store_cost(mode)
            }
            "STY" => {
                let (a, _) = self.operand(bus, mode);
                bus.write(a, self.y);
                store_cost(mode)
            }
            // ---- register transfers ----
            "TAX" => { self.x = self.a; self.set_zn(self.x); 2 }
            "TAY" => { self.y = self.a; self.set_zn(self.y); 2 }
            "TXA" => { self.a = self.x; self.set_zn(self.a); 2 }
            "TYA" => { self.a = self.y; self.set_zn(self.a); 2 }
            "TSX" => { self.x = self.sp; self.set_zn(self.x); 2 }
            "TXS" => { self.sp = self.x; 2 }
            // ---- stack ----
            "PHA" => { self.push(bus, self.a); 3 }
            "PHP" => { self.push(bus, self.status(true)); 3 }
            "PLA" => { let v = self.pull(bus); self.a = v; self.set_zn(v); 4 }
            "PLP" => { let v = self.pull(bus); self.set_status(v); 4 }
            // ---- logical ----
            "AND" => {
                let (a, x) = self.operand(bus, mode);
                self.a &= bus.read(a);
                self.set_zn(self.a);
                read_cost(mode, x)
            }
            "ORA" => {
                let (a, x) = self.operand(bus, mode);
                self.a |= bus.read(a);
                self.set_zn(self.a);
                read_cost(mode, x)
            }
            "EOR" => {
                let (a, x) = self.operand(bus, mode);
                self.a ^= bus.read(a);
                self.set_zn(self.a);
                read_cost(mode, x)
            }
            "BIT" => {
                let (a, _) = self.operand(bus, mode);
                let v = bus.read(a);
                self.z = (self.a & v) == 0;
                self.v = v & 0x40 != 0;
                self.n = v & 0x80 != 0;
                if mode == AddrMode::Zp { 3 } else { 4 }
            }
            // ---- arithmetic ----
            "ADC" => {
                let (a, x) = self.operand(bus, mode);
                let v = bus.read(a);
                self.adc(v);
                read_cost(mode, x)
            }
            "SBC" => {
                let (a, x) = self.operand(bus, mode);
                let v = bus.read(a);
                self.sbc(v);
                read_cost(mode, x)
            }
            "CMP" => {
                let (a, x) = self.operand(bus, mode);
                let v = bus.read(a);
                self.compare(self.a, v);
                read_cost(mode, x)
            }
            "CPX" => {
                let (a, _) = self.operand(bus, mode);
                let v = bus.read(a);
                self.compare(self.x, v);
                if mode == AddrMode::Imm { 2 } else if mode == AddrMode::Zp { 3 } else { 4 }
            }
            "CPY" => {
                let (a, _) = self.operand(bus, mode);
                let v = bus.read(a);
                self.compare(self.y, v);
                if mode == AddrMode::Imm { 2 } else if mode == AddrMode::Zp { 3 } else { 4 }
            }
            // ---- inc / dec ----
            "INC" => {
                let (a, _) = self.operand(bus, mode);
                let v = bus.read(a).wrapping_add(1);
                bus.write(a, v);
                self.set_zn(v);
                rmw_cost(mode)
            }
            "DEC" => {
                let (a, _) = self.operand(bus, mode);
                let v = bus.read(a).wrapping_sub(1);
                bus.write(a, v);
                self.set_zn(v);
                rmw_cost(mode)
            }
            "INX" => { self.x = self.x.wrapping_add(1); self.set_zn(self.x); 2 }
            "INY" => { self.y = self.y.wrapping_add(1); self.set_zn(self.y); 2 }
            "DEX" => { self.x = self.x.wrapping_sub(1); self.set_zn(self.x); 2 }
            "DEY" => { self.y = self.y.wrapping_sub(1); self.set_zn(self.y); 2 }
            // ---- shifts / rotates ----
            "ASL" => self.rmw_shift(bus, mode, ShiftOp::Asl),
            "LSR" => self.rmw_shift(bus, mode, ShiftOp::Lsr),
            "ROL" => self.rmw_shift(bus, mode, ShiftOp::Rol),
            "ROR" => self.rmw_shift(bus, mode, ShiftOp::Ror),
            // ---- jumps / calls ----
            "JMP" => {
                let (a, _) = self.operand(bus, mode);
                self.pc = a;
                if mode == AddrMode::Ind { 5 } else { 3 }
            }
            "JSR" => {
                // Push address of the last byte of this instruction (PC-1).
                let target = self.fetch16(bus);
                let ret = self.pc.wrapping_sub(1);
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.pc = target;
                6
            }
            "RTS" => {
                let lo = self.pull(bus) as u16;
                let hi = self.pull(bus) as u16;
                self.pc = ((lo | (hi << 8)).wrapping_add(1)) as u16;
                6
            }
            "RTI" => {
                let p = self.pull(bus);
                self.set_status(p);
                let lo = self.pull(bus) as u16;
                let hi = self.pull(bus) as u16;
                self.pc = lo | (hi << 8);
                6
            }
            "BRK" => {
                // BRK is a 2-byte instruction: the pushed PC skips the padding byte.
                self.pc = self.pc.wrapping_add(1);
                self.interrupt_no_extra_cycles(bus);
                7
            }
            // ---- branches ----
            "BPL" => self.branch(bus, !self.n),
            "BMI" => self.branch(bus, self.n),
            "BVC" => self.branch(bus, !self.v),
            "BVS" => self.branch(bus, self.v),
            "BCC" => self.branch(bus, !self.c),
            "BCS" => self.branch(bus, self.c),
            "BNE" => self.branch(bus, !self.z),
            "BEQ" => self.branch(bus, self.z),
            // ---- flag ops ----
            "CLC" => { self.c = false; 2 }
            "SEC" => { self.c = true; 2 }
            "CLI" => { self.i = false; 2 }
            "SEI" => { self.i = true; 2 }
            "CLD" => { self.d = false; 2 }
            "SED" => { self.d = true; 2 }
            "CLV" => { self.v = false; 2 }
            "NOP" => {
                // Official NOP is implied; the undocumented NOPs read (and skip)
                // an operand, so consume its bytes to keep the stream aligned.
                match mode {
                    AddrMode::Imp => 2,
                    _ => {
                        let (_, x) = self.operand(bus, mode);
                        read_cost(mode, x)
                    }
                }
            }

            // ---- undocumented opcodes (see the table in lib.rs) ----
            "LAX" => {
                let (a, x) = self.operand(bus, mode);
                let v = bus.read(a);
                self.a = v;
                self.x = v;
                self.set_zn(v);
                read_cost(mode, x)
            }
            "SAX" => {
                let (a, _) = self.operand(bus, mode);
                bus.write(a, self.a & self.x);
                store_cost(mode)
            }
            "SLO" => {
                let (a, _) = self.operand(bus, mode);
                let v = self.shift(bus.read(a), ShiftOp::Asl);
                bus.write(a, v);
                self.a |= v;
                self.set_zn(self.a);
                illegal_rmw_cost(mode)
            }
            "RLA" => {
                let (a, _) = self.operand(bus, mode);
                let v = self.shift(bus.read(a), ShiftOp::Rol);
                bus.write(a, v);
                self.a &= v;
                self.set_zn(self.a);
                illegal_rmw_cost(mode)
            }
            "SRE" => {
                let (a, _) = self.operand(bus, mode);
                let v = self.shift(bus.read(a), ShiftOp::Lsr);
                bus.write(a, v);
                self.a ^= v;
                self.set_zn(self.a);
                illegal_rmw_cost(mode)
            }
            "RRA" => {
                let (a, _) = self.operand(bus, mode);
                let v = self.shift(bus.read(a), ShiftOp::Ror);
                bus.write(a, v);
                self.adc(v);
                illegal_rmw_cost(mode)
            }
            "DCP" => {
                let (a, _) = self.operand(bus, mode);
                let v = bus.read(a).wrapping_sub(1);
                bus.write(a, v);
                self.compare(self.a, v);
                illegal_rmw_cost(mode)
            }
            "ISC" => {
                let (a, _) = self.operand(bus, mode);
                let v = bus.read(a).wrapping_add(1);
                bus.write(a, v);
                self.sbc(v);
                illegal_rmw_cost(mode)
            }
            "ANC" => {
                let (a, _) = self.operand(bus, mode);
                self.a &= bus.read(a);
                self.set_zn(self.a);
                self.c = self.a & 0x80 != 0; // carry = bit 7
                2
            }
            "ALR" => {
                let (a, _) = self.operand(bus, mode);
                self.a &= bus.read(a);
                self.c = self.a & 0x01 != 0;
                self.a >>= 1;
                self.set_zn(self.a);
                2
            }
            "ARR" => {
                let (a, _) = self.operand(bus, mode);
                self.a &= bus.read(a);
                self.a = (self.a >> 1) | ((self.c as u8) << 7);
                self.set_zn(self.a);
                self.c = self.a & 0x40 != 0;
                self.v = (((self.a >> 6) ^ (self.a >> 5)) & 1) != 0;
                2
            }
            "SBX" => {
                let (a, _) = self.operand(bus, mode);
                let v = bus.read(a);
                let t = self.a & self.x;
                self.c = t >= v;
                self.x = t.wrapping_sub(v);
                self.set_zn(self.x);
                2
            }
            "SHA" => {
                let (a, _) = self.operand(bus, mode);
                let hi = (a >> 8) as u8;
                bus.write(a, self.a & self.x & hi.wrapping_add(1));
                store_cost(mode)
            }
            "SHX" => {
                let (a, _) = self.operand(bus, mode);
                let hi = (a >> 8) as u8;
                bus.write(a, self.x & hi.wrapping_add(1));
                store_cost(mode)
            }
            "SHY" => {
                let (a, _) = self.operand(bus, mode);
                let hi = (a >> 8) as u8;
                bus.write(a, self.y & hi.wrapping_add(1));
                store_cost(mode)
            }
            "TAS" => {
                let (a, _) = self.operand(bus, mode);
                self.sp = self.a & self.x;
                let hi = (a >> 8) as u8;
                bus.write(a, self.sp & hi.wrapping_add(1));
                store_cost(mode)
            }
            "LAS" => {
                let (a, x) = self.operand(bus, mode);
                let v = bus.read(a) & self.sp;
                self.a = v;
                self.x = v;
                self.sp = v;
                self.set_zn(v);
                read_cost(mode, x)
            }
            "ANE" => {
                // Highly unstable; a common, workable approximation.
                let (a, _) = self.operand(bus, mode);
                self.a = (self.a | 0xEE) & self.x & bus.read(a);
                self.set_zn(self.a);
                2
            }

            // JAM/KIL or anything else we don't model: 1-byte no-op.
            _ => 2,
        }
    }

    /// BRK path: like `interrupt` but the caller already adjusted PC and owns
    /// the cycle count, and the pushed status has B set.
    fn interrupt_no_extra_cycles(&mut self, bus: &mut impl Bus) {
        let pc = self.pc;
        self.push(bus, (pc >> 8) as u8);
        self.push(bus, pc as u8);
        self.push(bus, self.status(true));
        self.i = true;
        self.pc = self.read16(bus, VEC_IRQ);
    }

    fn branch(&mut self, bus: &mut impl Bus, cond: bool) -> u8 {
        let off = self.fetch8(bus) as i8;
        if !cond {
            return 2;
        }
        let from = self.pc;
        let to = (from as i32 + off as i32) as u16;
        self.pc = to;
        3 + page_crossed(from, to) as u8
    }

    /// Add with carry. Handles both binary and BCD (decimal) mode, following
    /// Bruce Clark's canonical NMOS 6502 algorithm ("Decimal Mode in the 6502").
    fn adc(&mut self, val: u8) {
        let a = self.a;
        let c = self.c as u16;

        if self.d {
            // Low nibble, with decimal fixup.
            let mut al = (a as u16 & 0x0F) + (val as u16 & 0x0F) + c;
            if al >= 0x0A {
                al = ((al + 0x06) & 0x0F) + 0x10;
            }
            // High nibble (al carries the +$10 from above).
            let mut a2 = (a as u16 & 0xF0) + (val as u16 & 0xF0) + al;
            // N and V are taken from the intermediate result before the high fixup.
            self.n = a2 & 0x80 != 0;
            self.v = ((a as u16 ^ a2) & (val as u16 ^ a2) & 0x80) != 0;
            if a2 >= 0xA0 {
                a2 += 0x60;
            }
            self.c = a2 >= 0x100;
            // Z is based on the plain binary sum, as on real NMOS silicon.
            self.z = ((a as u16 + val as u16 + c) & 0xFF) == 0;
            self.a = a2 as u8;
        } else {
            let sum = a as u16 + val as u16 + c;
            let result = sum as u8;
            self.v = ((a ^ result) & (val ^ result) & 0x80) != 0;
            self.c = sum > 0xFF;
            self.a = result;
            self.set_zn(result);
        }
    }

    /// Subtract with carry. In decimal mode the N/V/Z/C flags match a binary
    /// SBC (only the accumulator differs) — again per Bruce Clark's algorithm.
    fn sbc(&mut self, val: u8) {
        let a = self.a;
        let c_in = self.c as i16;

        // All flags come from the binary operation A + ~val + C.
        let bin = a as u16 + (!val) as u16 + self.c as u16;
        let bin_res = bin as u8;
        self.v = ((a ^ bin_res) & ((!val) ^ bin_res) & 0x80) != 0;
        self.c = bin > 0xFF;
        self.set_zn(bin_res);

        if self.d {
            let mut al = (a as i16 & 0x0F) - (val as i16 & 0x0F) + c_in - 1;
            if al < 0 {
                al = ((al - 0x06) & 0x0F) - 0x10;
            }
            let mut a2 = (a as i16 & 0xF0) - (val as i16 & 0xF0) + al;
            if a2 < 0 {
                a2 -= 0x60;
            }
            self.a = a2 as u8;
        } else {
            self.a = bin_res;
        }
    }

    fn compare(&mut self, reg: u8, val: u8) {
        let diff = reg.wrapping_sub(val);
        self.c = reg >= val;
        self.set_zn(diff);
    }

    fn rmw_shift(&mut self, bus: &mut impl Bus, mode: AddrMode, kind: ShiftOp) -> u8 {
        if mode == AddrMode::Acc {
            let v = self.shift(self.a, kind);
            self.a = v;
            return 2;
        }
        let (a, _) = self.operand(bus, mode);
        let v = self.shift(bus.read(a), kind);
        bus.write(a, v);
        rmw_cost(mode)
    }

    fn shift(&mut self, val: u8, kind: ShiftOp) -> u8 {
        let old_c = self.c;
        let result = match kind {
            ShiftOp::Asl => {
                self.c = val & 0x80 != 0;
                val << 1
            }
            ShiftOp::Lsr => {
                self.c = val & 0x01 != 0;
                val >> 1
            }
            ShiftOp::Rol => {
                self.c = val & 0x80 != 0;
                (val << 1) | old_c as u8
            }
            ShiftOp::Ror => {
                self.c = val & 0x01 != 0;
                (val >> 1) | ((old_c as u8) << 7)
            }
        };
        self.set_zn(result);
        result
    }
}

#[derive(Clone, Copy)]
enum ShiftOp {
    Asl,
    Lsr,
    Rol,
    Ror,
}

fn page_crossed(a: u16, b: u16) -> bool {
    (a & 0xFF00) != (b & 0xFF00)
}

/// Base cycles for a read/ALU instruction, plus the page-cross penalty.
fn read_cost(mode: AddrMode, crossed: bool) -> u8 {
    use AddrMode::*;
    let base = match mode {
        Imm => 2,
        Zp => 3,
        Zpx | Zpy => 4,
        Abs => 4,
        Abx | Aby => 4,
        Izx => 6,
        Izy => 5,
        _ => 2,
    };
    base + (crossed && matches!(mode, Abx | Aby | Izy)) as u8
}

/// Cycles for a store instruction (no page-cross penalty).
fn store_cost(mode: AddrMode) -> u8 {
    use AddrMode::*;
    match mode {
        Zp => 3,
        Zpx | Zpy => 4,
        Abs => 4,
        Abx | Aby => 5,
        Izx | Izy => 6,
        _ => 2,
    }
}

/// Cycles for a read-modify-write instruction (always the worst case).
fn rmw_cost(mode: AddrMode) -> u8 {
    use AddrMode::*;
    match mode {
        Zp => 5,
        Zpx => 6,
        Abs => 6,
        Abx => 7,
        _ => 2,
    }
}

/// Cycles for an undocumented RMW+ALU op (SLO/RLA/SRE/RRA/DCP/ISC).
fn illegal_rmw_cost(mode: AddrMode) -> u8 {
    use AddrMode::*;
    match mode {
        Zp => 5,
        Zpx => 6,
        Abs => 6,
        Abx | Aby => 7,
        Izx | Izy => 8,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flat 64 KB RAM — the simplest possible bus.
    struct Ram([u8; 0x10000]);
    impl Ram {
        fn new() -> Self {
            Ram([0; 0x10000])
        }
        fn load(&mut self, at: u16, bytes: &[u8]) {
            self.0[at as usize..at as usize + bytes.len()].copy_from_slice(bytes);
        }
    }
    impl Bus for Ram {
        fn read(&mut self, addr: u16) -> u8 {
            self.0[addr as usize]
        }
        fn write(&mut self, addr: u16, val: u8) {
            self.0[addr as usize] = val;
        }
    }

    #[test]
    fn illegal_lax_loads_a_and_x() {
        let mut cpu = Cpu::new();
        let mut ram = Ram::new();
        ram.0[0x0040] = 0x77;
        ram.load(0x0200, &[0xA7, 0x40]); // LAX $40
        cpu.pc = 0x0200;
        cpu.step(&mut ram);
        assert_eq!(cpu.a, 0x77);
        assert_eq!(cpu.x, 0x77);
    }

    #[test]
    fn illegal_sax_stores_a_and_x() {
        let mut cpu = Cpu::new();
        let mut ram = Ram::new();
        // LDA #$CF ; LDX #$3C ; SAX $50   -> $50 = $CF & $3C = $0C
        ram.load(0x0200, &[0xA9, 0xCF, 0xA2, 0x3C, 0x87, 0x50]);
        cpu.pc = 0x0200;
        cpu.step(&mut ram);
        cpu.step(&mut ram);
        cpu.step(&mut ram);
        assert_eq!(ram.0[0x0050], 0x0C);
    }

    #[test]
    fn illegal_dcp_decrements_then_compares() {
        let mut cpu = Cpu::new();
        let mut ram = Ram::new();
        ram.0[0x0060] = 0x11;
        // LDA #$10 ; DCP $60  -> $60 becomes $10, and A($10) == mem -> Z set, C set
        ram.load(0x0200, &[0xA9, 0x10, 0xC7, 0x60]);
        cpu.pc = 0x0200;
        cpu.step(&mut ram);
        cpu.step(&mut ram);
        assert_eq!(ram.0[0x0060], 0x10);
        assert!(cpu.z);
        assert!(cpu.c);
    }

    #[test]
    fn multibyte_nop_keeps_the_stream_aligned() {
        let mut cpu = Cpu::new();
        let mut ram = Ram::new();
        // $0C is a 3-byte undocumented NOP. If decoded as 1 byte, the LDX below
        // would be misread. It must skip all three bytes.
        ram.load(0x0200, &[0x0C, 0xAD, 0xDE, 0xA2, 0x99]); // NOP $DEAD ; LDX #$99
        cpu.pc = 0x0200;
        cpu.step(&mut ram); // NOP abs
        assert_eq!(cpu.pc, 0x0203);
        cpu.step(&mut ram); // LDX #$99
        assert_eq!(cpu.x, 0x99);
    }

    #[test]
    fn adc_sets_carry_and_overflow() {
        let mut cpu = Cpu::new();
        let mut ram = Ram::new();
        // LDA #$50 ; ADC #$50  -> $A0, V=1 (positive+positive=negative), C=0
        ram.load(0x0200, &[0xA9, 0x50, 0x69, 0x50]);
        cpu.pc = 0x0200;
        cpu.step(&mut ram);
        cpu.step(&mut ram);
        assert_eq!(cpu.a, 0xA0);
        assert!(cpu.v);
        assert!(!cpu.c);
        assert!(cpu.n);
    }

    #[test]
    fn jsr_rts_round_trip() {
        let mut cpu = Cpu::new();
        let mut ram = Ram::new();
        // 0300: JSR $0400 ; (returns here) LDX #$AA
        ram.load(0x0300, &[0x20, 0x00, 0x04, 0xA2, 0xAA]);
        // 0400: LDA #$11 ; RTS
        ram.load(0x0400, &[0xA9, 0x11, 0x60]);
        cpu.pc = 0x0300;
        cpu.step(&mut ram); // JSR
        assert_eq!(cpu.pc, 0x0400);
        cpu.step(&mut ram); // LDA #$11
        cpu.step(&mut ram); // RTS
        assert_eq!(cpu.pc, 0x0303);
        assert_eq!(cpu.a, 0x11);
        cpu.step(&mut ram); // LDX #$AA
        assert_eq!(cpu.x, 0xAA);
    }

    #[test]
    fn sum_1_to_10_loop() {
        let mut cpu = Cpu::new();
        let mut ram = Ram::new();
        // Sum 1..=10 into A, leaving 55 ($37). X counts down from 10.
        //   LDA #$00
        //   LDX #$0A
        // loop:
        //   STX $10        ; scratch
        //   CLC
        //   ADC $10
        //   DEX
        //   BNE loop
        //   STA $20
        ram.load(
            0x0200,
            &[
                0xA9, 0x00, // LDA #0
                0xA2, 0x0A, // LDX #10
                0x86, 0x10, // STX $10
                0x18, // CLC
                0x65, 0x10, // ADC $10
                0xCA, // DEX
                0xD0, 0xF8, // BNE loop (-8 -> back to STX)
                0x85, 0x20, // STA $20
            ],
        );
        cpu.pc = 0x0200;
        for _ in 0..200 {
            cpu.step(&mut ram);
            if cpu.pc == 0x020F {
                break; // reached STA $20's successor after running it
            }
        }
        assert_eq!(ram.0[0x20], 55);
    }

    /// End-to-end: run the *actual* 1541 zero-page RAM test bytes from
    /// `C1541.rom` ($EAA7-$EAC8) and confirm the CPU clears zero page and
    /// falls through to $EAC9 — cross-checking the executor against the ROM and
    /// against the hand-ported Rust in the `c1541` crate.
    #[test]
    fn runs_real_1541_ram_test() {
        let mut cpu = Cpu::new();
        let mut ram = Ram::new();
        ram.load(
            0xEAA7,
            &[
                0xE8, // INX
                0xA0, 0x00, // LDY #$00
                0xA2, 0x00, // LDX #$00
                // EAAC fill loop
                0x8A, // TXA
                0x95, 0x00, // STA $00,X
                0xE8, // INX
                0xD0, 0xFA, // BNE $EAAC
                // EAB2 test loop
                0x8A, // TXA
                0xD5, 0x00, // CMP $00,X
                0xD0, 0xB7, // BNE $EA6E (fault)
                0xF6, 0x00, // INC $00,X
                0xC8, // INY
                0xD0, 0xFB, // BNE $EAB7
                0xD5, 0x00, // CMP $00,X
                0xD0, 0xAE, // BNE $EA6E
                0x94, 0x00, // STY $00,X
                0xB5, 0x00, // LDA $00,X
                0xD0, 0xA8, // BNE $EA6E
                0xE8, // INX
                0xD0, 0xE9, // BNE $EAB2
            ],
        );
        // Put a trap at the fault handler and the fall-through so we can tell
        // success (reach $EAC9) from failure (reach $EA6E).
        cpu.pc = 0xEAA7;
        cpu.x = 0xFF; // as left by reset, so the first INX makes X=0
        // The routine runs ~256 outer * ~256 inner increments ≈ 200k instructions.
        let mut reached_end = false;
        for _ in 0..2_000_000 {
            if cpu.pc == 0xEAC9 {
                reached_end = true;
                break;
            }
            if cpu.pc == 0xEA6E {
                panic!("RAM test branched to the fault handler on healthy RAM");
            }
            cpu.step(&mut ram);
        }
        assert!(reached_end, "RAM test never reached the fall-through at $EAC9");
        // Exactly like the hand-ported `zero_page_ram_test`: zero page is cleared.
        assert!(ram.0[0x00..0x100].iter().all(|&b| b == 0));
    }
}
