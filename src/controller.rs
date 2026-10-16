//! The standard NES controller (4021 shift register) at both ports.
//!
//! Reference: NESdev "Standard controller", fetched 2026-10-16 --
//! "After 8 bits are read, all subsequent bits will report 1 on a standard NES
//! controller", the bit order "A, B, Select, Start, Up, Down, Left, Right", and
//! the $4016 strobe reloading the register while it is high.
//!
pub struct Controller {
    /// Latched button state, in `$4016` report order: A, B, Select, Start,
    /// Up, Down, Left, Right from bit 0 up.
    buttons: u8,
    /// The 4021's shift register, held in the same "1 = pressed" sense as
    /// `buttons`, so A is bit 0 and each read clocks one more bit out.
    shift: u8,
    /// How many of the eight real buttons are still to be read. The chip's
    /// serial input shifts in zeros and its output is inverted, so the report
    /// does not run dry at 0 -- it runs out and reads 1 forever. A count is the
    /// honest way to say that: shifting a refill bit in would put a spurious 1
    /// into the Right slot, which is read eighth and would land second.
    left: u8,
    strobe: bool,
}

impl Controller {
    /// No controller plugged in: "When no controller is connected, the
    /// corresponding status bit will report 0" (NESdev "Standard controller",
    /// pull-ups in the socket).
    pub fn new() -> Controller {
        Controller {
            buttons: 0,
            shift: 0,
            left: 0,
            strobe: false,
        }
    }

    /// Replace the whole button state. `mask` bit order is A, B, Select, Start,
    /// Up, Down, Left, Right.
    pub fn set_buttons(&mut self, mask: u8) {
        self.buttons = mask;
    }

    /// One report bit. While the strobe is high the register is being reloaded
    /// every instant, so every read reports A; once it is low each read clocks
    /// one more button out. Past the eighth read the register is empty and the
    /// inverted output reports 1 ("After 8 bits are read, all subsequent bits
    /// will report 1 on a standard NES controller" -- NESdev "Standard
    /// controller"), so an empty pad still reads 0 for all eight of its bits and
    /// 1 thereafter.
    pub fn read(&mut self) -> u8 {
        if self.strobe {
            return self.buttons & 1;
        }
        if self.left == 0 {
            return 1;
        }
        let bit = self.shift & 1;
        self.shift >>= 1;
        self.left -= 1;
        bit
    }

    /// `$4016` bit 0. The high pulse latches the buttons; games always write a
    /// 1 then a 0, and the low write is what releases the register.
    pub fn write_strobe(&mut self, val: u8) {
        self.strobe = val & 1 != 0;
        if self.strobe {
            self.shift = self.buttons;
            self.left = 8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Controller;

    #[test]
    fn the_controller_shifts_one_button_per_read_after_a_strobe() {
        let mut c = Controller::new();
        c.set_buttons(0x01 | 0x80); // A and Right held
        c.write_strobe(1); // load, then release
        c.write_strobe(0);
        assert_eq!(c.read() & 1, 1, "A should read first");
        assert_eq!(c.read() & 1, 0, "second button was not B");
        assert_eq!(c.read() & 1, 0);
        assert_eq!(c.read() & 1, 0);
        assert_eq!(c.read() & 1, 0);
        assert_eq!(c.read() & 1, 0);
        assert_eq!(c.read() & 1, 0);
        assert_eq!(c.read() & 1, 1, "Right should read eighth");
    }

    /// A game that polls `$4016` before it ever strobes -- or strobes and then
    /// reads eight times without a button held -- must see "nothing pressed",
    /// which on a 4021 means 1s, not 0s. So a fresh pad reads 1 forever.
    #[test]
    fn a_fresh_controller_reads_one_on_every_read() {
        let mut c = Controller::new();
        for i in 0..8 {
            assert_eq!(c.read() & 1, 1, "unstrobed read {i} is not 1");
        }
        assert_eq!(c.read() & 1, 1, "and past the eighth");
    }

    #[test]
    fn only_the_held_button_reads_back_one() {
        let mut c = Controller::new();
        c.set_buttons(0x08); // Start
        c.write_strobe(1);
        c.write_strobe(0);
        let bits: Vec<u8> = (0..8).map(|_| c.read() & 1).collect();
        assert_eq!(
            bits,
            [0, 0, 0, 1, 0, 0, 0, 0],
            "A,B,Select,Start,Up,Down,Left,Right"
        );
    }

    #[test]
    fn the_reads_run_out_after_eight() {
        let mut c = Controller::new();
        c.set_buttons(0);
        c.write_strobe(1);
        c.write_strobe(0);
        for _ in 0..8 {
            assert_eq!(c.read() & 1, 0);
        }
        assert_eq!(c.read() & 1, 1, "ninth read");
        assert_eq!(c.read() & 1, 1, "tenth read");
    }

    #[test]
    fn a_high_strobe_keeps_reporting_a() {
        let mut c = Controller::new();
        c.set_buttons(0x01);
        c.write_strobe(1);
        assert_eq!(c.read() & 1, 1);
        assert_eq!(c.read() & 1, 1, "strobe never shifts");
        c.write_strobe(0);
        c.set_buttons(0x01);
        c.write_strobe(1);
        c.write_strobe(0);
        assert_eq!(c.read() & 1, 1, "A again after a fresh strobe");
    }

    #[test]
    fn a_strobe_reloads_the_shift_register() {
        let mut c = Controller::new();
        c.set_buttons(0x01 | 0x02);
        c.write_strobe(1);
        c.write_strobe(0);
        assert_eq!(c.read() & 1, 1); // A
        assert_eq!(c.read() & 1, 1); // B
        c.write_strobe(1);
        c.write_strobe(0);
        assert_eq!(c.read() & 1, 1, "A again, so the strobe reloaded");
    }
}
