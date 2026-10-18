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

/// The duty sequencer tables, stepped from bit 7 down to bit 0 (NESdev "APU
/// Pulse", "Duty Cycle Sequences"). Bit 7 is step 0, which is why every
/// waveform starts at 0.
const DUTY_SEQUENCES: [u8; 4] = [0x40, 0x60, 0x78, 0x9F];

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

/// The envelope every pulse and noise channel owns (NESdev "APU Envelope").
#[derive(Clone, Copy, Default)]
struct Envelope {
    /// A settings write sets this; the next quarter-frame clock honors it.
    start: bool,
    /// `$400x` bit 5, shared with the length counter's halt.
    looping: bool,
    /// `$400x` bit 4: output the volume field directly.
    constant: bool,
    /// `$400x` bits 3-0: the divider reload, or the constant volume.
    volume: u8,
    divider: u8,
    decay: u8,
}

impl Envelope {
    fn write(&mut self, val: u8) {
        self.volume = val & 0x0F;
        self.constant = val & 0x10 != 0;
        self.looping = val & 0x20 != 0;
        self.start = true;
    }

    /// One quarter-frame clock.
    fn clock(&mut self) {
        if self.start {
            self.start = false;
            self.decay = 15;
            self.divider = self.volume;
        } else if self.divider == 0 {
            self.divider = self.volume;
            if self.decay > 0 {
                self.decay -= 1;
            } else if self.looping {
                self.decay = 15;
            }
        } else {
            self.divider -= 1;
        }
    }

    fn output(&self) -> u8 {
        if self.constant {
            self.volume
        } else {
            self.decay
        }
    }
}

/// The sweep unit (NESdev "APU Pulse", "Sweep unit operation").
#[derive(Clone, Copy, Default)]
struct Sweep {
    enable: bool,
    period: u8,
    negate: bool,
    shift: u8,
    divider: u8,
    /// A `$4001` write schedules a divider reload for the next half frame.
    reload: bool,
    /// Pulse 1 adds the ones' complement (-c - 1), pulse 2 the two's
    /// complement (-c); the channels' adder carry inputs differ.
    ones_complement: bool,
}

impl Sweep {
    fn write(&mut self, val: u8) {
        self.enable = val & 0x80 != 0;
        self.period = (val >> 4) & 0x07;
        self.negate = val & 0x08 != 0;
        self.shift = val & 0x07;
        self.reload = true;
    }

    /// The target period for the current timer value, clamped to zero. It is
    /// derived on demand, which is the same as "recalculated whenever the
    /// current period changes".
    fn target(&self, timer: u16) -> i32 {
        let change = (timer >> self.shift) as i32;
        let target = if self.negate {
            timer as i32 - change - i32::from(self.ones_complement)
        } else {
            timer as i32 + change
        };
        target.max(0)
    }

    /// "If the target period is larger than $7FF, it silences the channel",
    /// and that holds regardless of the enable flag.
    fn muted(&self, timer: u16) -> bool {
        self.target(timer) > 0x7FF
    }

    /// One half-frame clock.
    fn clock(&mut self, timer: &mut u16) {
        if self.divider == 0 && self.enable && self.shift != 0 && !self.muted(*timer) {
            *timer = self.target(*timer) as u16;
        }
        if self.divider == 0 || self.reload {
            self.divider = self.period;
            self.reload = false;
        } else {
            self.divider -= 1;
        }
    }
}

/// One pulse channel: timer and sequencer, envelope, sweep, length counter.
#[derive(Clone, Copy, Default)]
struct Pulse {
    len: LengthCounter,
    duty: u8,
    /// The sequencer's current output; `output()` gates on it.
    duty_bit: bool,
    sequence: u8,
    timer: u16,
    divider: u16,
    envelope: Envelope,
    sweep: Sweep,
}

impl Pulse {
    fn new(ones_complement: bool) -> Pulse {
        Pulse {
            sweep: Sweep {
                ones_complement,
                ..Sweep::default()
            },
            ..Pulse::default()
        }
    }

    fn write_ctrl(&mut self, val: u8) {
        self.duty = val >> 6;
        self.len.halt = val & 0x20 != 0;
        self.envelope.write(val & 0x1F);
    }

    fn write_sweep(&mut self, val: u8) {
        self.sweep.write(val);
    }

    fn write_timer_low(&mut self, val: u8) {
        self.timer = (self.timer & 0x700) | val as u16;
    }

    fn write_length(&mut self, val: u8) {
        self.timer = (self.timer & 0xFF) | ((val as u16 & 0x07) << 8);
        self.len.write_length(val);
        // "Writing to this register also resets the sequencer index (i.e.,
        // the pulse phase) to the first value, restart envelope."
        self.sequence = 0;
        self.duty_bit = false; // every duty waveform starts at 0
        self.envelope.start = true;
    }

    /// One APU clock: the timer counts t down to 0, then steps the sequencer.
    fn tick(&mut self) {
        if self.divider == 0 {
            self.divider = self.timer;
            self.duty_bit = (DUTY_SEQUENCES[self.duty as usize] >> (7 - self.sequence)) & 1 != 0;
            self.sequence = (self.sequence + 1) & 7;
        } else {
            self.divider -= 1;
        }
    }

    /// The value this channel contributes to the mixer, 0-15.
    fn output(&self) -> u8 {
        if self.len.length == 0 || self.timer < 8 || !self.duty_bit || self.sweep.muted(self.timer)
        {
            0
        } else {
            self.envelope.output()
        }
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
    /// The APU clock: channels' timers advance on every other CPU cycle.
    apu_phase: bool,
}

impl Apu {
    pub fn new() -> Apu {
        let mut apu = Apu {
            pulses: [Pulse::new(true), Pulse::new(false)],
            triangle: Triangle::default(),
            noise: Noise::default(),
            frame: Frame::new(),
            apu_phase: false,
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
        self.pulses = [Pulse::new(true), Pulse::new(false)];
        for p in &mut self.pulses {
            p.len.enabled = true;
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
        // The channels' timers run at half the CPU rate.
        self.apu_phase = !self.apu_phase;
        if self.apu_phase {
            for p in &mut self.pulses {
                p.tick();
            }
        }

        let clocks = self.frame.tick();
        if clocks.quarter {
            for p in &mut self.pulses {
                p.envelope.clock();
            }
            // The triangle's linear counter and the noise envelope clock with
            // their channels.
        }
        if clocks.half {
            // Sweep first, then the length counters (the wiki's half-frame
            // order).
            for p in &mut self.pulses {
                p.sweep.clock(&mut p.timer);
            }
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
            0x4001 => self.pulses[0].write_sweep(val),
            0x4002 => self.pulses[0].write_timer_low(val),
            0x4003 => self.pulses[0].write_length(val),
            0x4004 => self.pulses[1].write_ctrl(val),
            0x4005 => self.pulses[1].write_sweep(val),
            0x4006 => self.pulses[1].write_timer_low(val),
            0x4007 => self.pulses[1].write_length(val),
            0x4008 => self.triangle.write_ctrl(val),
            0x400B => self.triangle.write_length(val),
            0x400C => self.noise.write_ctrl(val),
            0x400F => self.noise.write_length(val),
            0x4015 => self.write_status(val),
            0x4017 => self.frame.write(val),
            // $4010-$4013 are the DMC, which is cut. The triangle's and the
            // noise's remaining registers land with their channels.
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
        assert_eq!(
            p.len.length, 31,
            "a same-clock halt write does not stop the clock"
        );
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
        assert_eq!(
            p.len.length, 99,
            "reload dropped, the counter still clocked"
        );
        p.write_length(0xF0);
        p.len.reload_recent = false;
        p.len.clock();
        assert_eq!(
            p.len.length, 31,
            "a write between clocks reloads, then clocks"
        );

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

    /// The four digitized waveforms (NESdev "APU Pulse"), stepped once per
    /// timer period. Constant volume 15 makes the output the waveform itself.
    #[test]
    fn duty_sequences_match_the_four_standard_patterns() {
        let expected: [[u8; 8]; 4] = [
            [0, 1, 0, 0, 0, 0, 0, 0],
            [0, 1, 1, 0, 0, 0, 0, 0],
            [0, 1, 1, 1, 1, 0, 0, 0],
            [1, 0, 0, 1, 1, 1, 1, 1],
        ];
        for (duty, wave) in expected.iter().enumerate() {
            let mut apu = Apu::new();
            apu.pulses[0].len.length = 200; // active; the load is measured elsewhere
            apu.cpu_write(0x4000, 0x1F | (duty as u8) << 6); // constant volume 15
            apu.cpu_write(0x4003, 0x00); // resets the sequencer to step 0
                                         // t = 8: any t < 8 would silence the channel by spec, and 8 keeps
                                         // the period short.
            apu.cpu_write(0x4002, 0x08);
            apu.pulses[0].divider = 0;
            let mut samples = Vec::new();
            let mut seq = None;
            while samples.len() < 8 {
                apu.tick();
                let now = apu.pulses[0].sequence;
                if seq != Some(now) {
                    seq = Some(now);
                    samples.push(apu.pulses[0].output());
                }
            }
            let want: Vec<u8> = wave.iter().map(|&bit| bit * 15).collect();
            assert_eq!(samples, want, "duty {duty}");
        }
    }

    /// "The period of the timer is t+1 APU cycles": with t = 1 a step
    /// boundary lands every four CPU cycles.
    #[test]
    fn the_timer_period_is_t_plus_one_apu_cycles_per_step() {
        let mut apu = Apu::new();
        apu.pulses[0].len.length = 200;
        apu.cpu_write(0x4000, 0x9F); // duty 2, constant volume 15
        apu.cpu_write(0x4002, 0x01); // t = 1
        apu.cpu_write(0x4003, 0x00);
        apu.pulses[0].divider = 0;
        let mut boundaries = Vec::new();
        let mut seq = None;
        for tick in 0..64 {
            apu.tick();
            let now = apu.pulses[0].sequence;
            if seq.is_some() && now != seq.unwrap() {
                boundaries.push(tick);
            }
            seq = Some(now);
        }
        let gaps: Vec<i64> = boundaries.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps.len() >= 4, "the sequencer must keep stepping");
        assert!(
            gaps.iter().all(|&g| g == 4),
            "(t+1) APU cycles = 4 CPU cycles per step: {gaps:?}"
        );
    }

    /// "If the target period is larger than $7FF, it silences the channel"
    /// -- enable or no -- and the wiki's own recipe for turning the sweep
    /// off (write $08: negate, disabled) brings it back.
    #[test]
    fn a_sweep_that_overflows_mutes_the_channel() {
        let mut apu = Apu::new();
        apu.pulses[0].len.length = 200;
        apu.cpu_write(0x4000, 0x1F); // duty 0, constant volume 15
        apu.cpu_write(0x4002, 0xFF);
        apu.cpu_write(0x4003, 0x07); // t = $7FF
        apu.cpu_write(0x4001, 0x81); // enable, add, shift 1: target $BFE
        apu.pulses[0].duty_bit = true; // duty 0's one "on" step
        assert_eq!(apu.pulses[0].output(), 0, "target $BFE overflows $7FF");
        apu.cpu_write(0x4001, 0x01); // even disabled, the adder still mutes
        assert_eq!(
            apu.pulses[0].output(),
            0,
            "muting does not need the enable bit"
        );
        apu.cpu_write(0x4001, 0x08); // negate with shift 0: target clamps to 0
        assert_eq!(
            apu.pulses[0].output(),
            15,
            "back in range, the channel sounds"
        );
    }

    /// The envelope walks down one step every V+1 quarter frames, the loop
    /// flag wraps it 0 -> 15, the constant flag overrides the walk, and
    /// without the loop it freezes at zero.
    #[test]
    fn the_envelope_decays_and_the_loop_flag_wraps_it() {
        let mut e = Envelope::default();
        e.write(0x08); // period 8, envelope mode, no loop, not constant
        assert!(e.start, "a write starts the envelope");
        e.clock();
        assert_eq!(e.decay, 15);
        assert!(!e.start);
        for _ in 0..8 {
            e.clock();
        }
        assert_eq!(e.decay, 15, "the divider has not wrapped yet");
        e.clock();
        assert_eq!(e.decay, 14, "one step per V+1 quarter frames");

        e.decay = 0;
        e.divider = 0;
        e.looping = true;
        e.clock();
        assert_eq!(e.decay, 15, "the loop flag wraps");
        e.looping = false;
        e.decay = 0;
        e.divider = 0;
        e.clock();
        assert_eq!(e.decay, 0, "without the loop it freezes at zero");

        let mut c = Envelope::default();
        c.write(0x1F); // constant volume 15
        c.decay = 3;
        assert_eq!(c.output(), 15, "constant volume overrides the decay");
        let mut d = Envelope::default();
        d.write(0x08);
        d.decay = 7;
        assert_eq!(d.output(), 7, "otherwise the output is the decay level");
    }
}
