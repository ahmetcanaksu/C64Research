//! MOS 6581/8580 SID — the C64's 3-voice sound chip at $D400.
//!
//! Each of the three voices is a phase-accumulator oscillator (triangle,
//! sawtooth, pulse, noise) shaped by an ADSR envelope; the three are mixed and
//! scaled by the master volume. Clock it alongside the CPU with [`Sid::clock`],
//! then read the mixed output with [`Sid::output`] at your audio sample rate.
//!
//! This is a recognizable, compact SID — good enough to hear tunes and effects.
//! It is not cycle-exact like reSID, and the analog filter ($D415-$D417) is not
//! modeled yet (voices pass through unfiltered). `no_std`, MCU-ready.

#![no_std]

/// PAL SID master clock (Hz). NTSC is 1_022_727.
pub const CLOCK_PAL: u32 = 985_248;

/// reSID envelope rate-counter periods, indexed by the 4-bit attack/decay/
/// release value. Number of SID clocks between envelope steps.
const RATE_PERIODS: [u16; 16] = [
    9, 32, 63, 95, 149, 220, 267, 313, 392, 977, 1954, 3126, 3907, 11720, 19532, 31251,
];

#[derive(Clone, Copy, PartialEq)]
enum EnvState {
    Attack,
    Decay,
    Sustain,
    Release,
}

/// The ADSR envelope generator for one voice.
#[derive(Clone)]
struct Envelope {
    state: EnvState,
    value: u8,   // current level 0..255
    rate: u16,   // counts up to RATE_PERIODS[selected]
    exp: u8,     // exponential sub-counter for decay/release curves
    exp_period: u8,
    attack: u8,  // 4-bit rate selectors
    decay: u8,
    sustain: u8, // 0..15
    release: u8,
    gate: bool,
}

impl Envelope {
    fn new() -> Self {
        Envelope {
            state: EnvState::Release,
            value: 0,
            rate: 0,
            exp: 0,
            exp_period: 1,
            attack: 0,
            decay: 0,
            sustain: 0,
            release: 0,
            gate: false,
        }
    }

    fn set_gate(&mut self, on: bool) {
        if on && !self.gate {
            self.state = EnvState::Attack;
            self.exp_period = 1; // attack is linear
        } else if !on && self.gate {
            self.state = EnvState::Release;
            self.update_exp_period();
        }
        self.gate = on;
    }

    fn selected_rate(&self) -> u8 {
        match self.state {
            EnvState::Attack => self.attack,
            EnvState::Decay => self.decay,
            EnvState::Release => self.release,
            EnvState::Sustain => self.release, // idle
        }
    }

    /// The exponential-decay approximation: as the level falls, steps get rarer,
    /// producing the SID's characteristic curved decay/release.
    fn update_exp_period(&mut self) {
        self.exp_period = match self.value {
            255 => 1,
            93 => 2,
            54 => 4,
            26 => 8,
            14 => 16,
            6 => 30,
            0 => 1,
            _ => self.exp_period,
        };
    }

    fn clock(&mut self) {
        if self.state == EnvState::Sustain {
            return;
        }
        self.rate += 1;
        if self.rate < RATE_PERIODS[self.selected_rate() as usize] {
            return;
        }
        self.rate = 0;

        // Attack rises linearly; decay/release fall on the exponential schedule.
        if self.state != EnvState::Attack {
            self.exp += 1;
            if self.exp < self.exp_period {
                return;
            }
            self.exp = 0;
        }

        match self.state {
            EnvState::Attack => {
                self.value = self.value.saturating_add(1);
                if self.value == 0xFF {
                    self.state = EnvState::Decay;
                    self.update_exp_period();
                }
            }
            EnvState::Decay => {
                let sustain_level = self.sustain * 17; // 0..255
                if self.value > sustain_level {
                    self.value -= 1;
                    self.update_exp_period();
                } else {
                    self.state = EnvState::Sustain;
                }
            }
            EnvState::Release => {
                if self.value > 0 {
                    self.value -= 1;
                    self.update_exp_period();
                }
            }
            EnvState::Sustain => {}
        }
    }
}

/// One SID voice: oscillator + waveform selector + envelope.
#[derive(Clone)]
struct Voice {
    freq: u16,
    pulse_width: u16, // 12-bit
    control: u8,      // bit0 gate, bit1 sync, bit2 ring, bit3 test, bits4-7 waveform
    phase: u32,       // 24-bit accumulator
    noise: u32,       // 23-bit LFSR
    env: Envelope,
}

impl Voice {
    fn new() -> Self {
        Voice {
            freq: 0,
            pulse_width: 0,
            control: 0,
            phase: 0,
            noise: 0x7FFFFF,
            env: Envelope::new(),
        }
    }

    fn clock(&mut self) {
        if self.control & 0x08 != 0 {
            // Test bit held: oscillator reset.
            self.phase = 0;
        } else {
            let old = self.phase;
            self.phase = (self.phase + self.freq as u32) & 0x00FF_FFFF;
            // Clock the noise LFSR when accumulator bit 19 goes high.
            if (old & 0x0008_0000) == 0 && (self.phase & 0x0008_0000) != 0 {
                let bit = ((self.noise >> 22) ^ (self.noise >> 17)) & 1;
                self.noise = ((self.noise << 1) | bit) & 0x007F_FFFF;
            }
        }
        self.env.clock();
    }

    /// 12-bit oscillator output before the envelope.
    fn waveform(&self) -> u16 {
        let wf = self.control >> 4;
        let mut out = 0xFFFu16;
        if wf & 0x01 != 0 {
            // Triangle.
            let msb = if self.phase & 0x0080_0000 != 0 { 0xFFF } else { 0 };
            out &= (((self.phase >> 11) as u16) ^ msb) & 0xFFF;
        }
        if wf & 0x02 != 0 {
            // Sawtooth.
            out &= (self.phase >> 12) as u16 & 0xFFF;
        }
        if wf & 0x04 != 0 {
            // Pulse.
            let p = (self.phase >> 12) as u16 & 0xFFF;
            out &= if p >= self.pulse_width { 0xFFF } else { 0x000 };
        }
        if wf & 0x08 != 0 {
            // Noise: 8 bits picked from the LFSR, spread across the 12-bit range.
            let n = self.noise;
            let b = ((n >> 22) & 1) << 11
                | ((n >> 20) & 1) << 10
                | ((n >> 16) & 1) << 9
                | ((n >> 13) & 1) << 8
                | ((n >> 11) & 1) << 7
                | ((n >> 7) & 1) << 6
                | ((n >> 4) & 1) << 5
                | ((n >> 2) & 1) << 4;
            out &= b as u16;
        }
        if wf == 0 {
            out = 0x000;
        }
        out
    }

    /// Envelope-shaped, centered sample for this voice (~-2048..2047).
    fn output(&self) -> i32 {
        let centered = self.waveform() as i32 - 0x800;
        centered * self.env.value as i32 / 256
    }
}

/// The SID chip.
#[derive(Clone)]
pub struct Sid {
    voices: [Voice; 3],
    volume: u8, // master volume, $D418 low nibble (0..15)
}

impl Default for Sid {
    fn default() -> Self {
        Sid { voices: [Voice::new(), Voice::new(), Voice::new()], volume: 0 }
    }
}

impl Sid {
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance every voice by `cycles` SID clocks.
    pub fn clock(&mut self, cycles: u32) {
        for _ in 0..cycles {
            for v in &mut self.voices {
                v.clock();
            }
        }
    }

    /// The current mixed output as a float in roughly `-1.0..1.0`.
    pub fn output(&self) -> f32 {
        let mix: i32 = self.voices.iter().map(|v| v.output()).sum();
        // 3 voices * ~2048 range, scaled by master volume (0..15).
        let scaled = mix * self.volume as i32;
        scaled as f32 / (3.0 * 2048.0 * 15.0)
    }

    /// Write a SID register (offset 0x00..0x1C).
    pub fn write(&mut self, reg: u8, val: u8) {
        let reg = reg & 0x1F;
        let voice = (reg / 7) as usize; // regs 0-6 v1, 7-13 v2, 14-20 v3
        if voice < 3 {
            let v = &mut self.voices[voice];
            match reg % 7 {
                0 => v.freq = (v.freq & 0xFF00) | val as u16,
                1 => v.freq = (v.freq & 0x00FF) | ((val as u16) << 8),
                2 => v.pulse_width = (v.pulse_width & 0x0F00) | val as u16,
                3 => v.pulse_width = (v.pulse_width & 0x00FF) | (((val & 0x0F) as u16) << 8),
                4 => {
                    v.control = val;
                    v.env.set_gate(val & 0x01 != 0);
                }
                5 => {
                    v.env.attack = val >> 4;
                    v.env.decay = val & 0x0F;
                }
                6 => {
                    v.env.sustain = val >> 4;
                    v.env.release = val & 0x0F;
                }
                _ => {}
            }
            return;
        }
        // Filter / volume registers ($D415-$D418); only master volume is modeled.
        if reg == 0x18 {
            self.volume = val & 0x0F;
        }
    }

    /// Read a SID register. Most are write-only and read as open bus; the two
    /// useful reads are voice-3 oscillator ($D41B) and envelope ($D41C).
    pub fn read(&self, reg: u8) -> u8 {
        match reg & 0x1F {
            0x1B => (self.voices[2].waveform() >> 4) as u8, // OSC3 (high 8 bits)
            0x1C => self.voices[2].env.value,               // ENV3
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Voice 3's registers ($D40E-$D414) — its envelope is read back at $D41C.
    const V3_AD: u8 = 0x13;
    const V3_SR: u8 = 0x14;
    const V3_CTRL: u8 = 0x12;

    #[test]
    fn gate_runs_attack_up_to_peak() {
        let mut sid = Sid::new();
        sid.write(V3_AD, 0x00); // attack=0 (fastest), decay=0
        sid.write(V3_SR, 0xF0); // sustain=15 (full), release=0
        sid.write(V3_CTRL, 0x11); // pulse waveform + gate on
        sid.clock(5000); // let the attack run
        assert!(sid.read(0x1C) > 200, "envelope should have risen, got {}", sid.read(0x1C));
    }

    #[test]
    fn gate_off_releases_toward_zero() {
        let mut sid = Sid::new();
        sid.write(V3_AD, 0x00);
        sid.write(V3_SR, 0xF0);
        sid.write(V3_CTRL, 0x11); // gate on
        sid.clock(5000);
        sid.write(V3_CTRL, 0x10); // gate off -> release
        sid.clock(200_000);
        assert_eq!(sid.read(0x1C), 0);
    }

    #[test]
    fn silent_at_zero_volume() {
        let mut sid = Sid::new();
        sid.write(0x00, 0x00);
        sid.write(0x01, 0x10); // some frequency
        sid.write(0x04, 0x21); // sawtooth + gate
        sid.write(0x05, 0x00);
        sid.write(0x06, 0xF0);
        sid.clock(5000);
        // Master volume defaults to 0 (as IOINIT leaves it): silence.
        assert_eq!(sid.output(), 0.0);
    }

    #[test]
    fn produces_signal_when_playing() {
        let mut sid = Sid::new();
        sid.write(0x18, 0x0F); // master volume max
        sid.write(0x00, 0x00);
        sid.write(0x01, 0x20); // frequency
        sid.write(0x04, 0x21); // sawtooth + gate
        sid.write(0x05, 0x00);
        sid.write(0x06, 0xF0);
        sid.clock(5000);
        // Sample a bunch and confirm the output actually swings.
        let mut min = f32::MAX;
        let mut max = f32::MIN;
        for _ in 0..2000 {
            sid.clock(20);
            let s = sid.output();
            min = min.min(s);
            max = max.max(s);
        }
        assert!(max - min > 0.05, "expected an audible swing, got {}", max - min);
    }
}
