//! The 2A03 APU: the frame counter, the length counters, `$4015`/`$4017`, the
//! two pulse channels, the triangle, the noise channel, the DMC, and the mixer
//! that turns their outputs into samples.
//!
//! References: NESdev "APU Frame Counter" (steps in APU cycles, the 3-or-4
//! CPU cycle write delay, and the flag "set three times in a row"), NESdev
//! "APU Length Counter" (the table and the halt/reload edge cases), NESdev
//! "APU DMC" (registers, rate table, reader/output split), and blargg's
//! `test_roms/apu_2005/readme.txt`, which is the measured authority wherever
//! the two disagree.
//!
//! The DMC's memory reader is the CPU bus, so a fetch is a request this type
//! hands to `Bus`: `take_dmc_fetch` returns the address and `dmc_fetch_complete`
//! hands the byte back after the DMA. `Bus` owns the stall accounting and the
//! controller-read corruption; this file owns the sample timer, buffer,
//! shifter, level and IRQ flag.

use crate::region::Region;

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

/// The triangle's 32-step ladder: 15 down to 0, then 0 back up to 15
/// (NESdev "APU Triangle", "Sequencer operation").
const TRIANGLE_SEQUENCE: [u8; 32] = [
    15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12,
    13, 14, 15,
];

/// The noise period table in CPU cycles (NESdev "APU Noise", NTSC column).
const NOISE_PERIODS: [u16; 16] = [
    4, 8, 16, 32, 64, 96, 128, 160, 202, 254, 380, 508, 762, 1016, 2034, 4068,
];

/// The same table for the 2A07 (NESdev "APU Noise", PAL column). The 2A07's
/// CPU clock is slower, but the shift-register periods are shorter, not just
/// divided: $2 is 14 cycles against NTSC's 16, and $F is 3778 against 4068.
const PAL_NOISE_PERIODS: [u16; 16] = [
    4, 8, 14, 30, 60, 88, 118, 148, 188, 236, 354, 472, 708, 944, 1890, 3778,
];

fn noise_periods(region: Region) -> &'static [u16; 16] {
    match region {
        Region::Ntsc => &NOISE_PERIODS,
        Region::Pal => &PAL_NOISE_PERIODS,
    }
}

/// DMC playback periods in CPU cycles (NESdev "APU DMC", "Pitch table").
const DMC_PERIODS: [u16; 16] = [
    428, 380, 340, 320, 286, 254, 226, 214, 190, 160, 142, 128, 106, 84, 72, 54,
];

/// The 2A07's table: the same rate indices, different clock.
const PAL_DMC_PERIODS: [u16; 16] = [
    398, 354, 316, 298, 276, 236, 210, 198, 176, 148, 132, 118, 98, 78, 66, 50,
];

fn dmc_periods(region: Region) -> &'static [u16; 16] {
    match region {
        Region::Ntsc => &DMC_PERIODS,
        Region::Pal => &PAL_DMC_PERIODS,
    }
}

/// CPU cycles from a `$4015` start to the load DMA's request. NESdev "DMA"
/// schedules the halt on a get cycle during the second APU cycle after the
/// write: four cycles after a get-cycle write, three after a put-cycle write.
/// `apu_phase == true` is a get cycle.
const DMC_LOAD_DELAY_GET: u8 = 4;
const DMC_LOAD_DELAY_PUT: u8 = 3;
/// CPU cycles from the output unit emptying its buffer to the reload DMA's
/// request. The output unit clocks on a get cycle, so the next put cycle is
/// one CPU cycle later.
const DMC_RELOAD_DELAY: u8 = 1;

/// One second of stereo samples at 48 kHz: 96,000 `i16`, which is 48,000
/// stereo pairs. When the queue is full the newest pair is dropped, which
/// keeps memory bounded if no player drains it.
const SAMPLE_QUEUE_CAP: usize = 96_000;

/// How far into its frame the frame counter already is when power-on or reset
/// hands control to the first instruction, in APU clocks. blargg measured the
/// implied `$4017` write landing "9 to 12" CPU cycles before that instruction
/// and "usually 9" (`apu_reset`); a `pre` of 3, on top of the write's own
/// delay, is what `4017_timing` reports as 9.
const RESET_PRE: u32 = 3;

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
    /// Quarter/half signals raised by a write or by a step, delivered on the
    /// next CPU cycle: "each step of the sequence will occur once that total
    /// has reached the indicated amount (with an additional delay of one CPU
    /// cycle for the quarter and half frame signals)" (NESdev "APU Frame
    /// Counter"). `apu_2005`'s `05.len_timing_mode0` measures that cycle.
    quarter: bool,
    half: bool,
    /// The interrupt flag is still being re-raised for this many CPU cycles.
    pulse: u8,
    /// Which step schedule and APU-cycle count this frame counter uses.
    region: Region,
}

impl Frame {
    fn new(region: Region) -> Frame {
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
            region,
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

    /// Power-on and reset: as if `val` had been written to `$4017` before the
    /// first instruction, late enough that its mode change has already taken
    /// effect and the counter is `pre` APU clocks into its frame. blargg's
    /// apu_reset suite measures the write 9-12 CPU cycles before that first
    /// instruction and "usually 9" (readme.txt); `pre` of 3 puts this
    /// implementation on the 9 that `4017_timing` reports.
    fn start(&mut self, val: u8, pre: u32) {
        self.schedule(val, 0);
        self.mode = self.pending_mode;
        self.cycle = pre;
        self.reset_in = 0;
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

    /// Quarter, second (quarter+half), third quarter, interrupt, and the
    /// 5-step mode's last quarter+half, in APU cycles.
    fn steps(&self) -> (u32, u32, u32, u32, u32) {
        match self.region {
            Region::Ntsc => (3728, 7456, 11185, 14914, 18640),
            Region::Pal => (4156, 8313, 12469, 16626, 20782),
        }
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

        let clocks = Clocks {
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
            // "with an additional delay of one CPU cycle for the quarter and
            // half frame signals": the step raises them, the next CPU cycle
            // delivers them. The interrupt flag below is NOT delayed.
            let (first, second, third, interrupt, fifth) = self.steps();
            match (self.mode, self.cycle) {
                (_, c) if c == first || c == third => self.quarter = true,
                (_, c)
                    if c == second
                        || (self.mode == 0 && c == interrupt)
                        || (self.mode == 1 && c == fifth) =>
                {
                    self.quarter = true;
                    self.half = true;
                }
                _ => {}
            }
            // The frame IRQ flag is NOT delayed with them: the wiki's table
            // sets it on three consecutive CPU cycles, 14914 GET through
            // 14915 GET (16626 through 16627 on PAL), which is what
            // `apu_2005`'s `07.irq_flag_timing` measures.
            if self.mode == 0 && self.cycle == interrupt {
                self.set_flag();
                self.pulse = 2;
            }
        } else {
            self.cycle += 1;
            let (_, _, _, interrupt, fifth) = self.steps();
            let end = if self.mode == 0 {
                interrupt + 1
            } else {
                fifth + 1
            };
            if self.cycle == end {
                self.cycle = 0;
            }
        }
        clocks
    }
}

/// The length counter every channel owns.
#[derive(Clone, Copy, Default)]
struct LengthCounter {
    /// Set by `$4015`; a disabled channel's counter is held at zero. Power-on
    /// and reset both leave every channel disabled until `$4015` is written
    /// (blargg, `apu_reset` `4015_cleared`).
    enabled: bool,
    /// The halt flag as written, and the flag the clocking circuits see one
    /// CPU clock later. Hardware registers the write on the next APU clock and
    /// ours lands at the end of the write's own CPU cycle (see
    /// `Bus::apu_advance_ahead`), so the circuits see it two clocks later.
    halt: bool,
    halt_staged: bool,
    halt_delayed: bool,
    length: u8,
    reload_value: u8,
    /// A load waiting for the same two clocks, and whether a length clock in
    /// the CPU clock right after the write's own clock suppresses it.
    load_pending: bool,
    load_staged: bool,
    load_suppressed: bool,
    /// The counter was clocked while live in this CPU clock.
    load_ignored: bool,
}

impl LengthCounter {
    /// `$400x`: loaded straight away, not at the next half frame (NESdev
    /// "APU Length Counter": "if the enabled flag is set, the length counter is
    /// loaded with entry L of the length table"). A disabled channel cannot be
    /// loaded at all. Like the halt flag, the load lands two CPU clocks later.
    fn write_length(&mut self, val: u8) {
        self.reload_value = val >> 3;
        self.load_pending = true;
        self.load_suppressed = false;
    }

    /// One half-frame clock (NESdev "APU Length Counter"): decrement, except
    /// when the counter is zero or halted.
    fn clock(&mut self) {
        let was_zero = self.length == 0;
        if !was_zero && !self.halt_delayed {
            self.length -= 1;
        }
        self.load_ignored = !was_zero;
    }

    /// One CPU cycle of bookkeeping, before the frame counter clocks: the halt
    /// flag and any load arrive two clocks late (blargg, "Length Halt"), and
    /// the "clocked while live" mark lasts one clock.
    fn tick(&mut self) {
        self.halt_delayed = self.halt_staged;
        self.halt_staged = self.halt;
        // A length clock in the CPU clock right after the write's own clock
        // ignores the load completely, when the counter was live before it
        // (blargg, "Length Reload" -- "written during length clocking"). A
        // clock in the write's own CPU clock came first and lets the load
        // through afterwards.
        if self.load_staged && self.load_ignored {
            self.load_suppressed = true;
        }
        if self.load_staged {
            self.load_staged = false;
            if self.enabled && !self.load_suppressed {
                self.length = LENGTH_TABLE[self.reload_value as usize];
            }
        }
        self.load_staged = self.load_pending;
        self.load_pending = false;
        self.load_ignored = false;
    }

    /// Clearing `$4015`'s bit for this channel: the counter is forced to 0 and
    /// stays there until enabled again. A reset clears every bit.
    fn clear(&mut self) {
        self.enabled = false;
        self.length = 0;
        self.load_pending = false;
        self.load_staged = false;
        self.load_suppressed = false;
        self.load_ignored = false;
        self.halt_staged = self.halt;
        self.halt_delayed = self.halt;
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

/// The triangle channel: sequencer, linear counter, length counter. Its
/// timer runs at the CPU clock, twice the rate of every other channel's.
#[derive(Clone, Copy, Default)]
struct Triangle {
    len: LengthCounter,
    /// `$4008` bit 7: the linear counter control flag, shared with the
    /// length counter halt.
    control: bool,
    /// `$4008` bits 6-0.
    reload_value: u8,
    linear_reload: bool,
    linear: u8,
    timer: u16,
    divider: u16,
    /// The index of the next sequence step; the value of the step just
    /// taken lives in `value`.
    sequence: u8,
    /// The held DAC output: 15..0..15 while clocked, frozen when silenced.
    value: u8,
}

impl Triangle {
    fn write_ctrl(&mut self, val: u8) {
        self.control = val & 0x80 != 0;
        self.reload_value = val & 0x7F;
        self.len.halt = self.control;
    }

    fn write_timer_low(&mut self, val: u8) {
        self.timer = (self.timer & 0x700) | val as u16;
    }

    fn write_length(&mut self, val: u8) {
        self.timer = (self.timer & 0xFF) | ((val as u16 & 0x07) << 8);
        self.len.write_length(val);
        // "Writing to this register sets the linear counter reload flag."
        self.linear_reload = true;
    }

    /// One quarter-frame clock (NESdev "APU Triangle", "Sequencer operation",
    /// step 1): "If the linear counter reload flag is set, the counter is
    /// reloaded; otherwise, if the counter is non-zero, it is decremented.
    /// If the control flag is clear, the reload flag is cleared."
    fn clock_linear(&mut self) {
        if self.linear_reload {
            self.linear = self.reload_value;
        } else if self.linear > 0 {
            self.linear -= 1;
        }
        if !self.control {
            self.linear_reload = false;
        }
    }

    /// One CPU clock. "The triangle channel's timer is clocked at the CPU
    /// clock rate, rather than the APU clock rate"; the sequencer advances
    /// only while both counters are non-zero.
    fn tick(&mut self) {
        if self.divider == 0 {
            self.divider = self.timer;
            if self.len.length > 0 && self.linear > 0 {
                self.value = TRIANGLE_SEQUENCE[self.sequence as usize];
                self.sequence = (self.sequence + 1) & 31;
            }
        } else {
            self.divider -= 1;
        }
    }

    /// The held DAC value. There is no gate here: a silenced triangle keeps
    /// feeding its last value to the mixer, which is the audible "pop".
    fn output(&self) -> u8 {
        self.value
    }
}

/// The noise channel: the linear-feedback shift register, envelope, and
/// length counter. Its timer runs at the CPU clock.
#[derive(Clone, Copy, Default)]
struct Noise {
    len: LengthCounter,
    /// `$400E` bit 7: the periodic-noise tap.
    mode: bool,
    period: u16,
    divider: u16,
    /// 15 bits of shift register, "loaded with the value 1" at power-up.
    lfsr: u16,
    envelope: Envelope,
    /// Which CPU's period table `$400E` indexes.
    region: Region,
}

impl Noise {
    #[cfg(test)]
    fn new() -> Noise {
        Noise::for_region(Region::Ntsc)
    }

    fn for_region(region: Region) -> Noise {
        let period = noise_periods(region)[0];
        Noise {
            period,
            divider: period,
            lfsr: 1,
            region,
            ..Noise::default()
        }
    }

    fn write_ctrl(&mut self, val: u8) {
        self.len.halt = val & 0x20 != 0;
        self.envelope.write(val & 0x1F);
    }

    fn write_period(&mut self, val: u8) {
        self.mode = val & 0x80 != 0;
        self.period = noise_periods(self.region)[(val & 0x0F) as usize];
    }

    fn write_length(&mut self, val: u8) {
        self.len.write_length(val);
        // "Writing to this register also restarts the envelope."
        self.envelope.start = true;
    }

    /// One shift-register clock: "feedback is the XOR of bit 0 and bit 6 if
    /// periodic noise mode is set, otherwise bit 1; shift right by one bit;
    /// bit 14 is set to the feedback."
    fn step_lfsr(&mut self) {
        let tap = if self.mode { 6 } else { 1 };
        let feedback = (self.lfsr & 1) ^ ((self.lfsr >> tap) & 1);
        self.lfsr = (self.lfsr >> 1) | (feedback << 14);
    }

    /// One CPU clock: the divider counts the table period down, then clocks
    /// the shift register.
    fn tick(&mut self) {
        if self.divider > 0 {
            self.divider -= 1;
        }
        if self.divider == 0 {
            self.divider = self.period;
            self.step_lfsr();
        }
    }

    /// The value this channel contributes to the mixer, 0-15.
    fn output(&self) -> u8 {
        if self.lfsr & 1 != 0 || self.len.length == 0 {
            0
        } else {
            self.envelope.output()
        }
    }
}

/// Which of the DMC's two DMA kinds a fetch is. Loads schedule on a get cycle
/// after `$4015` starts a sample and cost 3 cycles; reloads schedule on a put
/// cycle when the output unit empties its buffer and cost 4 (NESdev "DMA").
/// The kind picks the request delay in `Apu::write_status`; the cost is the
/// bus's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DmcFetchKind {
    #[default]
    Load,
    Reload,
}

/// A memory-reader request: the CPU address to read and which DMA kind it is.
/// `Bus` turns this into a cycle-counted fetch through `cpu_read`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DmcFetch {
    pub addr: u16,
    pub kind: DmcFetchKind,
}

/// The delta modulation channel (NESdev "APU DMC"). The memory reader is a
/// request/response handshake with `Bus`; everything else is local.
#[derive(Clone, Copy, Default)]
struct Dmc {
    region: Region,
    /// `$4010` bits.
    irq_enable: bool,
    looping: bool,
    rate: u8,
    /// The output divider, clocked on APU cycles. `timer` is `period / 2 - 1`
    /// so reloading it yields a clock every `period` CPU cycles.
    timer: u16,
    divider: u16,
    /// The 7-bit counter the mixer sees; `$4011` loads it directly.
    level: u8,
    /// Reload values from `$4012` and `$4013`.
    sample_addr: u16,
    sample_len: u16,
    /// The reader's live address and bytes-remaining counter.
    addr: u16,
    remaining: u16,
    /// The one-byte buffer between reader and shifter, and the shifter.
    buffer: Option<u8>,
    shift: u8,
    bits: u8,
    /// `$4010` bit 7 raised a sample-end interrupt.
    irq: bool,
    /// `$4015` bit 4. The output level is independent of this; only automatic
    /// playback is gated.
    enabled: bool,
    /// CPU cycles until the scheduled fetch becomes a request, and its kind.
    fetch_wait: u8,
    fetch_kind: DmcFetchKind,
    /// A request waiting for `Bus`, and one accepted but not yet completed.
    request: Option<DmcFetch>,
    in_flight: bool,
}

impl Dmc {
    fn for_region(region: Region) -> Dmc {
        let mut dmc = Dmc {
            region,
            ..Dmc::default()
        };
        dmc.set_rate(0);
        dmc
    }

    fn power_on(&mut self) {
        let region = self.region;
        *self = Dmc::for_region(region);
    }

    /// RESET keeps the written registers and the output level, clears `$4015`'s
    /// enable (and therefore playback) and the sample-end flag.
    fn reset(&mut self) {
        self.enabled = false;
        self.remaining = 0;
        self.buffer = None;
        self.shift = 0;
        self.bits = 0;
        self.irq = false;
        self.fetch_wait = 0;
        self.request = None;
        self.in_flight = false;
    }

    fn set_rate(&mut self, rate: u8) {
        self.rate = rate & 0x0F;
        let period = dmc_periods(self.region)[self.rate as usize];
        self.timer = period / 2 - 1;
        if self.divider > self.timer {
            self.divider = self.timer;
        }
    }

    fn write_ctrl(&mut self, val: u8) {
        self.irq_enable = val & 0x80 != 0;
        self.looping = val & 0x40 != 0;
        self.set_rate(val);
        // "It can be cleared by ... clearing the sample-end interrupt flag
        // (I bit) in $4010."
        if !self.irq_enable {
            self.irq = false;
        }
    }

    fn write_level(&mut self, val: u8) {
        self.level = val & 0x7F;
    }

    fn write_addr(&mut self, val: u8) {
        self.sample_addr = 0xC000 + ((val as u16) << 6);
    }

    fn write_len(&mut self, val: u8) {
        self.sample_len = (val as u16) * 16 + 1;
    }

    /// Whether the reader may start a fetch: enabled, bytes remain, the buffer
    /// is empty, and no request is scheduled or in flight.
    fn can_fetch(&self) -> bool {
        self.enabled
            && self.remaining > 0
            && self.buffer.is_none()
            && self.request.is_none()
            && !self.in_flight
            && self.fetch_wait == 0
    }

    /// `$4015` bit 4. A 1 only restarts playback when the bytes-remaining
    /// counter is zero; a 0 sets it to zero and leaves the current shifter and
    /// buffer to finish.
    fn set_enabled(&mut self, on: bool, load_delay: u8) {
        if !on {
            self.enabled = false;
            self.remaining = 0;
            self.fetch_wait = 0;
            self.request = None;
            // A bus DMA may still be in flight; its byte is stale now.
            self.in_flight = false;
            return;
        }
        self.enabled = true;
        if self.remaining == 0 {
            self.addr = self.sample_addr;
            self.remaining = self.sample_len;
            // A load only requires an empty sample buffer. The shifter may
            // still be playing the previous sample's byte, and waiting for it
            // to run dry would delay the load by up to eight DMC clocks.
            if self.can_fetch() {
                self.fetch_wait = load_delay;
                self.fetch_kind = DmcFetchKind::Load;
            }
        }
    }

    fn schedule_reload(&mut self) {
        if self.can_fetch() {
            self.fetch_wait = DMC_RELOAD_DELAY;
            self.fetch_kind = DmcFetchKind::Reload;
        }
    }

    /// One APU clock: the timer clocks the output unit, which shifts one bit
    /// (or idles) whether or not automatic playback is enabled.
    fn tick_apu(&mut self) {
        if self.divider == 0 {
            self.divider = self.timer;
            self.clock_output();
        } else {
            self.divider -= 1;
        }
    }

    /// "The output unit takes the sample byte from the sample buffer and
    /// update[s] the output level accordingly": bit 0 picks up or down by 2,
    /// the register shifts right, and a spent byte is replaced from the
    /// buffer (or the reader is asked for another byte).
    fn clock_output(&mut self) {
        if self.bits > 0 {
            if self.shift & 1 == 0 {
                if self.level > 1 {
                    self.level -= 2;
                }
            } else if self.level < 126 {
                self.level += 2;
            }
            self.shift >>= 1;
            self.bits -= 1;
        }
        if self.bits == 0 {
            if let Some(byte) = self.buffer.take() {
                self.shift = byte;
                self.bits = 8;
            }
        }
        // Ask for the next byte as soon as the buffer is empty, even while the
        // shifter still has bits. Waiting for the shifter to run dry delays the
        // reload (and a multi-byte sample's IRQ) by up to eight DMC clocks.
        self.schedule_reload();
    }

    /// One CPU cycle: a scheduled load/reload becomes a request after its
    /// delay. `Bus` takes the request on the next cycle.
    fn tick_cpu(&mut self) {
        if self.fetch_wait == 0 {
            return;
        }
        self.fetch_wait -= 1;
        if self.can_fetch() {
            self.request = Some(DmcFetch {
                addr: self.addr,
                kind: self.fetch_kind,
            });
        }
    }

    fn take_fetch(&mut self) -> Option<DmcFetch> {
        let req = self.request.take()?;
        self.in_flight = true;
        Some(req)
    }

    /// The reader's byte has arrived. Incrementing past `$FFFF` wraps to
    /// `$8000`; the last byte either restarts the loop or raises the sample-end
    /// flag.
    fn complete_fetch(&mut self, byte: u8) {
        if !self.in_flight {
            // Disabled (or reset) after the bus accepted the request; drop the
            // stale byte.
            return;
        }
        self.in_flight = false;
        if !self.enabled || self.remaining == 0 {
            return;
        }
        self.buffer = Some(byte);
        self.addr = self.addr.wrapping_add(1);
        if self.addr == 0 {
            self.addr = 0x8000;
        }
        self.remaining -= 1;
        if self.remaining == 0 {
            if self.looping {
                self.addr = self.sample_addr;
                self.remaining = self.sample_len;
            } else if self.irq_enable {
                self.irq = true;
            }
        }
    }

    fn output(&self) -> u8 {
        self.level
    }
}

pub struct Apu {
    pulses: [Pulse; 2],
    triangle: Triangle,
    noise: Noise,
    dmc: Dmc,
    frame: Frame,
    /// The APU clock: channels' timers advance on every other CPU cycle.
    /// `true` is the get (first) half of the APU cycle, which is the phase the
    /// DMC's load/reload DMA scheduling aligns to.
    apu_phase: bool,
    /// The configured output rate in Hz.
    rate: u32,
    /// CPU cycles carried over since the last emitted sample.
    acc: u32,
    /// Interleaved stereo, `i16`; drained by `take_samples`.
    samples: Vec<i16>,
    /// Every CPU cycle `tick` has been called for since this `Apu` was made.
    /// Power-on and reset do not clear it, so it is a monotonic measure of how
    /// far the APU has been advanced, for tests and debugging.
    ticks: u64,
    /// Which CPU's clock and period tables this APU is.
    region: Region,
}

impl Apu {
    #[cfg(test)]
    pub fn new() -> Apu {
        Apu::for_region(Region::Ntsc)
    }

    /// A 2A03 for NTSC or a 2A07 for PAL.
    pub fn for_region(region: Region) -> Apu {
        let mut apu = Apu {
            pulses: [Pulse::new(true), Pulse::new(false)],
            triangle: Triangle::default(),
            noise: Noise::for_region(region),
            dmc: Dmc::for_region(region),
            frame: Frame::new(region),
            apu_phase: false,
            rate: 48_000,
            acc: 0,
            samples: Vec::new(),
            ticks: 0,
            region,
        };
        apu.power_on();
        apu
    }

    /// Power-on: all channel state starts cleared, and the frame counter
    /// behaves "as if $00 were written to $4017" a few clocks before the first
    /// instruction.
    pub fn power_on(&mut self) {
        self.pulses = [Pulse::new(true), Pulse::new(false)];
        self.triangle = Triangle::default();
        self.noise = Noise::for_region(self.region);
        self.dmc.power_on();
        self.frame = Frame::new(self.region);
        self.frame.start(0x00, RESET_PRE);
    }

    /// The RESET button: RAM survives, every channel register keeps its value,
    /// `$4015` is cleared (which forces the length counters to 0), and the
    /// counter restarts as if the last value written to `$4017` were written
    /// again.
    ///
    /// The preserved registers are measured: `apu_reset`'s `len_ctrs_enabled`
    /// presses RESET mid-run and then re-enables `$4015` and reloads each
    /// channel's length itself, so what it reads back from `$4015` is those
    /// reloads plus the frame clocks that follow (`$04`, only the triangle
    /// still live). A reset that kept the old enables or length counters would
    /// report a different code.
    ///
    /// Nothing in the player's run loop presses RESET yet; the `apu_reset`
    /// harness is what drives it.
    #[allow(dead_code)]
    pub fn reset(&mut self) {
        let last = self.frame.last_write;
        for p in &mut self.pulses {
            p.len.clear();
        }
        self.triangle.len.clear();
        self.noise.len.clear();
        self.dmc.reset();
        self.frame = Frame::new(self.region);
        self.frame.start(last, RESET_PRE);
    }

    /// Sample rate for the mixer.
    pub fn set_sample_rate(&mut self, hz: u32) {
        self.rate = hz;
    }

    /// One CPU cycle.
    /// How many CPU cycles this APU has been advanced through `tick`. The
    /// placement in `Bus::apu_advance_ahead` is a pre-advance plus an equal
    /// number of skipped ticks, so a test can assert that one instruction
    /// moved this by exactly its cycle count.
    ///
    /// Nothing in the player's run loop reads it; the bus tests do.
    #[allow(dead_code)]
    pub fn total_cycles(&self) -> u64 {
        self.ticks
    }

    pub fn tick(&mut self) {
        self.ticks += 1;
        // CPU-cycle work first: a reload scheduled by this cycle's APU clock
        // must wait for the next CPU cycle (the next put cycle), not consume
        // its wait in the same tick that scheduled it.
        self.dmc.tick_cpu();
        // The channels' timers run at half the CPU rate...
        self.apu_phase = !self.apu_phase;
        if self.apu_phase {
            for p in &mut self.pulses {
                p.tick();
            }
            self.dmc.tick_apu();
        }
        // ...except the triangle's and the noise's, which run at the CPU rate.
        self.triangle.tick();
        self.noise.tick();

        // The channels' delayed control signals -- halt, and the "clocked while
        // live" mark -- arrive before this cycle's clocks.
        for p in &mut self.pulses {
            p.len.tick();
        }
        self.triangle.len.tick();
        self.noise.len.tick();

        let clocks = self.frame.tick();
        if clocks.quarter {
            for p in &mut self.pulses {
                p.envelope.clock();
            }
            self.triangle.clock_linear();
            self.noise.envelope.clock();
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

        self.acc += self.rate;
        while self.acc >= self.region.cpu_hz() {
            self.acc -= self.region.cpu_hz();
            self.emit();
        }
    }

    /// The nonlinear mixer, straight from NESdev "APU Mixer": the pulse term
    /// plus the triangle/noise/DMC term. All terms are non-negative, so the
    /// result is too.
    fn mix(&self) -> i16 {
        let p = self.pulses[0].output() as u16 + self.pulses[1].output() as u16;
        let pulse_out = if p == 0 {
            0.0
        } else {
            95.88 / (8128.0 / p as f64 + 100.0)
        };
        let t = self.triangle.output();
        let n = self.noise.output();
        let d = self.dmc.output();
        let tnd_out = if t == 0 && n == 0 && d == 0 {
            0.0
        } else {
            159.79 / (1.0 / (t as f64 / 8227.0 + n as f64 / 12_241.0 + d as f64 / 22_638.0) + 100.0)
        };
        ((pulse_out + tnd_out) * 32_767.0).clamp(0.0, 32_767.0) as i16
    }

    fn emit(&mut self) {
        // ponytail: fixed cap, drop newest; per-device backpressure if a
        // player ever re-introduces latency.
        if self.samples.len() >= SAMPLE_QUEUE_CAP {
            return;
        }
        let s = self.mix();
        self.samples.push(s); // left
        self.samples.push(s); // mono fanned out to stereo
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
                if self.dmc.remaining > 0 {
                    v |= 0x10;
                }
                if self.dmc.irq {
                    v |= 0x80;
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
            0x400A => self.triangle.write_timer_low(val),
            0x400B => self.triangle.write_length(val),
            0x400C => self.noise.write_ctrl(val),
            0x400E => self.noise.write_period(val),
            0x400F => self.noise.write_length(val),
            0x4010 => self.dmc.write_ctrl(val),
            0x4011 => self.dmc.write_level(val),
            0x4012 => self.dmc.write_addr(val),
            0x4013 => self.dmc.write_len(val),
            0x4015 => self.write_status(val),
            0x4017 => self.frame.write(val),
            _ => {}
        }
    }

    fn write_status(&mut self, val: u8) {
        // "Writing to this register also clears the DMC sample-end interrupt
        // flag regardless of the written value."
        self.dmc.irq = false;
        let load_delay = if self.apu_phase {
            DMC_LOAD_DELAY_GET
        } else {
            DMC_LOAD_DELAY_PUT
        };
        self.dmc.set_enabled(val & 0x10 != 0, load_delay);
        for (i, p) in self.pulses.iter_mut().enumerate() {
            if val & (1 << i) != 0 {
                p.len.enabled = true;
            } else {
                p.len.clear();
            }
        }
        if val & 0x04 != 0 {
            self.triangle.len.enabled = true;
        } else {
            self.triangle.len.clear();
        }
        if val & 0x08 != 0 {
            self.noise.len.enabled = true;
        } else {
            self.noise.len.clear();
        }
    }

    pub fn irq_pending(&self) -> bool {
        self.frame.flag || self.dmc.irq
    }

    /// Hand the DMC's pending memory-reader request to the bus. The bus turns
    /// it into a 3/4-cycle DMA and returns the byte through
    /// `dmc_fetch_complete`.
    pub fn take_dmc_fetch(&mut self) -> Option<DmcFetch> {
        self.dmc.take_fetch()
    }

    /// The byte the DMC's memory reader fetched through the CPU bus.
    pub fn dmc_fetch_complete(&mut self, byte: u8) {
        self.dmc.complete_fetch(byte);
    }

    /// Whether automatic DMC playback can still ask for bytes. Controller
    /// reads use this to decide whether an instruction needs the bus's DMC
    /// pre-advance for conflict detection.
    pub fn dmc_playing(&self) -> bool {
        self.dmc.enabled && (self.dmc.remaining > 0 || self.dmc.request.is_some())
    }

    /// Test view of the reader state: live address, bytes remaining, buffer.
    #[cfg(test)]
    pub(crate) fn dmc_state(&self) -> (u16, u16, Option<u8>) {
        (self.dmc.addr, self.dmc.remaining, self.dmc.buffer)
    }

    pub fn take_samples(&mut self) -> Vec<i16> {
        std::mem::take(&mut self.samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// blargg's readme, mode 0: after a `$00` write the steps land on CPU
    /// cycles 7459, 14915, 22373 and 29831, and only steps 2 and 4 clock the
    /// length counter.
    #[test]
    fn the_frame_counter_clocks_length_twice_a_mode_0_frame() {
        let mut apu = Apu::new();
        apu.cpu_write(0x4015, 0x01); // enable pulse 1
        apu.cpu_write(0x4003, 0xF0); // length index 30 -> 32
        apu.tick(); // into the write's staging slot
        apu.tick(); // and into the counter
        assert_eq!(
            apu.pulses[0].len.length, 32,
            "the load does not wait for a half frame"
        );

        apu.cpu_write(0x4017, 0x00); // begin mode 0
        for _ in 0..14_900 {
            apu.tick();
        }
        assert_eq!(
            apu.pulses[0].len.length, 32,
            "step 1 at CPU 7459 clocks the linear counter only"
        );
        for _ in 0..10_000 {
            apu.tick();
        }
        assert_eq!(
            apu.pulses[0].len.length, 31,
            "step 2 at CPU 14915 clocks the length too"
        );
        for _ in 0..15_100 {
            apu.tick();
        }
        assert_eq!(
            apu.pulses[0].len.length, 30,
            "step 4 at CPU 29831 clocks it again"
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

    /// blargg's halt and reload edge cases (readmes "Length Halt", "Length
    /// Reload"), as unit tests of the counter.
    #[test]
    fn length_halt_and_reload_edges_follow_the_readme() {
        // The clocking circuits see the halt flag two CPU clocks late: the
        // write lands at the end of its own clock, hardware registers it on
        // the next APU clock. A halt write in the clocking clock therefore
        // does not stop that clock, and one in the clock before it does.
        let mut p = Pulse::default();
        p.len.enabled = true;
        p.len.length = 32;
        p.write_ctrl(0x30); // halt set...
        p.len.clock(); // ...but the same-clock clock fires anyway
        assert_eq!(
            p.len.length, 31,
            "a same-clock halt write does not stop the clock"
        );
        p.len.tick();
        p.len.tick();
        p.len.clock();
        assert_eq!(p.len.length, 31, "the second clock sees the halt");

        // A write loads the counter two CPU clocks later (NESdev "APU Length
        // Counter"), no half frame needed.
        let mut p = Pulse::default();
        p.len.enabled = true;
        p.len.length = 100;
        p.write_length(0xF0); // index 30 -> 32
        p.len.tick();
        p.len.tick();
        assert_eq!(p.len.length, 32, "the load does not wait for a half frame");

        // A load written in the clock right after the write's own clock is
        // ignored completely; the clock itself still runs. "Length reload is
        // completely ignored if written during length clocking and length
        // counter is non-zero before clocking."
        p.len.length = 100;
        p.write_length(0xF0);
        p.len.tick();
        p.len.clock(); // 100 -> 99, and this clock drops the load
        p.len.tick();
        assert_eq!(
            p.len.length, 99,
            "reload dropped, the counter still clocked"
        );

        // A load written between clocks lands, and the next clock clocks it.
        p.write_length(0xF0);
        p.len.tick();
        p.len.tick();
        assert_eq!(p.len.length, 32, "a write between clocks reloads");
        p.len.clock();
        assert_eq!(p.len.length, 31, "and then the clock runs");

        // With a zero counter the load wins: "clocked while already zero"
        // neither decrements nor ignores the load.
        p.len.length = 0;
        p.len.clock();
        p.write_length(0xF0);
        p.len.tick();
        p.len.tick();
        assert_eq!(p.len.length, 32, "a load during a zero clock lands in full");

        // A disabled channel cannot be loaded at all.
        p.len.enabled = false;
        p.len.length = 0;
        p.write_length(0xF0);
        p.len.tick();
        p.len.tick();
        assert_eq!(p.len.length, 0, "a disabled counter stays zero");
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

    /// The wiki's looping 32 steps: 15 down to 0, then 0 back up to 15. The
    /// triangle's timer runs at the CPU clock, so t = 8 is nine cycles a step.
    #[test]
    fn the_triangle_sequence_runs_15_down_to_0_then_back() {
        let mut apu = Apu::new();
        apu.triangle.len.length = 200;
        apu.triangle.linear = 200;
        apu.cpu_write(0x400A, 0x08); // t = 8
        apu.cpu_write(0x400B, 0x00);
        apu.triangle.divider = 0;
        let mut samples = Vec::new();
        let mut seq = None;
        for _ in 0..4_000 {
            if samples.len() >= 40 {
                break;
            }
            apu.tick();
            let now = apu.triangle.sequence;
            if seq != Some(now) {
                seq = Some(now);
                samples.push(apu.triangle.output());
            }
        }
        let want: Vec<u8> = (0..32u8)
            .map(|i| if i < 16 { 15 - i } else { i - 16 })
            .collect();
        assert_eq!(samples[..32], want[..], "15 down to 0, then 0 up to 15");
        assert_eq!(samples[32..], want[..8], "and the sequence loops");
    }

    /// The sequencer is clocked only while both counters are non-zero, and
    /// its output is *held* when it stops -- the silence pop SMB relies on.
    #[test]
    fn the_linear_counter_holds_the_output_at_its_last_value_when_it_runs_out() {
        let mut apu = Apu::new();
        apu.triangle.len.length = 200;
        apu.triangle.linear = 200;
        apu.cpu_write(0x400A, 0x08);
        apu.cpu_write(0x400B, 0x00);
        apu.triangle.divider = 0;
        for _ in 0..20 {
            apu.tick();
        }
        let sounding = apu.triangle.output();
        assert_ne!(sounding, 0, "the channel is sounding");
        let seq = apu.triangle.sequence;

        apu.triangle.linear = 0; // the linear counter ran out
        for _ in 0..200 {
            apu.tick();
        }
        assert_eq!(
            apu.triangle.sequence, seq,
            "the sequencer stops when silenced"
        );
        assert_eq!(apu.triangle.output(), sounding, "held, not snapped to zero");
    }

    /// The control flag is the halt: while set, the reload flag stays set and
    /// the counter is held; while clear, the flag is cleared every clock and
    /// the counter walks down after one reload.
    #[test]
    fn the_control_flag_becomes_the_linear_reload_when_clear() {
        let mut t = Triangle::default();

        t.write_ctrl(0x88); // control set, reload value 8
        t.write_length(0x00); // $400B: sets the reload flag
        t.clock_linear();
        assert_eq!(t.linear, 8, "reloaded");
        t.clock_linear();
        assert_eq!(t.linear, 8, "held while the control flag is set");
        t.write_ctrl(0x85); // reload value 5, still held
        t.clock_linear();
        assert_eq!(t.linear, 5, "a $4008 write lands at the next clock");

        t.write_ctrl(0x05); // control clear, reload value 5
        t.write_length(0x00); // one reload...
        t.clock_linear();
        assert_eq!(t.linear, 5, "reloaded once");
        t.clock_linear();
        assert_eq!(t.linear, 4, "...then it counts down");
        t.clock_linear();
        assert_eq!(t.linear, 3);
    }

    /// The 15-bit LFSR: feedback is bit 0 XOR bit 1, shifted right with the
    /// feedback landing in bit 14; the sequence is 32767 steps and never
    /// reaches zero (NESdev "APU Noise", "Operation").
    #[test]
    fn the_lfsr_sequence_follows_the_15_bit_taps() {
        let mut n = Noise::new();
        n.lfsr = 0x7FFF; // all ones: 1 XOR 1 = 0
        n.step_lfsr();
        assert_eq!(n.lfsr, 0x3FFF, "bits 0 and 1 both set: feedback zero");

        n.lfsr = 1; // the power-up seed
        for _ in 0..32_767 {
            n.step_lfsr();
            assert_ne!(n.lfsr, 0, "the all-zero state is unreachable");
        }
        assert_eq!(
            n.lfsr, 1,
            "the generator returns to its seed after 32767 steps"
        );
    }

    /// The periodic mode swaps bit 6 for bit 1 in the feedback: the same seed
    /// diverges, and the sequence collapses to the wiki's 93 steps.
    #[test]
    fn mode_flag_swaps_bit_6_for_bit_1_in_the_feedback() {
        let seed = 0x41; // bit 0 and bit 6 set, bit 1 clear: the two taps differ
        let mut m0 = Noise::new();
        m0.lfsr = seed;
        m0.step_lfsr();
        let mut m1 = Noise::new();
        m1.lfsr = seed;
        m1.mode = true;
        m1.step_lfsr();
        assert_eq!(m0.lfsr, 0x4020, "bit 0 XOR bit 1 = 1");
        assert_eq!(m1.lfsr, 0x0020, "bit 0 XOR bit 6 = 0");

        m1.lfsr = 1;
        let mut steps = 0;
        loop {
            m1.step_lfsr();
            steps += 1;
            assert!(steps <= 93, "periodic noise runs at most 93 steps");
            if m1.lfsr == 1 {
                break;
            }
        }
        assert_eq!(steps, 93, "the measured periodic-mode sequence length");
    }

    /// The NTSC period table, and "the period determines how many CPU cycles
    /// happen between shift register clocks".
    #[test]
    fn the_period_table_matches_the_ntsc_values() {
        assert_eq!(
            NOISE_PERIODS,
            [4, 8, 16, 32, 64, 96, 128, 160, 202, 254, 380, 508, 762, 1016, 2034, 4068]
        );

        let mut apu = Apu::new();
        apu.cpu_write(0x400E, 0x00); // P = 0: four CPU cycles per step
        let mut changes = Vec::new();
        let mut lfsr = apu.noise.lfsr;
        for tick in 0..400 {
            apu.tick();
            if apu.noise.lfsr != lfsr {
                changes.push(tick);
                lfsr = apu.noise.lfsr;
            }
        }
        let gaps: Vec<i64> = changes.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps.len() > 50, "the register must keep stepping");
        assert!(
            gaps.iter().all(|&g| g == 4),
            "P = 0 is 4 CPU cycles: {gaps:?}"
        );
    }

    /// The PAL period table, and the same "P = 2" register selecting 14 CPU
    /// cycles instead of the NTSC 16.
    #[test]
    fn the_period_table_matches_the_pal_values() {
        assert_eq!(
            PAL_NOISE_PERIODS,
            [4, 8, 14, 30, 60, 88, 118, 148, 188, 236, 354, 472, 708, 944, 1890, 3778]
        );

        let mut apu = Apu::for_region(Region::Pal);
        apu.cpu_write(0x400E, 0x02);
        assert_eq!(apu.noise.period, 14);
    }

    /// The 2A07's frame counter runs at 50 Hz: quarter at APU cycle 4156, half
    /// at 8313, quarter at 12469, and the mode-0 interrupt at 16626, wrapping
    /// at 16627 (NESdev "APU Frame Counter", PAL column).
    #[test]
    fn the_pal_frame_counter_steps_at_the_2a07_period() {
        let mut f = Frame::new(Region::Pal);
        f.start(0x00, 0);
        for _ in 0..33_252 {
            f.tick();
        }
        assert!(!f.flag, "no interrupt one CPU cycle early");
        f.tick();
        assert!(f.flag, "the flag rises at APU cycle 16626");
        f.tick();
        assert_eq!(f.cycle, 0, "the sequence wraps at 16627");
    }

    /// The mixer's accumulator is the CPU clock's: half a 2A07 second must be
    /// 24,000 stereo frames, not the 22,304 an NTSC clock would emit.
    #[test]
    fn the_pal_mixer_runs_on_the_2a07_clock() {
        let mut apu = Apu::for_region(Region::Pal);
        for _ in 0..831_303 {
            apu.tick();
        }
        let pairs = apu.take_samples().len() / 2;
        assert!(
            (23_900..=24_100).contains(&pairs),
            "half a PAL second emitted {pairs} stereo frames"
        );
    }

    /// "The mixer receives the current envelope volume except when bit 0 of
    /// the shift register is set, or the length counter is zero."
    #[test]
    fn the_envelope_is_gated_by_bit_0_and_the_length_counter() {
        let mut apu = Apu::new();
        apu.cpu_write(0x400C, 0x1F); // constant volume 15
        apu.noise.len.length = 200;
        apu.noise.lfsr = 1;
        assert_eq!(apu.noise.output(), 0, "bit 0 set silences the channel");
        apu.noise.lfsr = 2;
        assert_eq!(
            apu.noise.output(),
            15,
            "bit 0 clear lets the envelope through"
        );
        apu.noise.len.length = 0;
        assert_eq!(
            apu.noise.output(),
            0,
            "a zero length counter silences it too"
        );
    }

    /// No channels enabled: the queue still fills at the configured rate, and
    /// every value is exactly zero.
    #[test]
    fn silence_in_is_silence_out() {
        let mut apu = Apu::new();
        for _ in 0..179_000 {
            apu.tick();
        }
        let s = apu.take_samples();
        assert!(!s.is_empty(), "48000 Hz must still emit samples when idle");
        assert!(
            s.iter().all(|&v| v == 0),
            "no channels enabled -> exactly 0"
        );
    }

    /// One emulated second of CPU ticks is exactly 48000 stereo frames.
    #[test]
    fn the_sample_rate_accumulator_emits_the_requested_rate() {
        let mut apu = Apu::new();
        for _ in 0..1_789_773 {
            apu.tick();
        }
        assert_eq!(
            apu.take_samples().len(),
            96_000,
            "48000 frames, two i16 each"
        );

        let mut apu = Apu::new();
        apu.set_sample_rate(1_000);
        for _ in 0..1_789_773 {
            apu.tick();
        }
        assert_eq!(apu.take_samples().len(), 2_000, "the configured rate wins");
    }

    /// The mixer's output matches the wiki's formula at full scale, never
    /// escapes i16, and the queue never grows past its cap.
    #[test]
    fn the_output_never_clips_past_i16_range_and_never_overflows_capacity() {
        // Arrange every channel at full scale *without ticking*: the exact
        // value is only observable before the sequencers start moving.
        let mut apu = Apu::new();
        for (ctrl, timer) in [(0x4000u16, 0x4002u16), (0x4004, 0x4006)] {
            apu.cpu_write(ctrl, 0x1F); // duty 0, constant volume 15
            apu.cpu_write(timer, 0x08); // t = 8: audible
        }
        apu.pulses[0].len.length = 200;
        apu.pulses[1].len.length = 200;
        apu.pulses[0].duty_bit = true;
        apu.pulses[1].duty_bit = true;
        apu.cpu_write(0x400C, 0x1F); // noise: constant volume 15
        apu.noise.len.length = 200;
        apu.noise.lfsr = 2; // bit 0 clear: the envelope passes
        apu.triangle.len.length = 200;
        apu.triangle.linear = 200;
        apu.triangle.value = 15;

        let pulse_out = 95.88 / (8128.0 / 30.0 + 100.0); // both pulses at 15
        let tnd_out = 159.79 / (1.0 / (15.0 / 8227.0 + 15.0 / 12_241.0) + 100.0);
        let expected = ((pulse_out + tnd_out) * 32_767.0) as i16;
        assert_eq!(
            apu.mix(),
            expected,
            "pulses at 30, triangle and noise at 15"
        );
        assert!(apu.mix() > 15_000, "fully open channels must be loud");

        // Over a moving mix the values stay in range and stay loud.
        let mut apu = Apu::new();
        for (ctrl, timer) in [(0x4000u16, 0x4002u16), (0x4004, 0x4006)] {
            apu.cpu_write(ctrl, 0xFF); // duty 3, held, constant volume 15
            apu.cpu_write(timer, 0x08);
        }
        apu.cpu_write(0x400C, 0x3F); // noise: held, constant volume 15
        apu.pulses[0].len.length = 200;
        apu.pulses[1].len.length = 200;
        apu.noise.len.length = 200;
        apu.triangle.len.length = 200;
        apu.triangle.linear = 200;
        let mut max = 0i16;
        for _ in 0..200_000 {
            apu.tick();
            for v in apu.take_samples() {
                assert!(v >= 0, "every mixer term is non-negative");
                max = max.max(v);
            }
        }
        assert!(max > 15_000, "full-volume channels must be loud");

        // The queue is capped: at one pair per CPU tick it overruns, and the
        // newest samples are dropped instead of growing without bound.
        let mut apu = Apu::new();
        apu.set_sample_rate(1_789_773);
        for _ in 0..100_000 {
            apu.tick();
        }
        assert_eq!(apu.take_samples().len(), SAMPLE_QUEUE_CAP);
    }

    /// `$4015` starts the reader, the load DMA asks for the first byte after
    /// its delay, and fetching the last byte raises the sample-end flag. A
    /// `$4015` write clears that flag; a read does not.
    #[test]
    fn the_dmc_loads_a_sample_and_raises_its_irq() {
        let mut apu = Apu::new();
        apu.cpu_write(0x4017, 0x40); // inhibit the frame IRQ for this test
        apu.cpu_write(0x4010, 0x8F); // IRQ on, rate $F
        apu.cpu_write(0x4012, 0x00); // $C000
        apu.cpu_write(0x4013, 0x00); // one byte
        apu.cpu_write(0x4015, 0x10);

        assert_eq!(apu.cpu_read(0x4015) & 0x10, 0x10, "sample is playing");
        for _ in 0..DMC_LOAD_DELAY_PUT {
            apu.tick();
        }
        assert_eq!(
            apu.take_dmc_fetch(),
            Some(DmcFetch {
                addr: 0xC000,
                kind: DmcFetchKind::Load,
            }),
            "the load DMA arrives after its delay"
        );
        apu.dmc_fetch_complete(0xAA);

        assert_eq!(apu.cpu_read(0x4015) & 0x10, 0, "last byte fetched");
        assert_eq!(apu.cpu_read(0x4015) & 0x80, 0x80, "sample-end flag");
        assert!(apu.dmc.irq);

        apu.cpu_read(0x4015);
        assert!(apu.dmc.irq, "reading $4015 does not clear the DMC flag");
        apu.cpu_write(0x4015, 0x10);
        assert!(!apu.dmc.irq, "writing $4015 clears it");
    }

    /// The loop flag reloads both counters at the last byte, so playback
    /// continues and the sample-end flag never rises.
    #[test]
    fn the_dmc_loop_reloads_the_address_and_length() {
        let mut apu = Apu::new();
        apu.cpu_write(0x4010, 0x4F); // loop, rate $F, IRQ off
        apu.cpu_write(0x4012, 0x01); // $C040
        apu.cpu_write(0x4013, 0x00); // one byte
        apu.cpu_write(0x4015, 0x10);
        for _ in 0..DMC_LOAD_DELAY_PUT {
            apu.tick();
        }
        assert!(apu.take_dmc_fetch().is_some());
        apu.dmc_fetch_complete(0x00);

        assert_eq!(apu.dmc.remaining, 1, "the length reloads");
        assert_eq!(apu.dmc.addr, 0xC040, "and so does the address");
        assert!(!apu.dmc.irq, "the loop flag suppresses the IRQ");
    }

    /// The reader wraps past `$FFFF` to `$8000`, not to zero.
    #[test]
    fn the_dmc_reader_wraps_to_8000() {
        let mut dmc = Dmc::for_region(Region::Ntsc);
        dmc.enabled = true;
        dmc.in_flight = true;
        dmc.addr = 0xFFFF;
        dmc.remaining = 2;
        dmc.complete_fetch(0x00);
        assert_eq!(dmc.addr, 0x8000);
        assert_eq!(dmc.remaining, 1);
    }

    /// The output unit moves by two per bit, clamps at both ends, and loads
    /// the buffer only after the previous byte's eight bits are spent.
    #[test]
    fn the_dmc_output_moves_by_two_and_clamps() {
        let mut dmc = Dmc::for_region(Region::Ntsc);
        dmc.write_level(64);
        dmc.buffer = Some(0b0000_0011);
        dmc.bits = 0;

        dmc.clock_output(); // the buffer moves into the shifter, no output yet
        assert_eq!(dmc.level, 64);
        dmc.clock_output(); // bit 0 = 1
        assert_eq!(dmc.level, 66);
        dmc.clock_output(); // bit 1 = 1
        assert_eq!(dmc.level, 68);

        dmc.write_level(126);
        dmc.shift = 0xFF;
        dmc.bits = 8;
        dmc.clock_output();
        assert_eq!(dmc.level, 126, "increments stop below 126");
        dmc.write_level(1);
        dmc.shift = 0;
        dmc.bits = 8;
        dmc.clock_output();
        assert_eq!(dmc.level, 1, "decrements stop above 1");

        // The mixer sees the counter even when playback is disabled.
        let mut apu = Apu::new();
        apu.cpu_write(0x4011, 0x7F);
        assert_eq!(apu.dmc.output(), 0x7F);
        assert!(apu.mix() > 0, "the DMC term reaches the mixer");
    }

    /// The DMC's timer is clocked at APU rate, so its reload is half the CPU
    /// period; PAL uses the 2A07 table.
    #[test]
    fn the_dmc_rate_table_uses_the_region() {
        let mut ntsc = Dmc::for_region(Region::Ntsc);
        ntsc.set_rate(0x0F);
        assert_eq!(ntsc.timer, 54 / 2 - 1);
        let mut pal = Dmc::for_region(Region::Pal);
        pal.set_rate(0x0F);
        assert_eq!(pal.timer, 50 / 2 - 1);
    }

    /// The reader prefetches as soon as the output unit takes the buffer, not
    /// after the shifter has spent all eight bits. A late prefetch delays the
    /// sample-end IRQ of every multi-byte sample by up to eight DMC clocks.
    #[test]
    fn the_dmc_prefetches_while_the_shifter_still_has_bits() {
        let mut dmc = Dmc::for_region(Region::Ntsc);
        dmc.enabled = true;
        dmc.remaining = 2;
        dmc.addr = 0xC000;
        dmc.buffer = Some(0xAA);
        dmc.bits = 0;

        dmc.clock_output();
        assert_eq!(dmc.bits, 8, "the buffer moved into the shifter");
        assert!(dmc.buffer.is_none());
        assert_eq!(
            dmc.request, None,
            "the reload is scheduled, not requested yet"
        );

        dmc.tick_cpu();
        assert_eq!(
            dmc.request,
            Some(DmcFetch {
                addr: 0xC000,
                kind: DmcFetchKind::Reload,
            }),
            "the next byte is requested while the shifter is still busy"
        );
    }

    /// A `$4015` start is a load DMA whenever the sample buffer is empty, even
    /// if the previous sample's shifter is still playing. The load must not
    /// wait for the shifter to run dry.
    #[test]
    fn a_4015_start_loads_with_an_empty_buffer_while_the_shifter_plays() {
        let mut apu = Apu::new();
        apu.cpu_write(0x4010, 0x0F); // rate $F, no IRQ
        apu.cpu_write(0x4012, 0x00);
        apu.cpu_write(0x4013, 0x00);
        apu.dmc.bits = 4; // a previous sample is still shifting
        apu.dmc.remaining = 0;
        apu.cpu_write(0x4015, 0x10);

        for _ in 0..3 {
            apu.tick();
        }
        assert_eq!(
            apu.take_dmc_fetch(),
            Some(DmcFetch {
                addr: 0xC000,
                kind: DmcFetchKind::Load,
            }),
            "the empty buffer starts a load even while bits remain"
        );
    }

    /// A load DMA from a get-cycle write halts four CPU cycles later; a
    /// put-cycle write halts three cycles later. Both land on a get cycle.
    #[test]
    fn the_dmc_load_delay_follows_the_write_cycle_phase() {
        for (phase, delay) in [(true, 4u8), (false, 3u8)] {
            let mut apu = Apu::new();
            apu.apu_phase = phase;
            apu.cpu_write(0x4010, 0x0F);
            apu.cpu_write(0x4012, 0x00);
            apu.cpu_write(0x4013, 0x00);
            apu.cpu_write(0x4015, 0x10);

            for _ in 0..delay - 1 {
                apu.tick();
            }
            assert_eq!(apu.take_dmc_fetch(), None, "phase {phase}: too early");
            apu.tick();
            assert!(apu.take_dmc_fetch().is_some(), "phase {phase}: request");
        }
    }

    /// The output unit clocks on a get cycle; the reload request belongs to the
    /// following put cycle. It must not consume its one-cycle wait in the same
    /// CPU tick that scheduled it.
    #[test]
    fn a_reload_request_waits_for_the_next_cpu_cycle() {
        let mut apu = Apu::new();
        apu.dmc.enabled = true;
        apu.dmc.remaining = 2;
        apu.dmc.buffer = Some(0xAA);
        apu.dmc.bits = 0;
        apu.dmc.divider = 0;
        apu.apu_phase = false; // the next tick is an APU clock (get)

        apu.tick();
        assert!(
            apu.dmc.request.is_none(),
            "the reload is scheduled this cycle"
        );
        apu.tick();
        assert!(apu.dmc.request.is_some(), "the next put cycle requests it");
    }

    /// A two-byte sample raises the sample-end IRQ when the second byte is
    /// fetched, which happens while the first byte is still shifting.
    #[test]
    fn a_multi_byte_sample_raises_the_irq_when_the_last_byte_is_prefetched() {
        let mut apu = Apu::new();
        apu.cpu_write(0x4017, 0x40); // inhibit the frame IRQ
        apu.cpu_write(0x4010, 0x8F); // IRQ on, rate $F
        apu.cpu_write(0x4012, 0x00);
        apu.cpu_write(0x4013, 0x00);
        apu.dmc.sample_len = 2;
        apu.cpu_write(0x4015, 0x10);

        for _ in 0..3 {
            apu.tick();
        }
        let first = apu.take_dmc_fetch().expect("first byte");
        assert_eq!(first.kind, DmcFetchKind::Load);
        apu.dmc_fetch_complete(0x11);
        assert!(!apu.irq_pending(), "one byte remains");

        apu.dmc.clock_output(); // take the first byte into the shifter
        assert!(apu.dmc.bits > 0);

        let mut waited = 0;
        let second = loop {
            if let Some(req) = apu.take_dmc_fetch() {
                break req;
            }
            apu.tick();
            waited += 1;
            assert!(waited < 4, "the second byte was not prefetched");
        };
        assert_eq!(second.addr, 0xC001);
        assert_eq!(second.kind, DmcFetchKind::Reload);
        apu.dmc_fetch_complete(0x22);
        assert!(
            apu.irq_pending(),
            "the last byte raises the sample-end flag"
        );
        assert_eq!(apu.dmc.remaining, 0);
    }
}
