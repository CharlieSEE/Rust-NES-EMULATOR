//! Versioned save states.
//!
//! A state is a fixed header, a payload written by the machine's own types, and
//! a checksum:
//!
//! ```text
//! magic      8   b"NESSTATE"
//! version    2   u16, STATE_VERSION
//! mapper     1   iNES mapper number
//! region     1   0 NTSC, 1 PAL
//! fingerprint 8  FNV-1a of the iNES header, PRG and CHR bytes
//! length     4   payload bytes, little endian
//! payload    n   CPU, bus, PPU, APU, cartridge registers, PRG RAM
//! checksum   8   FNV-1a of the payload
//! ```
//!
//! The identity fields are checked before a single byte of the machine is
//! restored, and `Bus::load_state` applies the payload to a clone, so a corrupt
//! or incompatible file leaves the running game untouched. The ROM bytes are
//! not in the state -- the cartridge must already be the one the state names.
//! The framebuffer and the APU's not-yet-played sample queue are outputs, not
//! emulation state, and are rebuilt by the next frame.
//!
//! The format is hand-written rather than derived: every field is explicit, so
//! a future version can move one and say so, and the crate keeps its
//! no-serialization-dependency rule.

use crate::region::Region;

/// The only format this build writes and reads. A state from another version is
/// refused with the version in the message, never reinterpreted.
pub(crate) const STATE_VERSION: u16 = 1;

const MAGIC: &[u8; 8] = b"NESSTATE";

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv_update(mut hash: u64, bytes: &[u8]) -> u64 {
    for &byte in bytes {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn fnv(bytes: &[u8]) -> u64 {
    fnv_update(FNV_OFFSET, bytes)
}

/// Identity of the exact ROM a state came from: the iNES header plus PRG and
/// CHR bytes. Lengths are folded in so a PRG/CHR split cannot collide with a
/// differently split image that happens to have the same concatenated bytes.
pub(crate) fn rom_fingerprint(header: &[u8], prg: &[u8], chr: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET;
    for part in [header, prg, chr] {
        hash = fnv_update(hash, &(part.len() as u64).to_le_bytes());
        hash = fnv_update(hash, part);
    }
    hash
}

pub(crate) fn region_id(region: Region) -> u8 {
    match region {
        Region::Ntsc => 0,
        Region::Pal => 1,
    }
}

fn region_name(id: u8) -> &'static str {
    match id {
        0 => "NTSC",
        1 => "PAL",
        _ => "unknown",
    }
}

/// Writes the primitives a state is made of. Little endian, no padding: the
/// byte layout is the format, and `StateReader` mirrors every method here.
#[derive(Default)]
pub(crate) struct StateWriter {
    bytes: Vec<u8>,
}

impl StateWriter {
    pub(crate) fn new() -> StateWriter {
        StateWriter::default()
    }

    pub(crate) fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub(crate) fn bool(&mut self, value: bool) {
        self.u8(value as u8);
    }

    pub(crate) fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub(crate) fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub(crate) fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub(crate) fn bytes(&mut self, value: &[u8]) {
        self.bytes.extend_from_slice(value);
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

/// Bounds-checked reads over a state or a payload. Every failure names the byte
/// offset, because "truncated save state" with no position is a support ticket.
pub(crate) struct StateReader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> StateReader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> StateReader<'a> {
        StateReader { bytes, at: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let end = self
            .at
            .checked_add(count)
            .ok_or_else(|| format!("save state is truncated at byte {}", self.at))?;
        if end > self.bytes.len() {
            return Err(format!(
                "save state is truncated at byte {}: needed {count} more bytes, {} present",
                self.at,
                self.bytes.len().saturating_sub(self.at)
            ));
        }
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn bool(&mut self) -> Result<bool, String> {
        let at = self.at;
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(format!(
                "save state has invalid boolean {other} at byte {at}"
            )),
        }
    }

    pub(crate) fn u16(&mut self) -> Result<u16, String> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, String> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub(crate) fn u64(&mut self) -> Result<u64, String> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        let mut array = [0; N];
        array.copy_from_slice(self.take(N)?);
        Ok(array)
    }

    /// A variable-length byte run. The caller has already read and validated
    /// the length; the slice is borrowed from the file, not copied.
    pub(crate) fn bytes(&mut self, count: usize) -> Result<&'a [u8], String> {
        self.take(count)
    }

    pub(crate) fn expect_end(&self) -> Result<(), String> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err(format!(
                "save state has {} trailing bytes at offset {}",
                self.bytes.len() - self.at,
                self.at
            ))
        }
    }
}

/// A whole state file: header, then the payload the caller wrote.
pub(crate) fn wrap(payload: &[u8], mapper: u8, region: Region, fingerprint: u64) -> Vec<u8> {
    let mut writer = StateWriter::new();
    writer.bytes(MAGIC);
    writer.u16(STATE_VERSION);
    writer.u8(mapper);
    writer.u8(region_id(region));
    writer.u64(fingerprint);
    writer.u32(payload.len() as u32);
    writer.bytes(payload);
    writer.u64(fnv(payload));
    writer.finish()
}

/// Validate everything that decides whether this state can be applied at all,
/// then hand back a reader positioned at the start of the payload. Nothing has
/// been restored yet when this returns.
pub(crate) fn unwrap<'a>(
    data: &'a [u8],
    mapper: u8,
    region: Region,
    fingerprint: u64,
) -> Result<StateReader<'a>, String> {
    let mut reader = StateReader::new(data);
    if reader.take(MAGIC.len())? != MAGIC {
        return Err("not a NES save state: bad magic".to_string());
    }
    let version = reader.u16()?;
    if version != STATE_VERSION {
        return Err(format!(
            "save state version {version} is not supported; this build reads version {STATE_VERSION}"
        ));
    }
    let saved_mapper = reader.u8()?;
    if saved_mapper != mapper {
        return Err(format!(
            "save state is for mapper {saved_mapper}, this cartridge is mapper {mapper}"
        ));
    }
    let saved_region = reader.u8()?;
    if saved_region != region_id(region) {
        return Err(format!(
            "save state is for a {} console, this cartridge is {}",
            region_name(saved_region),
            region_name(region_id(region))
        ));
    }
    let saved_fingerprint = reader.u64()?;
    if saved_fingerprint != fingerprint {
        return Err("save state belongs to a different cartridge".to_string());
    }
    let length = reader.u32()? as usize;
    let payload = reader.take(length)?;
    let checksum = reader.u64()?;
    if checksum != fnv(payload) {
        return Err("save state checksum mismatch: the file is corrupt".to_string());
    }
    reader.expect_end()?;
    Ok(StateReader::new(payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAYLOAD: &[u8] = &[1, 2, 3, 4, 5];

    fn wrapped() -> Vec<u8> {
        wrap(PAYLOAD, 1, Region::Pal, 0x1234_5678_9abc_def0)
    }

    #[test]
    fn a_wrapped_state_unwraps_to_its_payload() {
        let bytes = wrapped();
        let mut reader =
            unwrap(&bytes, 1, Region::Pal, 0x1234_5678_9abc_def0).expect("header must match");
        assert_eq!(reader.take(PAYLOAD.len()).unwrap(), PAYLOAD);
        reader.expect_end().expect("payload consumed exactly");
    }

    #[test]
    fn a_state_from_another_version_mapper_region_or_rom_is_refused() {
        let bytes = wrapped();
        let mut wrong_version = bytes.clone();
        wrong_version[8..10].copy_from_slice(&2u16.to_le_bytes());
        let err = unwrap(&wrong_version, 1, Region::Pal, 0x1234_5678_9abc_def0)
            .err()
            .expect("version 2 must not load");
        assert!(err.contains('2'), "error names the version: {err}");

        assert!(unwrap(&bytes, 2, Region::Pal, 0x1234_5678_9abc_def0).is_err());
        assert!(unwrap(&bytes, 1, Region::Ntsc, 0x1234_5678_9abc_def0).is_err());
        assert!(unwrap(&bytes, 1, Region::Pal, 0).is_err());
    }

    #[test]
    fn a_truncated_or_corrupt_state_is_refused_with_its_offset() {
        let bytes = wrapped();
        let err = unwrap(
            &bytes[..bytes.len() - 1],
            1,
            Region::Pal,
            0x1234_5678_9abc_def0,
        )
        .err()
        .expect("truncated");
        assert!(err.contains("truncated"), "{err}");
        assert!(err.contains("byte"), "error names the offset: {err}");

        let mut corrupt = bytes.clone();
        let payload_at = 8 + 2 + 1 + 1 + 8 + 4;
        corrupt[payload_at] ^= 0xff;
        let err = unwrap(&corrupt, 1, Region::Pal, 0x1234_5678_9abc_def0)
            .err()
            .expect("corrupt");
        assert!(err.contains("checksum"), "{err}");

        let mut trailing = bytes.clone();
        trailing.push(0);
        let err = unwrap(&trailing, 1, Region::Pal, 0x1234_5678_9abc_def0)
            .err()
            .expect("trailing");
        assert!(err.contains("trailing"), "{err}");
    }

    #[test]
    fn a_fingerprint_sees_the_header_both_halves_and_the_split() {
        let header = [0u8; 16];
        let same = rom_fingerprint(&header, b"abc", b"def");
        assert_eq!(same, rom_fingerprint(&header, b"abc", b"def"));
        assert_ne!(same, rom_fingerprint(&[1u8; 16], b"abc", b"def"));
        assert_ne!(same, rom_fingerprint(&header, b"abcd", b"ef"));
        assert_ne!(same, rom_fingerprint(&header, b"abc", b"deg"));
        assert_ne!(same, rom_fingerprint(&header, b"ab", b"cdef"));
    }
}
