//! Just enough H.264 (Annex-B) to hand XMouse access units it can start decoding from.
//!
//! The console sends its parameter sets (SPS/PPS: picture size and coding options) once, in
//! the stream description, and chiaki passes them on as a first "sample" by themselves. Its
//! pictures then follow without them. A decoder that starts later, or restarts after an
//! error, needs the parameter sets again, so they are remembered here and put in front of
//! every full picture (IDR). Nothing is decoded or rewritten.

/// NAL unit types (the low five bits of the byte after a start code).
const NAL_SLICE: u8 = 1;
const NAL_IDR: u8 = 5;
const NAL_SPS: u8 = 7;
const NAL_PPS: u8 = 8;

/// Parameter sets are a few dozen bytes; refuse to remember something that clearly isn't.
const MAX_HEADER: usize = 4096;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Contents {
    /// A full picture: decoding can start here.
    pub idr: bool,
    /// Any coded picture data (IDR or not).
    pub picture: bool,
    pub sps: bool,
    pub pps: bool,
}

/// Which kinds of NAL units are in this Annex-B data.
pub fn scan(data: &[u8]) -> Contents {
    let mut found = Contents::default();
    let mut i = 0;
    // A start code is 00 00 01 (a fourth leading zero is just padding in front of it).
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            match data[i + 3] & 0x1f {
                NAL_IDR => {
                    found.idr = true;
                    found.picture = true;
                }
                NAL_SLICE..=4 => found.picture = true,
                NAL_SPS => found.sps = true,
                NAL_PPS => found.pps = true,
                _ => {}
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    found
}

/// chiaki hands out its buffers with zero padding at the end (room a decoder library may read
/// past). A NAL unit never ends in a zero byte, so the padding can be cut off safely.
pub fn trim_padding(data: &[u8]) -> &[u8] {
    let end = data.iter().rposition(|&b| b != 0).map_or(0, |p| p + 1);
    &data[..end]
}

/// One access unit ready to send: `prefix` (stored parameter sets, possibly empty) followed
/// by `body`.
#[derive(Debug, PartialEq, Eq)]
pub struct Unit<'a> {
    pub keyframe: bool,
    pub prefix: &'a [u8],
    pub body: &'a [u8],
}

/// Remembers the stream's parameter sets and attaches them to full pictures.
#[derive(Default)]
pub struct Assembler {
    header: Vec<u8>,
}

impl Assembler {
    /// Take one sample from chiaki. `None`: nothing to send (it was only parameter sets, now
    /// remembered, or it was empty).
    pub fn push<'a>(&'a mut self, sample: &'a [u8]) -> Option<Unit<'a>> {
        let body = trim_padding(sample);
        if body.is_empty() {
            return None;
        }
        let contents = scan(body);
        if !contents.picture {
            if contents.sps || contents.pps {
                if body.len() <= MAX_HEADER {
                    self.header.clear();
                    self.header.extend_from_slice(body);
                }
                return None;
            }
            // Not something this code recognises: pass it on untouched rather than guess.
            return Some(Unit { keyframe: false, prefix: &[], body });
        }
        // A picture that brings its own parameter sets doesn't need ours in front.
        let prefix: &[u8] = if contents.idr && !contents.sps { &self.header } else { &[] };
        Some(Unit { keyframe: contents.idr, prefix, body })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS_PPS: &[u8] = &[0, 0, 0, 1, 0x67, 0x42, 0xc0, 0x1e, 0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80];
    const IDR: &[u8] = &[0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21];
    const P: &[u8] = &[0, 0, 1, 0x41, 0x9a, 0x02];

    fn padded(data: &[u8]) -> Vec<u8> {
        let mut v = data.to_vec();
        v.extend_from_slice(&[0u8; 64]);
        v
    }

    #[test]
    fn nal_types_are_found_with_both_start_code_lengths() {
        assert_eq!(scan(SPS_PPS), Contents { sps: true, pps: true, ..Default::default() });
        assert_eq!(scan(IDR), Contents { idr: true, picture: true, ..Default::default() });
        assert_eq!(scan(P), Contents { picture: true, ..Default::default() });
        // nal_ref_idc bits (the top of the byte) don't change the type.
        assert!(scan(&[0, 0, 1, 0x25, 1]).idr);
        assert!(scan(&[0, 0, 1, 0x01, 1]).picture);
        let mut both = SPS_PPS.to_vec();
        both.extend_from_slice(IDR);
        assert_eq!(scan(&both), Contents { idr: true, picture: true, sps: true, pps: true });
    }

    #[test]
    fn scanning_garbage_never_panics() {
        assert_eq!(scan(&[]), Contents::default());
        assert_eq!(scan(&[0]), Contents::default());
        assert_eq!(scan(&[0, 0, 1]), Contents::default(), "start code with nothing after it");
        assert_eq!(scan(&[0, 0, 0, 0, 0, 0]), Contents::default());
        assert_eq!(scan(&[0xff; 100]), Contents::default());
        assert_eq!(trim_padding(&[]), &[] as &[u8]);
        assert_eq!(trim_padding(&[0, 0, 0]), &[] as &[u8]);
        assert_eq!(trim_padding(&[0, 0, 1, 0x65, 0x80, 0, 0]), &[0, 0, 1, 0x65, 0x80]);
    }

    #[test]
    fn parameter_sets_are_held_back_and_put_in_front_of_full_pictures() {
        let mut a = Assembler::default();
        // First comes chiaki's header sample: nothing to send yet.
        assert_eq!(a.push(&padded(SPS_PPS)), None);
        // The first full picture gets it in front and is marked as a keyframe.
        let idr = padded(IDR);
        let unit = a.push(&idr).unwrap();
        assert_eq!(unit, Unit { keyframe: true, prefix: SPS_PPS, body: IDR });
        // Ordinary pictures go out as they are.
        let p = padded(P);
        assert_eq!(a.push(&p).unwrap(), Unit { keyframe: false, prefix: &[], body: P });
        // A later full picture (after a keyframe request) gets the header again.
        assert_eq!(a.push(&idr).unwrap().prefix, SPS_PPS);
    }

    #[test]
    fn a_picture_with_its_own_parameter_sets_is_not_doubled() {
        let mut a = Assembler::default();
        assert_eq!(a.push(SPS_PPS), None);
        let mut whole = SPS_PPS.to_vec();
        whole.extend_from_slice(IDR);
        let unit = a.push(&whole).unwrap();
        assert!(unit.keyframe);
        assert!(unit.prefix.is_empty());
        assert_eq!(unit.body, &whole[..]);
    }

    #[test]
    fn odd_samples_are_handled_without_panicking() {
        let mut a = Assembler::default();
        assert_eq!(a.push(&[]), None);
        assert_eq!(a.push(&[0; 64]), None);
        // A full picture before any header: sent with nothing in front.
        let unit = a.push(IDR).unwrap();
        assert!(unit.keyframe && unit.prefix.is_empty());
        // Unrecognised data is passed through, not marked as a keyframe.
        let unit = a.push(&[1, 2, 3]).unwrap();
        assert_eq!(unit, Unit { keyframe: false, prefix: &[], body: &[1, 2, 3] });
        // An absurdly large "header" is not remembered.
        let mut huge = SPS_PPS.to_vec();
        huge.resize(MAX_HEADER + 10, 0xaa);
        assert_eq!(a.push(&huge), None);
        assert!(a.push(IDR).unwrap().prefix.is_empty());
    }
}
