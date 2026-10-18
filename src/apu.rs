//! The 2A03 APU: the frame counter, the length counters, and `$4015`/`$4017`.
//!
//! The channels' synthesis (duty timers, envelopes, sweep, LFSR, linear
//! counter) and the mixer arrive in the next tasks; this module starts with
//! everything the control plane needs.
//!
//! References: NESdev "APU Frame Counter" (steps in APU cycles, the 3-or-4
//! CPU cycle write delay, and the flag "set three times in a row"), NESdev
//! "APU Length Counter" (the table and the halt/reload edge cases), and
//! blargg's `test_roms/apu_2005/readme.txt`, which is the measured authority
//! wherever the two disagree.
//!
//! DMC is cut: `$4010-$4013` writes are dropped and `$4015` bits 4 and 7
//! always read 0.

/// The length counter table: `$4003` bits 7-3 index it directly (all 32
/// entries; NESdev "APU Length Counter").
const LENGTH_TABLE: [u8; 32] = [
    10, 254, 20, 2, 40, 4, 80, 6, 160, 8, 60, 10, 14, 12, 26, 14, 12, 16, 24, 18, 48, 20, 96, 22,
    192, 24, 72, 26, 16, 28, 32, 30,
];

/// CPU cycles after power-on or reset before the implied `$4017 = $00` write
/// takes effect: blargg measured "as if $00 were written to $4017, then a 9-12
/// clock delay, then execution" (`apu_reset`). Provisional; `09.reset_timing`
/// and `4017_timing` are the arbiters.
const RESET_DELAY: u8 = 12;

/// The frame counter's output for one CPU cycle.
#[derive(Clone, Copy, Default)]
struct Clocks {
    quarter: bool,
    half: bool,
}

/// The frame counter: mode, the interrupt flag, and the step schedule.
struct Frame {
    /// 0 = 4-step freerun, 1 = 5-step (bit 7 of `$4017`).
    mode: u8,
    /// Bit 6 of `$4017`: no flag is raised while set.
    inhibit: bool,
    flag: bool,
    /// APU cycles since the counter last reset; the steps are the wiki's
    /// 3728 / 7456 / 11185 / 14914 (mode 0) and 18640 / 18641 (mode 1).
    cycle: u32,
    /// True on the PUT half of the current APU cycle.
    phase: bool,
    /// CPU cycles until a pending `$4017` write takes effect.
    reset_in: u8,
    pending_mode: u8,
    /// The last value written to `$4017`, reapplied on reset.
    last_write: u8,
    /// Quarter/half signals raised by a write, delivered next CPU cycle.
    quarter: bool,
    half: bool,
    /// The interrupt flag is still being re-raised for this many CPU cycles.
    pulse: u8,
}

impl Frame {
    fn new() -> Frame {
        Frame {
            mode: 0,
            inhibit: false,
            flag: false,
            cycle: 0,
            phase: false,
            reset_in: 0,
            pending_mode: 0,
            last_write: 0,
            quarter: false,
            half: false,
            pulse: 0,
        }
    }

    fn set_flag(&mut self) {
        if !self.inhibit {
            self.flag = true;
        }
    }

    /// A `$4017` write. "If the write occurs during an APU cycle, the effects
    /// occur 3 CPU cycles after the write cycle, and if the write occurs
    /// between APU cycles, 4" -- the 3/4 split is the even/odd jitter "changes
    /// to the mode only occur on even internal APU clocks".
    fn write(&mut self, val: u8) {
        let delay = if self.phase { 4 } else { 3 };
        self.schedule(val, delay);
    }

    fn schedule(&mut self, val: u8, delay: u8) {
        self.last_write = val;
        self.inhibit = val & 0x40 != 0;
        if self.inhibit {
            // "Cleared only when $4015 is read or $4017 is written with bit 6
            // set."
            self.flag = false;
        }
        if val & 0x80 != 0 {
            // "When the write occurs with bit 7 set, both Quarter frame and
            // Half frame clocks are also generated" (blargg: one clock later).
            self.quarter = true;
            self.half = true;
        }
        self.pending_mode = val >> 7;
        self.reset_in = delay;
    }

    /// One CPU cycle: toggles the APU clock phase, advances the counter on
    /// GET halves, fires the step clocks on PUT halves.
    fn tick(&mut self) -> Clocks {
        // "The frame interrupt flag is set three times in a row": the clock
        // that raises it (below), plus these two.
        if self.pulse > 0 {
            self.pulse -= 1;
            self.set_flag();
        }

        let mut clocks = Clocks {
            quarter: self.quarter,
            half: self.half,
        };
        self.quarter = false;
        self.half = false;

        self.phase = !self.phase;
        if self.reset_in > 0 {
            self.reset_in -= 1;
            if self.reset_in == 0 {
                self.cycle = 0;
                self.mode = self.pending_mode;
            }
        } else if self.phase {
            // The PUT half of APU cycle `cycle`: where the wiki's steps sit.
            match (self.mode, self.cycle) {
                (_, 3728) | (_, 11185) => clocks.quarter = true,
                (_, 7456) | (0, 14914) | (1, 18640) => {
                    clocks.quarter = true;
                    clocks.half = true;
                }
                _ => {}
            }
            if self.mode == 0 && self.cycle == 14914 {
                self.set_flag();
                self.pulse = 2;
            }
        } else {
            self.cycle += 1;
            match (self.mode, self.cycle) {
                (0, 14915) | (1, 18641) => self.cycle = 0,
                _ => {}
            }
        }
        clocks
    }
}

/// The length counter every channel owns.
#[derive(Clone, Copy, Default)]
struct LengthCounter {
    /// Set by `$4015`; a disabled channel's counter is held at zero. Every
    /// channel starts enabled at power and reset (blargg, `apu_reset`).
    enabled: bool,
    /// The halt flag as written. The clocking circuits see it one CPU clock
    /// later, through `halt_delayed`.
    halt: bool,
    halt_delayed: bool,
    length: u8,
    reload_value: u8,
    reload_pending: bool,
    /// The reload flag was set during the clocking clock just now.
    reload_recent: bool,
}

impl LengthCounter {
    fn write_length(&mut self, val: u8) {
        self.reload_value = val >> 3;
        self.reload_pending = true;
        self.reload_recent = true;
    }

    /// One half-frame clock. blargg's four edge cases:
    /// - a reload written in the clocking clock is dropped when the counter
    ///   is live (the clock still runs);
    /// - a reload written between clocks lands, and the clock still runs,
    ///   because the "clocked while already zero" check uses the value from
    ///   before the reload;
    /// - with a zero counter the reload wins and nothing clocks;
    /// - a halt write in the clocking clock does not stop that clock.
    fn clock(&mut self) {
        let was_zero = self.length == 0;
        let dropped = self.reload_recent && !was_zero;
        if self.reload_pending && self.enabled && !dropped {
            self.length = LENGTH_TABLE[self.reload_value as usize];
        }
        if !was_zero && !self.halt_delayed {
            self.length -= 1;
        }
        self.reload_pending = false;
    }

    /// One CPU clock of bookkeeping: the halt flag arrives one clock late and
    /// a write's "recent" mark lasts one clock.
    fn tick(&mut self) {
        self.halt_delayed = self.halt;
        self.reload_recent = false;
    }
}

/// One pulse channel. Synthesis (duty timer, envelope, sweep) joins the
/// length counter in the next task.
#[derive(Clone, Copy, Default)]
struct Pulse {
    len: LengthCounter,
}

impl Pulse {
    fn write_ctrl(&mut self, val: u8) {
        self.len.halt = val & 0x20 != 0;
    }

    fn write_length(&mut self, val: u8) {
        self.len.write_length(val);
    }
}

/// The triangle channel.
#[derive(Clone, Copy, Default)]
struct Triangle {
    len: LengthCounter,
}

impl Triangle {
    fn write_ctrl(&mut self, val: u8) {
        // The control flag is the length counter halt for this channel.
        self.len.halt = val & 0x80 != 0;
    }

    fn write_length(&mut self, val: u8) {
        self.len.write_length(val);
    }
}

/// The noise channel.
#[derive(Clone, Copy, Default)]
struct Noise {
    len: LengthCounter,
}

impl Noise {
    fn write_ctrl(&mut self, val: u8) {
        self.len.halt = val & 0x20 != 0;
    }

    fn write_length(&mut self, val: u8) {
        self.len.write_length(val);
    }
}

pub struct Apu {
    pulses: [Pulse; 2],
    triangle: Triangle,
    noise: Noise,
    frame: Frame,
}

impl Apu {
    pub fn new() -> Apu {
        let mut apu = Apu {
            pulses: [Pulse::default(); 2],
            triangle: Triangle::default(),
            noise: Noise::default(),
            frame: Frame::new(),
        };
        apu.power_on();
        apu
    }

    /// Power-on: the counter behaves "as if $00 were written to $4017" a few
    /// clocks before the first instruction, and every length counter starts
    /// enabled.
    pub fn power_on(&mut self) {
        self.restart(0x00);
    }

    /// The RESET button: RAM survives, the counter restarts, and `$4017` is
    /// re-written with its last value (both measured by the apu_reset suite).
    pub fn reset(&mut self) {
        let last = self.frame.last_write;
        self.restart(last);
    }

    fn restart(&mut self, val: u8) {
        self.frame = Frame::new();
        self.frame.schedule(val, RESET_DELAY);
        for p in &mut self.pulses {
            p.len = LengthCounter {
                enabled: true,
                ..LengthCounter::default()
            };
        }
        self.triangle.len = LengthCounter {
            enabled: true,
            ..LengthCounter::default()
        };
        self.noise.len = LengthCounter {
            enabled: true,
            ..LengthCounter::default()
        };
    }

    /// Sample rate for the mixer; the mixer itself arrives with the channels.
    pub fn set_sample_rate(&mut self, _hz: u32) {}

    /// One CPU cycle.
    pub fn tick(&mut self) {
        let clocks = self.frame.tick();
        if clocks.quarter {
            // Envelopes and the triangle's linear counter clock here; they
            // arrive with their channels.
        }
        if clocks.half {
            for p in &mut self.pulses {
                p.len.clock();
            }
            self.triangle.len.clock();
            self.noise.len.clock();
        }
        for p in &mut self.pulses {
            p.len.tick();
        }
        self.triangle.len.tick();
        self.noise.len.tick();
    }

    pub fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x4015 => {
                let mut v = 0;
                for (i, p) in self.pulses.iter().enumerate() {
                    if p.len.length > 0 {
                        v |= 1 << i;
                    }
                }
                if self.triangle.len.length > 0 {
                    v |= 0x04;
                }
                if self.noise.len.length > 0 {
                    v |= 0x08;
                }
                if self.frame.flag {
                    v |= 0x40;
                    self.frame.flag = false;
                }
                v
            }
            _ => 0,
        }
    }

    pub fn cpu_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x4000 => self.pulses[0].write_ctrl(val),
            0x4003 => self.pulses[0].write_length(val),
            0x4004 => self.pulses[1].write_ctrl(val),
            0x4007 => self.pulses[1].write_length(val),
            0x4008 => self.triangle.write_ctrl(val),
            0x400B => self.triangle.write_length(val),
            0x400C => self.noise.write_ctrl(val),
            0x400F => self.noise.write_length(val),
            0x4015 => self.write_status(val),
            0x4017 => self.frame.write(val),
            // $4010-$4013 are the DMC, which is cut. The timer registers
            // ($4001-$4002 and friends) land with their channels.
            _ => {}
        }
    }

    fn write_status(&mut self, val: u8) {
        for (i, p) in self.pulses.iter_mut().enumerate() {
            p.len.enabled = val & (1 << i) != 0;
            if !p.len.enabled {
                p.len.length = 0;
            }
        }
        self.triangle.len.enabled = val & 0x04 != 0;
        if !self.triangle.len.enabled {
            self.triangle.len.length = 0;
        }
        self.noise.len.enabled = val & 0x08 != 0;
        if !self.noise.len.enabled {
            self.noise.len.length = 0;
        }
    }

    pub fn irq_pending(&self) -> bool {
        self.frame.flag
    }

    pub fn take_samples(&mut self) -> Vec<i16> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// blargg's readme, mode 0, working backwards: a `$00` write takes effect
    /// 3 CPU cycles later and the step clocks land on the PUT halves of APU
    /// cycles 7456 and 14914 -- CPU 14915 and 29831 after the write.
    #[test]
    fn the_frame_counter_clocks_length_only_on_the_steps_that_do() {
        let mut apu = Apu::new();
        apu.cpu_write(0x4015, 0x01); // enable pulse 1
        apu.cpu_write(0x4003, 0xF0); // length index 30 -> 32, on a clock
        assert_eq!(
            apu.pulses[0].len.length, 0,
            "the load waits for a half frame"
        );

        apu.cpu_write(0x4017, 0x00); // begin mode 0
        for _ in 0..10_000 {
            apu.tick();
        }
        assert_eq!(apu.pulses[0].len.length, 0, "no half frame before 14915");
        for _ in 0..10_000 {
            apu.tick();
        }
        assert_eq!(
            apu.pulses[0].len.length, 32,
            "the first half frame reloads and does not clock"
        );
        for _ in 0..10_000 {
            apu.tick();
        }
        assert_eq!(
            apu.pulses[0].len.length, 31,
            "the fourth step clocks the length too"
        );
    }

    #[test]
    fn a_4015_read_clears_the_frame_flag_and_a_4017_inhibit_write_clears_it_too() {
        let mut apu = Apu::new();
        apu.cpu_write(0x4017, 0x00);
        for _ in 0..29_831 {
            apu.tick();
        }
        assert!(apu.irq_pending(), "the fourth mode-0 step raises the flag");
        assert_eq!(apu.cpu_read(0x4015) & 0x40, 0x40, "bit 6 reports it");

        // "The frame IRQ flag is set three times in a row": clearing it while
        // the pulse runs just makes the next clock raise it again.
        apu.cpu_read(0x4015);
        apu.tick();
        assert!(apu.irq_pending(), "set again on the next clock");
        apu.cpu_read(0x4015);
        apu.tick();
        assert!(apu.irq_pending(), "and a third time");
        apu.cpu_read(0x4015);
        apu.tick();
        assert!(!apu.irq_pending(), "then it stays clear");

        // "Cleared only when $4015 is read or $4017 is written with bit 6 set."
        apu.cpu_write(0x4017, 0x00);
        while !apu.irq_pending() {
            apu.tick();
        }
        apu.cpu_write(0x4017, 0x40); // inhibit write clears it
        assert!(!apu.irq_pending());
        apu.cpu_write(0x4017, 0x00);
        while !apu.irq_pending() {
            apu.tick();
        }
        apu.cpu_write(0x4017, 0x00); // $00 does not clear it
        assert!(apu.irq_pending());
    }

    /// "Changes to the mode by writing to $4017 only occur on even internal
    /// APU clocks; if written on an odd clock, the first step of the mode is
    /// delayed by one clock."
    #[test]
    fn the_4017_write_takes_effect_on_even_clocks_or_delays_one() {
        fn ticks_to_first_flag(pre: usize) -> usize {
            let mut apu = Apu::new();
            for _ in 0..pre {
                apu.tick();
            }
            apu.cpu_write(0x4017, 0x00);
            let mut n = 0;
            while !apu.irq_pending() {
                apu.tick();
                n += 1;
            }
            n
        }
        assert_eq!(
            ticks_to_first_flag(0),
            29_831,
            "an even clock starts the mode immediately"
        );
        assert_eq!(
            ticks_to_first_flag(1),
            29_832,
            "an odd clock delays the first step by one"
        );
    }

    /// blargg's four halt/reload edge cases, as unit tests of the counters.
    #[test]
    fn length_halt_and_reload_edges_follow_the_readme() {
        // The clocking circuits see the halt flag one CPU clock late: a halt
        // write in the clocking clock does not stop it, the next one does.
        let mut p = Pulse::default();
        p.len.enabled = true;
        p.len.length = 32;
        p.write_ctrl(0x30); // halt set...
        p.len.clock(); // ...but the clock fires anyway
        assert_eq!(p.len.length, 31, "a same-clock halt write does not stop the clock");
        p.len.halt_delayed = p.len.halt;
        p.len.clock();
        assert_eq!(p.len.length, 31, "the next clock sees the halt");

        // A reload written in the clocking clock is dropped when the counter
        // is live; a reload written between clocks lands (and still clocks).
        let mut p = Pulse::default();
        p.len.enabled = true;
        p.len.length = 100;
        p.write_length(0xF0); // index 30 -> 32
        p.len.reload_recent = true;
        p.len.clock();
        assert_eq!(p.len.length, 99, "reload dropped, the counter still clocked");
        p.write_length(0xF0);
        p.len.reload_recent = false;
        p.len.clock();
        assert_eq!(p.len.length, 31, "a write between clocks reloads, then clocks");

        // With a zero counter the reload wins even in the clocking clock, and
        // the clock does not run: "clocked while already zero" silences.
        p.len.length = 0;
        p.write_length(0xF0);
        p.len.reload_recent = true;
        p.len.clock();
        assert_eq!(
            p.len.length, 32,
            "reloaded, and not clocked because it was zero"
        );
    }
}
