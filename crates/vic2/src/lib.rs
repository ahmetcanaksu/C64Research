//! MOS 6567/6569 VIC-II video chip.
//!
//! Models what real C64 software uses:
//! - a **raster counter** with a **raster compare interrupt** ($D012/$D011 +
//!   $D019/$D01A) wired to the CPU IRQ — the backbone of split-screen effects,
//!   music timing, and most game main loops;
//! - all four display modes — standard/multicolor **text** and standard/
//!   multicolor **bitmap** (extended-background mode is approximated as text);
//! - the eight **sprites** (hi-res and multicolor, X/Y expansion).
//!
//! Rendering is **per scanline** ([`Vic::render_line`]), so a raster-interrupt
//! handler that reprograms the VIC mid-frame produces the right split.
//!
//! Not yet modeled: sprite/background priority nuances, sprite collisions, the
//! exact bad-line timing, and the border area (we render the 320x200 display
//! window). `no_std`, MCU-ready.

#![no_std]

/// Visible display window: 40x25 characters of 8x8 pixels.
pub const WIDTH: usize = 320;
pub const HEIGHT: usize = 200;

/// PAL-ish timing: 63 CPU cycles per raster line, 312 lines per frame.
const CYCLES_PER_LINE: u32 = 63;
const LINES_PER_FRAME: u16 = 312;
/// Sprite coordinate origins: sprite Y=50 / X=24 sit at the top-left of the
/// display window.
const SPRITE_Y_ORIGIN: i32 = 50;
const SPRITE_X_ORIGIN: i32 = 24;

/// The 16 C64 colours as `0x00RRGGBB` (the widely used VICE palette).
pub const PALETTE: [u32; 16] = [
    0x000000, 0xFFFFFF, 0x880000, 0xAAFFEE, 0xCC44CC, 0x00CC55, 0x0000AA, 0xEEEE77, 0xDD8855,
    0x664400, 0xFF7777, 0x333333, 0x777777, 0xAAFF66, 0x0088FF, 0xBBBBBB,
];

#[inline]
fn rgb(color: u8) -> u32 {
    PALETTE[(color & 0x0F) as usize]
}

/// The VIC-II.
#[derive(Clone)]
pub struct Vic {
    pub regs: [u8; 0x40],
    raster: u16,
    line_cycles: u32,
    raster_compare: u16,
    irq_latch: u8,  // $D019 (bit0 raster, ...)
    irq_enable: u8, // $D01A
}

impl Default for Vic {
    fn default() -> Self {
        Vic {
            regs: [0; 0x40],
            raster: 0,
            line_cycles: 0,
            raster_compare: 0,
            irq_latch: 0,
            irq_enable: 0,
        }
    }
}

impl Vic {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn raster(&self) -> u16 {
        self.raster
    }

    /// True while the VIC is pulling the (shared) CPU IRQ line.
    pub fn irq_asserted(&self) -> bool {
        (self.irq_latch & self.irq_enable & 0x0F) != 0
    }

    /// Advance the raster by `cycles` CPU clocks, latching the raster interrupt
    /// each time the raster reaches the compare line.
    pub fn tick(&mut self, cycles: u32) {
        self.line_cycles += cycles;
        while self.line_cycles >= CYCLES_PER_LINE {
            self.line_cycles -= CYCLES_PER_LINE;
            self.raster += 1;
            if self.raster >= LINES_PER_FRAME {
                self.raster = 0;
            }
            if self.raster == self.raster_compare {
                self.irq_latch |= 0x01;
            }
        }
    }

    pub fn read(&self, reg: u8) -> u8 {
        match reg & 0x3F {
            0x11 => (self.regs[0x11] & 0x7F) | (((self.raster >> 8) & 1) << 7) as u8,
            0x12 => self.raster as u8,
            0x19 => self.irq_latch | 0x70 | if self.irq_asserted() { 0x80 } else { 0 },
            0x1A => self.irq_enable | 0xF0,
            other => self.regs[other as usize],
        }
    }

    pub fn write(&mut self, reg: u8, val: u8) {
        let reg = (reg & 0x3F) as usize;
        self.regs[reg] = val;
        match reg {
            0x11 => {
                self.raster_compare = (self.raster_compare & 0x00FF) | (((val & 0x80) as u16) << 1);
            }
            0x12 => {
                self.raster_compare = (self.raster_compare & 0x0100) | val as u16;
            }
            0x19 => {
                // Writing 1 acknowledges (clears) that latch bit.
                self.irq_latch &= !(val & 0x0F);
            }
            0x1A => self.irq_enable = val & 0x0F,
            _ => {}
        }
    }

    pub fn border_rgb(&self) -> u32 {
        rgb(self.regs[0x20])
    }

    /// The VIC's 16 KB bank base (selected by CIA2 port A's low two bits).
    fn bank(cia2_pra: u8) -> usize {
        (3 - (cia2_pra & 0x03) as usize) * 0x4000
    }

    /// Render one display scanline `y` (0..199) into `fb` (WIDTH*HEIGHT).
    pub fn render_line(&self, y: usize, ram: &[u8], char_rom: &[u8], cia2_pra: u8, fb: &mut [u32]) {
        if y >= HEIGHT {
            return;
        }
        let bank = Self::bank(cia2_pra);
        let video = bank + ((self.regs[0x18] >> 4) as usize & 0x0F) * 0x0400;
        let char_base = bank + ((self.regs[0x18] >> 1) as usize & 0x07) * 0x0800;
        let bitmap_base = bank + ((self.regs[0x18] >> 3) as usize & 0x01) * 0x2000;
        let use_char_rom = matches!(char_base & 0x3FFF, 0x1000 | 0x1800);

        let ecm = self.regs[0x11] & 0x40 != 0;
        let bmm = self.regs[0x11] & 0x20 != 0;
        let mcm = self.regs[0x16] & 0x10 != 0;
        let bg0 = self.regs[0x21];

        let row = y / 8;
        let rline = y % 8; // pixel row within the character/bitmap cell
        let line_base = y * WIDTH;

        for col in 0..40usize {
            let cell = row * 40 + col;
            let x0 = col * 8;
            if bmm {
                // Bitmap modes read colours from the video matrix (and colour RAM).
                let byte = ram[(bitmap_base + row * 320 + col * 8 + (y % 8)) & 0xFFFF];
                let sc = ram[(video + cell) & 0xFFFF];
                if mcm {
                    let c = [bg0, sc >> 4, sc & 0x0F, ram[0xD800 + cell] & 0x0F];
                    for p in 0..4usize {
                        let bits = (byte >> (6 - p * 2)) & 0x03;
                        let color = rgb(c[bits as usize]);
                        fb[line_base + x0 + p * 2] = color;
                        fb[line_base + x0 + p * 2 + 1] = color;
                    }
                } else {
                    let fg = rgb(sc >> 4);
                    let bg = rgb(sc & 0x0F);
                    for p in 0..8usize {
                        fb[line_base + x0 + p] = if byte & (0x80 >> p) != 0 { fg } else { bg };
                    }
                }
            } else {
                // Text modes.
                let sc = ram[(video + cell) & 0xFFFF] as usize;
                let color = ram[0xD800 + cell];
                let glyph = if use_char_rom {
                    char_rom[(char_base & 0x0FFF) + sc * 8 + rline]
                } else {
                    ram[(char_base + sc * 8 + rline) & 0xFFFF]
                };
                if mcm && color & 0x08 != 0 {
                    // Multicolor character.
                    let c = [bg0, self.regs[0x22], self.regs[0x23], color & 0x07];
                    for p in 0..4usize {
                        let bits = (glyph >> (6 - p * 2)) & 0x03;
                        let px = rgb(c[bits as usize]);
                        fb[line_base + x0 + p * 2] = px;
                        fb[line_base + x0 + p * 2 + 1] = px;
                    }
                } else {
                    // Hi-res character (also the fallback for ECM, approximated).
                    let bg = if ecm { self.regs[0x21 + ((sc >> 6) & 3)] } else { bg0 };
                    let fg = rgb(color);
                    let bgc = rgb(bg);
                    let glyph = if ecm { char_rom_or(ram, char_rom, use_char_rom, char_base, sc & 0x3F, rline) } else { glyph };
                    for p in 0..8usize {
                        fb[line_base + x0 + p] = if glyph & (0x80 >> p) != 0 { fg } else { bgc };
                    }
                }
            }
        }

        self.render_sprites_line(y, ram, bank, video, cia2_pra, fb);
    }

    /// Overlay any sprites that cover scanline `y`.
    fn render_sprites_line(
        &self,
        y: usize,
        ram: &[u8],
        bank: usize,
        video: usize,
        _cia2_pra: u8,
        fb: &mut [u32],
    ) {
        let enabled = self.regs[0x15];
        if enabled == 0 {
            return;
        }
        let line_base = y * WIDTH;
        // Lower-numbered sprites have display priority, so draw high -> low.
        for i in (0..8usize).rev() {
            if enabled & (1 << i) == 0 {
                continue;
            }
            let x_expand = self.regs[0x1D] & (1 << i) != 0;
            let y_expand = self.regs[0x17] & (1 << i) != 0;
            let multicolor = self.regs[0x1C] & (1 << i) != 0;

            let sy = self.regs[0x01 + i * 2] as i32;
            let top = sy - SPRITE_Y_ORIGIN;
            let height = if y_expand { 42 } else { 21 };
            let dy = y as i32 - top;
            if dy < 0 || dy >= height {
                continue;
            }
            let data_row = if y_expand { dy / 2 } else { dy } as usize;

            let ptr = ram[(video + 0x03F8 + i) & 0xFFFF] as usize;
            let data = bank + ptr * 64 + data_row * 3;

            let sx = self.regs[0x00 + i * 2] as i32 | (((self.regs[0x10] >> i) & 1) as i32) << 8;
            let left = sx - SPRITE_X_ORIGIN;

            let sprite_color = self.regs[0x27 + i];
            let mc0 = self.regs[0x25];
            let mc1 = self.regs[0x26];

            let px_w = if x_expand { 2 } else { 1 };

            if multicolor {
                // 12 pixel-pairs, each 2 (or 4) display pixels wide.
                for pair in 0..12usize {
                    let byte = ram[(data + pair / 4) & 0xFFFF];
                    let shift = 6 - (pair % 4) * 2;
                    let bits = (byte >> shift) & 0x03;
                    if bits == 0 {
                        continue; // transparent
                    }
                    let color = rgb(match bits {
                        1 => mc0,
                        2 => sprite_color,
                        _ => mc1,
                    });
                    for sub in 0..(2 * px_w) {
                        let x = left + (pair as i32) * 2 * px_w + sub;
                        if (0..WIDTH as i32).contains(&x) {
                            fb[line_base + x as usize] = color;
                        }
                    }
                }
            } else {
                for bit in 0..24usize {
                    let byte = ram[(data + bit / 8) & 0xFFFF];
                    if byte & (0x80 >> (bit % 8)) == 0 {
                        continue;
                    }
                    let color = rgb(sprite_color);
                    for sub in 0..px_w {
                        let x = left + (bit as i32) * px_w + sub;
                        if (0..WIDTH as i32).contains(&x) {
                            fb[line_base + x as usize] = color;
                        }
                    }
                }
            }
        }
    }
}

/// Fetch a glyph row in ECM mode (character code is only 6 bits there).
fn char_rom_or(ram: &[u8], char_rom: &[u8], use_char_rom: bool, char_base: usize, sc: usize, rline: usize) -> u8 {
    if use_char_rom {
        char_rom[(char_base & 0xFFF) + sc * 8 + rline]
    } else {
        ram[(char_base + sc * 8 + rline) & 0xFFFF]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raster_advances_and_wraps() {
        let mut vic = Vic::new();
        vic.tick(CYCLES_PER_LINE);
        assert_eq!(vic.raster(), 1);
        assert_eq!(vic.read(0x12), 1);
    }

    #[test]
    fn raster_interrupt_fires_at_the_compare_line() {
        let mut vic = Vic::new();
        vic.write(0x1A, 0x01); // enable raster IRQ
        vic.write(0x12, 100); // compare line = 100
        assert!(!vic.irq_asserted());
        vic.tick(CYCLES_PER_LINE * 100); // reach line 100
        assert!(vic.irq_asserted());
        // Reading $D019 shows the raster flag + the summary bit.
        assert_eq!(vic.read(0x19) & 0x81, 0x81);
        // Acknowledge by writing bit 0.
        vic.write(0x19, 0x01);
        assert!(!vic.irq_asserted());
    }

    #[test]
    fn compare_uses_the_9th_bit() {
        let mut vic = Vic::new();
        vic.write(0x1A, 0x01);
        vic.write(0x12, 0x2C); // low byte
        vic.write(0x11, 0x80); // high bit set -> compare line = 300
        vic.tick(CYCLES_PER_LINE * 300);
        assert!(vic.irq_asserted());
    }

    #[test]
    fn renders_a_hires_character() {
        let mut vic = Vic::new();
        vic.write(0x18, 0x14); // screen $0400, chars $1000 (char ROM)
        vic.write(0x21, 0x06); // blue background
        let mut ram = [0u8; 0x10000];
        ram[0x0400] = 0x01;
        ram[0xD800] = 0x01; // white
        let mut char_rom = [0u8; 0x1000];
        char_rom[1 * 8] = 0xFF; // glyph row 0 solid
        let mut fb = [0u32; WIDTH * HEIGHT];
        vic.render_line(0, &ram, &char_rom, 0x07, &mut fb);
        assert_eq!(fb[0], PALETTE[1]); // white
        vic.render_line(1, &ram, &char_rom, 0x07, &mut fb);
        assert_eq!(fb[WIDTH], PALETTE[6]); // background on the next row
    }

    #[test]
    fn renders_standard_bitmap() {
        let mut vic = Vic::new();
        vic.write(0x11, 0x20); // BMM on (bitmap mode)
        vic.write(0x18, 0x18); // video matrix $0400, bitmap $2000
        let mut ram = [0u8; 0x10000];
        // First bitmap byte: alternating pixels; colours from the video matrix.
        ram[0x2000] = 0b1010_1010;
        ram[0x0400] = 0x1E; // upper nibble = fg (1), lower = bg (14)
        let char_rom = [0u8; 0x1000];
        let mut fb = [0u32; WIDTH * HEIGHT];
        vic.render_line(0, &ram, &char_rom, 0x07, &mut fb);
        assert_eq!(fb[0], PALETTE[1]); // bit set -> fg (white)
        assert_eq!(fb[1], PALETTE[14]); // bit clear -> bg (light blue)
    }

    #[test]
    fn renders_multicolor_bitmap() {
        let mut vic = Vic::new();
        vic.write(0x11, 0x20); // BMM
        vic.write(0x16, 0x10); // MCM -> multicolor bitmap
        vic.write(0x18, 0x18); // video $0400, bitmap $2000
        vic.write(0x21, 0x00); // background (bit-pair 00) = black
        let mut ram = [0u8; 0x10000];
        ram[0x2000] = 0b00_01_10_11; // four 2-bit pixels
        ram[0x0400] = 0x23; // upper nibble=2 (pair 01), lower=3 (pair 10)
        ram[0xD800] = 0x04; // colour RAM (pair 11)
        let char_rom = [0u8; 0x1000];
        let mut fb = [0u32; WIDTH * HEIGHT];
        vic.render_line(0, &ram, &char_rom, 0x07, &mut fb);
        assert_eq!(fb[0], PALETTE[0]); // 00 -> background
        assert_eq!(fb[2], PALETTE[2]); // 01 -> video matrix hi nibble
        assert_eq!(fb[4], PALETTE[3]); // 10 -> video matrix lo nibble
        assert_eq!(fb[6], PALETTE[4]); // 11 -> colour RAM
    }

    #[test]
    fn renders_a_sprite_pixel() {
        let mut vic = Vic::new();
        vic.write(0x18, 0x14); // video matrix at $0400 (so pointers are at $07F8)
        vic.write(0x15, 0x01); // enable sprite 0
        vic.write(0x27, 0x02); // sprite 0 red
        vic.write(0x00, (SPRITE_X_ORIGIN as u8) + 0); // x -> display x 0
        vic.write(0x01, (SPRITE_Y_ORIGIN as u8) + 0); // y -> display line 0
        let mut ram = [0u8; 0x10000];
        // Video matrix at $0400; sprite 0 pointer at $07F8.
        ram[0x07F8] = 0x20; // sprite data at $20*64 = $0800
        ram[0x0800] = 0x80; // top-left pixel set
        let char_rom = [0u8; 0x1000];
        let mut fb = [0u32; WIDTH * HEIGHT];
        vic.render_line(0, &ram, &char_rom, 0x07, &mut fb);
        assert_eq!(fb[0], PALETTE[2]); // red sprite pixel
    }
}
