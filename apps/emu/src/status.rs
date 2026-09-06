//! An opt-in "system monitor" window for the emulator (`--status`).
//!
//! It reads only **high-level state, once per frame** — the CPU registers, the
//! banking latch, a few VIC registers, the three serial-bus lines, the joystick
//! — and never instruments individual RAM or CPU accesses. A snapshot per frame
//! is essentially free, so the monitor doesn't slow the machine down.
//!
//! Text is drawn with the C64's own character ROM, for the look of the thing.

use c64::C64;

pub const COLS: usize = 40;
pub const ROWS: usize = 25;
pub const WIDTH: usize = COLS * 8; // 320
pub const HEIGHT: usize = ROWS * 8; // 200

const BG: u32 = 0x00_0A08; // near-black
const FG: u32 = 0x33_FF66; // phosphor green
const DIM: u32 = 0x1C_7A3C; // dim green for labels
const HOT: u32 = 0xFF_C24B; // amber for anything currently active

/// The monitor's framebuffer plus a little activity-tracking state.
pub struct Monitor {
    fb: Vec<u32>,
    last_bus: (bool, bool, bool),
    bus_active_until: u32,
}

impl Default for Monitor {
    fn default() -> Self {
        Monitor {
            fb: vec![BG; WIDTH * HEIGHT],
            last_bus: (true, true, true),
            bus_active_until: 0,
        }
    }
}

impl Monitor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn framebuffer(&self) -> &[u32] {
        &self.fb
    }

    /// Redraw the dashboard from the machine's current state.
    pub fn render(&mut self, c64: &C64, drive_attached: bool, frames: u32) {
        for px in self.fb.iter_mut() {
            *px = BG;
        }

        // Serial-bus activity: flag it "busy" briefly whenever a line moves.
        let bus = (c64.board.iec.atn(), c64.board.iec.clk(), c64.board.iec.data());
        if bus != self.last_bus {
            self.bus_active_until = frames + 25;
            self.last_bus = bus;
        }
        let bus_busy = frames < self.bus_active_until;

        let cpu = &c64.cpu;
        let vr = &c64.board.vic.regs;
        let (basic, kernal, io, chargen) = c64.board.banking();

        // ---- header ----
        self.put(c64, 0, 0, "== C64 SYSTEM MONITOR =================", DIM);

        // ---- CPU ----
        self.put(c64, 0, 2, "CPU", HOT);
        let line = format!(
            "PC:{:04X}  A:{:02X} X:{:02X} Y:{:02X} SP:{:02X}",
            cpu.pc, cpu.a, cpu.x, cpu.y, cpu.sp
        );
        self.put(c64, 5, 2, &line, FG);
        let flags: String = [
            ('N', cpu.n),
            ('V', cpu.v),
            ('D', cpu.d),
            ('I', cpu.i),
            ('Z', cpu.z),
            ('C', cpu.c),
        ]
        .iter()
        .map(|(ch, on)| if *on { *ch } else { ch.to_ascii_lowercase() })
        .collect();
        self.put(c64, 5, 3, &format!("FLAGS {flags}"), FG);
        let irq = if c64.board.vic.irq_asserted() {
            "VIC"
        } else if c64.board.cia1.irq_asserted() {
            "CIA1"
        } else {
            "-"
        };
        self.put(c64, 20, 3, &format!("IRQ:{irq}"), if irq == "-" { DIM } else { HOT });
        self.put(c64, 5, 4, &format!("CYCLES:{}", cpu.cycles), DIM);

        // ---- memory banking ----
        self.put(c64, 0, 6, "MEM", HOT);
        self.put(c64, 5, 6, &format!("$01={:02X}", c64.board.port01()), FG);
        let a000 = if basic { "BASIC" } else { "RAM" };
        let d000 = if io {
            "I/O"
        } else if chargen {
            "CHAR"
        } else {
            "RAM"
        };
        let e000 = if kernal { "KERNAL" } else { "RAM" };
        self.put(c64, 5, 7, &format!("A000:{a000}  D000:{d000}  E000:{e000}"), FG);

        // ---- VIC ----
        let ecm = vr[0x11] & 0x40 != 0;
        let bmm = vr[0x11] & 0x20 != 0;
        let mcm = vr[0x16] & 0x10 != 0;
        let mode = match (bmm, mcm, ecm) {
            (true, true, _) => "MC-BITMAP",
            (true, false, _) => "BITMAP",
            (false, true, _) => "MC-TEXT",
            (false, false, true) => "ECM-TEXT",
            _ => "TEXT",
        };
        self.put(c64, 0, 9, "VIC", HOT);
        self.put(
            c64,
            5,
            9,
            &format!("RASTER:{:3}  MODE:{mode}", c64.board.vic.raster()),
            FG,
        );
        self.put(
            c64,
            5,
            10,
            &format!("BORDER:{:X} BG:{:X}  SPR-EN:{:02X}", vr[0x20] & 15, vr[0x21] & 15, vr[0x15]),
            FG,
        );
        let rasterirq = vr[0x1A] & 0x01 != 0;
        self.put(
            c64,
            5,
            11,
            &format!("RASTER-IRQ:{}", if rasterirq { "ON" } else { "off" }),
            if rasterirq { FG } else { DIM },
        );

        // ---- disk / serial bus ----
        self.put(c64, 0, 13, "DISK", HOT);
        self.put(
            c64,
            5,
            13,
            if drive_attached { "device 8 attached" } else { "no drive on bus" },
            if drive_attached { FG } else { DIM },
        );
        let lvl = |high: bool| if high { '1' } else { '0' };
        self.put(
            c64,
            5,
            14,
            &format!("BUS ATN:{} CLK:{} DATA:{}", lvl(bus.0), lvl(bus.1), lvl(bus.2)),
            FG,
        );
        self.put(
            c64,
            27,
            14,
            if bus_busy { "<< BUSY >>" } else { "idle" },
            if bus_busy { HOT } else { DIM },
        );

        // ---- cassette (not emulated, but the port bits are real) ----
        let p01 = c64.board.port01();
        self.put(c64, 0, 16, "TAPE", HOT);
        self.put(
            c64,
            5,
            16,
            &format!("motor:{} sense:{} (no datasette)", (p01 >> 5) & 1, (p01 >> 4) & 1),
            DIM,
        );

        // ---- joystick 2 ----
        let j = c64.board.joy2;
        let d = |bit: u8, ch: char| if j & bit == 0 { ch } else { '.' };
        self.put(c64, 0, 18, "JOY2", HOT);
        let js = format!(
            "{} {} {} {}  FIRE:{}",
            d(0x01, 'U'),
            d(0x02, 'D'),
            d(0x04, 'L'),
            d(0x08, 'R'),
            if j & 0x10 == 0 { "YES" } else { "no " }
        );
        self.put(c64, 5, 18, &js, if j != 0xFF { HOT } else { FG });

        self.put(c64, 0, 24, &format!("FRAME:{frames}"), DIM);
    }

    /// Draw a string at character cell (`col`, `row`) using the C64 char ROM.
    fn put(&mut self, c64: &C64, col: usize, row: usize, s: &str, color: u32) {
        let cg = &c64.board.chargen;
        for (i, ch) in s.bytes().enumerate() {
            let cx = col + i;
            if cx >= COLS || row >= ROWS {
                break;
            }
            let sc = screencode(ch) as usize;
            let x0 = cx * 8;
            let y0 = row * 8;
            for gy in 0..8 {
                let bits = cg[sc * 8 + gy];
                for gx in 0..8 {
                    if bits & (0x80 >> gx) != 0 {
                        self.fb[(y0 + gy) * WIDTH + x0 + gx] = color;
                    }
                }
            }
        }
    }
}

/// ASCII byte -> C64 screen code (uppercase character set).
fn screencode(c: u8) -> u8 {
    match c {
        b'@' => 0,
        b'A'..=b'Z' => c - 0x40,
        b'a'..=b'z' => c - 0x60,
        0x20..=0x3F => c, // space, digits, punctuation coincide with ASCII
        _ => 0x20,
    }
}
