//! The length of one AAC-LC packet (a `raw_data_block`, ISO/IEC 14496-3
//! 4.4.2.1) read off its bits, for a stream that has no sample table to say
//! where each packet ends: the packet's syntax elements are walked to its
//! `END` element, decoding every Huffman codeword on the way but none of
//! the audio, and the packet ends at the next byte.
//!
//! Only the shape the phone's recorder writes is accepted (mono AAC-LC):
//! one single channel element (`SCE`), any fill (`FIL`) and data stream
//! (`DSE`) elements, and `END`, so a stream of more than one channel is
//! refused. A channel pair, a coupling or low-frequency channel, a program
//! config, predictor data or gain control (neither of which AAC-LC has), a
//! reserved codebook, a section past the last band, a scale factor out of
//! its range, an impossible escape or a packet longer than
//! [`MAX_PACKET_BYTES`] is [`PacketError::Invalid`]: bytes that are not a
//! packet of that shape, which ends the salvage there.
//!
//! The rate is not in the bits: 44.1 and 48 kHz share their band tables, so
//! a packet at either parses the same. The salvage takes the rate from the
//! phone's fixed preset instead ([`super::SAMPLE_RATE`]).

use std::sync::LazyLock;

use super::tables::{self, Codebook, LONG_BANDS, SHORT_BANDS};

/// The longest packet a mono AAC stream may hold: the decoder's input buffer
/// of 6 144 bits per channel (ISO/IEC 14496-3 4.5.3.1).
pub const MAX_PACKET_BYTES: usize = 6_144 / 8;

/// Why bytes are not one whole packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketError {
    /// The bytes end inside the packet: a recording cut off mid-packet.
    Truncated,
    /// Not a packet of the phone's shape, and why.
    Invalid(&'static str),
}

/// The length in bytes of the packet that starts `bytes`; `bytes` may run on
/// past it.
pub fn packet_length(bytes: &[u8]) -> Result<usize, PacketError> {
    let mut bits = Bits::new(bytes);
    let mut channels = 0;
    loop {
        match bits.read(3)? {
            SCE => {
                if channels > 0 {
                    return Err(PacketError::Invalid("a second channel"));
                }
                // The element's instance tag.
                bits.read(4)?;
                channel_stream(&mut bits)?;
                channels += 1;
            }
            DSE => {
                bits.read(4)?;
                let aligned = bits.bit()?;
                let mut count = bits.read(8)?;
                if count == 255 {
                    count += bits.read(8)?;
                }
                if aligned {
                    bits.align();
                }
                bits.skip(count as usize * 8)?;
            }
            FIL => {
                let mut count = bits.read(4)?;
                if count == 15 {
                    // 15 + the escape count - 1 (Table 4.11).
                    count = count + bits.read(8)? - 1;
                }
                bits.skip(count as usize * 8)?;
            }
            END => {
                if channels == 0 {
                    return Err(PacketError::Invalid("a packet without a channel"));
                }
                return Ok(bits.at.div_ceil(8));
            }
            _ => {
                return Err(PacketError::Invalid(
                    "an element the phone's recorder does not write",
                ));
            }
        }
    }
}

/// Element ids (ISO/IEC 14496-3 Table 4.85).
const SCE: u32 = 0;
const DSE: u32 = 4;
const FIL: u32 = 6;
const END: u32 = 7;

/// Section codebooks (Table 4.86) that carry no spectral data.
const ZERO_HCB: u8 = 0;
const RESERVED_HCB: u8 = 12;
const NOISE_HCB: u8 = 13;
const INTENSITY_HCB2: u8 = 14;
const INTENSITY_HCB: u8 = 15;

/// The window sequence of eight short windows.
const EIGHT_SHORT_SEQUENCE: u32 = 2;

/// The highest TNS filter order AAC-LC allows, in long and in short
/// windows (Table 4.139).
const TNS_MAX_ORDER_LONG: u32 = 12;
const TNS_MAX_ORDER_SHORT: u32 = 7;

/// An `individual_channel_stream` (4.4.2.7) outside a channel pair, so with
/// its own `ics_info`.
fn channel_stream(bits: &mut Bits<'_>) -> Result<(), PacketError> {
    let global_gain = bits.read(8)?;
    let window = ics_info(bits)?;
    let codebooks = section_data(bits, &window)?;
    scale_factor_data(bits, &window, &codebooks, global_gain)?;
    pulse_data(bits, &window)?;
    tns_data(bits, &window)?;
    // gain_control_data, which only AAC SSR has.
    if bits.bit()? {
        return Err(PacketError::Invalid(
            "gain control, which AAC-LC has none of",
        ));
    }
    spectral_data(bits, &window, &codebooks)
}

/// A channel's windows, as its `ics_info` gives them.
struct Window {
    short: bool,
    /// The bands coded, at most the window's.
    max_sfb: usize,
    /// Where each band starts, in lines of one window.
    bands: &'static [u16],
    /// Each window group's number of windows: one group of one window for
    /// a long window.
    groups: [u16; 8],
    group_count: usize,
}

impl Window {
    fn groups(&self) -> &[u16] {
        &self.groups[..self.group_count]
    }
}

/// Each window group's codebook per band.
type Codebooks = [[u8; LONG_BANDS.len() - 1]; 8];

/// `ics_info` (4.4.2.1, Table 4.6).
fn ics_info(bits: &mut Bits<'_>) -> Result<Window, PacketError> {
    if bits.bit()? {
        return Err(PacketError::Invalid("the reserved bit is set"));
    }
    let short = bits.read(2)? == EIGHT_SHORT_SEQUENCE;
    // The window shape.
    bits.bit()?;
    let mut groups = [1u16; 8];
    let mut group_count = 1;
    let (max_sfb, bands): (usize, &'static [u16]) = if short {
        let max_sfb = bits.read(4)? as usize;
        // scale_factor_grouping: a set bit adds the next window to the group.
        for _ in 0..7 {
            if bits.bit()? {
                groups[group_count - 1] += 1;
            } else {
                group_count += 1;
            }
        }
        (max_sfb, &SHORT_BANDS)
    } else {
        let max_sfb = bits.read(6)? as usize;
        if bits.bit()? {
            return Err(PacketError::Invalid(
                "predictor data, which AAC-LC has none of",
            ));
        }
        (max_sfb, &LONG_BANDS)
    };
    if max_sfb > bands.len() - 1 {
        return Err(PacketError::Invalid("more bands than the rate has"));
    }
    Ok(Window {
        short,
        max_sfb,
        bands,
        groups,
        group_count,
    })
}

/// `section_data` (Table 4.52): each band's codebook.
fn section_data(bits: &mut Bits<'_>, window: &Window) -> Result<Codebooks, PacketError> {
    let mut codebooks = [[ZERO_HCB; LONG_BANDS.len() - 1]; 8];
    let (length_bits, escape) = if window.short { (3, 7) } else { (5, 31) };
    for group in codebooks.iter_mut().take(window.group_count) {
        let mut band = 0;
        while band < window.max_sfb {
            let codebook = bits.read(4)? as u8;
            if codebook == RESERVED_HCB {
                return Err(PacketError::Invalid("the reserved codebook"));
            }
            let mut length = 0;
            loop {
                let increment = bits.read(length_bits)? as usize;
                length += increment;
                if increment != escape {
                    break;
                }
            }
            if length == 0 || band + length > window.max_sfb {
                return Err(PacketError::Invalid("a section past the last band"));
            }
            group[band..band + length].fill(codebook);
            band += length;
        }
    }
    Ok(codebooks)
}

/// `scale_factor_data` (Table 4.53): the first noise energy is 9 bits,
/// every other value a Huffman-coded difference from the last of its kind,
/// 60 meaning none. Only the ordinary scale factors' range is checked: 0 to
/// 255 (4.6.2.3.2).
fn scale_factor_data(
    bits: &mut Bits<'_>,
    window: &Window,
    codebooks: &Codebooks,
    global_gain: u32,
) -> Result<(), PacketError> {
    let mut scale_factor = i64::from(global_gain);
    let mut noise_pcm = true;
    for group in &codebooks[..window.group_count] {
        for &codebook in &group[..window.max_sfb] {
            match codebook {
                ZERO_HCB => {}
                NOISE_HCB if noise_pcm => {
                    noise_pcm = false;
                    bits.read(9)?;
                }
                // Intensity and noise positions: unchecked.
                INTENSITY_HCB | INTENSITY_HCB2 | NOISE_HCB => {
                    TREES.scale_factors.decode(bits)?;
                }
                _ => {
                    scale_factor += i64::from(TREES.scale_factors.decode(bits)?) - 60;
                    if !(0..=255).contains(&scale_factor) {
                        return Err(PacketError::Invalid("a scale factor out of range"));
                    }
                }
            }
        }
    }
    Ok(())
}

/// `pulse_data` (Table 4.7), in a long window only.
fn pulse_data(bits: &mut Bits<'_>, window: &Window) -> Result<(), PacketError> {
    if !bits.bit()? {
        return Ok(());
    }
    if window.short {
        return Err(PacketError::Invalid("pulse data in a short window"));
    }
    let pulses = bits.read(2)? + 1;
    if bits.read(6)? as usize >= LONG_BANDS.len() - 1 {
        return Err(PacketError::Invalid("pulses past the last band"));
    }
    // Each pulse's offset and amplitude.
    bits.skip(pulses as usize * (5 + 4))
}

/// `tns_data` (Table 4.48).
fn tns_data(bits: &mut Bits<'_>, window: &Window) -> Result<(), PacketError> {
    if !bits.bit()? {
        return Ok(());
    }
    let (windows, filter_bits, length_bits, order_bits, max_order) = if window.short {
        (8, 1, 4, 3, TNS_MAX_ORDER_SHORT)
    } else {
        (1, 2, 6, 5, TNS_MAX_ORDER_LONG)
    };
    for _ in 0..windows {
        let filters = bits.read(filter_bits)?;
        let coefficient_resolution = if filters > 0 { bits.read(1)? } else { 0 };
        for _ in 0..filters {
            bits.read(length_bits)?;
            let order = bits.read(order_bits)?;
            if order > max_order {
                return Err(PacketError::Invalid("a TNS filter of too high an order"));
            }
            if order > 0 {
                // The direction.
                bits.bit()?;
                let compressed = bits.read(1)?;
                let coefficient_bits = 3 + coefficient_resolution - compressed;
                bits.skip((order * coefficient_bits) as usize)?;
            }
        }
    }
    Ok(())
}

/// `spectral_data` (Table 4.56): a band's lines across its group's windows,
/// four (codebooks 1 to 4) or two (5 to 11) per codeword.
fn spectral_data(
    bits: &mut Bits<'_>,
    window: &Window,
    codebooks: &Codebooks,
) -> Result<(), PacketError> {
    for (group, &windows) in codebooks.iter().zip(window.groups()) {
        for (band, &codebook) in group[..window.max_sfb].iter().enumerate() {
            if matches!(
                codebook,
                ZERO_HCB | NOISE_HCB | INTENSITY_HCB | INTENSITY_HCB2
            ) {
                continue;
            }
            let lines =
                usize::from(window.bands[band + 1] - window.bands[band]) * usize::from(windows);
            let tree = &TREES.spectrum[usize::from(codebook) - 1];
            let per_codeword = if codebook <= 4 { 4 } else { 2 };
            for _ in 0..lines / per_codeword {
                spectral_codeword(bits, tree, codebook)?;
            }
        }
    }
    Ok(())
}

/// One spectral codeword of `codebook` and the sign and escape bits that
/// follow it (4.6.3.3).
fn spectral_codeword(bits: &mut Bits<'_>, tree: &Tree, codebook: u8) -> Result<(), PacketError> {
    let index = u32::from(tree.decode(bits)?);
    match codebook {
        // Signed codebooks: no sign bits.
        1 | 2 | 5 | 6 => {}
        // Unsigned quads, values 0 to 2: a sign bit per value that is not 0.
        3 | 4 => {
            let values = [index / 27, index / 9 % 3, index / 3 % 3, index % 3];
            bits.skip(values.iter().filter(|&&v| v != 0).count())?;
        }
        // Unsigned pairs, values 0 to 7 and 0 to 12.
        7..=10 => {
            let modulo = if codebook <= 8 { 8 } else { 13 };
            let values = [index / modulo, index % modulo];
            bits.skip(values.iter().filter(|&&v| v != 0).count())?;
        }
        // Codebook 11, values 0 to 16: the signs, then for each 16 an
        // escape, N ones, a zero and an (N + 4)-bit word, N at most 8.
        _ => {
            let values = [index / 17, index % 17];
            bits.skip(values.iter().filter(|&&v| v != 0).count())?;
            for _ in values.iter().filter(|&&v| v == 16) {
                let mut ones = 0;
                while bits.bit()? {
                    ones += 1;
                    if ones > 8 {
                        return Err(PacketError::Invalid("an escape past 8 191"));
                    }
                }
                bits.skip(ones + 4)?;
            }
        }
    }
    Ok(())
}

/// The bits of a packet, most significant first, never past
/// [`MAX_PACKET_BYTES`].
struct Bits<'a> {
    bytes: &'a [u8],
    /// Whether `bytes` was cut at [`MAX_PACKET_BYTES`]: running out of it
    /// then means a packet too long, not one cut off.
    capped: bool,
    /// The next bit's index.
    at: usize,
}

impl<'a> Bits<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        let capped = bytes.len() >= MAX_PACKET_BYTES;
        Self {
            bytes: &bytes[..bytes.len().min(MAX_PACKET_BYTES)],
            capped,
            at: 0,
        }
    }

    fn out_of_bits(&self) -> PacketError {
        if self.capped {
            PacketError::Invalid("longer than a packet can be")
        } else {
            PacketError::Truncated
        }
    }

    fn bit(&mut self) -> Result<bool, PacketError> {
        let byte = *self
            .bytes
            .get(self.at / 8)
            .ok_or_else(|| self.out_of_bits())?;
        let bit = byte >> (7 - self.at % 8) & 1 == 1;
        self.at += 1;
        Ok(bit)
    }

    /// The next `count` bits (at most 32) as a number.
    fn read(&mut self, count: u32) -> Result<u32, PacketError> {
        let mut value = 0;
        for _ in 0..count {
            value = value << 1 | u32::from(self.bit()?);
        }
        Ok(value)
    }

    fn skip(&mut self, count: usize) -> Result<(), PacketError> {
        let to = self.at + count;
        if to > self.bytes.len() * 8 {
            return Err(self.out_of_bits());
        }
        self.at = to;
        Ok(())
    }

    /// To the next byte, counted from the packet's start.
    fn align(&mut self) {
        self.at = self.at.next_multiple_of(8);
    }
}

/// A codebook as a binary tree, walked a bit at a time.
struct Tree {
    /// Each node's two children: 0 for none (the root is never a child),
    /// [`LEAF`] and the index for a codeword's end, else a node.
    nodes: Vec<[u16; 2]>,
}

/// Marks a child that ends a codeword.
const LEAF: u16 = 0x8000;

impl Tree {
    fn new(codebook: &Codebook) -> Self {
        let mut nodes = vec![[0u16; 2]];
        for (index, (&code, &length)) in codebook.codes.iter().zip(codebook.lens).enumerate() {
            let mut node = 0;
            for bit in (0..length).rev() {
                let side = (code >> bit & 1) as usize;
                if bit == 0 {
                    // Fewer than 300 indices in every codebook.
                    nodes[node][side] = LEAF | index as u16;
                } else {
                    if nodes[node][side] == 0 {
                        nodes.push([0; 2]);
                        nodes[node][side] = (nodes.len() - 1) as u16;
                    }
                    node = usize::from(nodes[node][side]);
                }
            }
        }
        Self { nodes }
    }

    /// The index of the codeword next in `bits`.
    fn decode(&self, bits: &mut Bits<'_>) -> Result<u16, PacketError> {
        let mut node = 0;
        loop {
            let next = self.nodes[node][usize::from(bits.bit()?)];
            if next == 0 {
                return Err(PacketError::Invalid("not a codeword"));
            }
            if next & LEAF != 0 {
                return Ok(next & !LEAF);
            }
            node = usize::from(next);
        }
    }
}

/// The codebooks as trees, built once.
struct Trees {
    spectrum: Vec<Tree>,
    scale_factors: Tree,
}

static TREES: LazyLock<Trees> = LazyLock::new(|| Trees {
    spectrum: tables::SPECTRUM.iter().map(Tree::new).collect(),
    scale_factors: Tree::new(&tables::SCALE_FACTORS),
});

#[cfg(test)]
mod tests {
    use super::*;

    /// Every codebook is a complete prefix code: no codeword is the start
    /// of another, and their lengths fill the code space exactly (Kraft's
    /// sum is 1), so a mistyped codeword or length shows here.
    #[test]
    fn every_codebook_is_a_complete_prefix_code() {
        for (number, codebook) in tables::SPECTRUM
            .iter()
            .chain([&tables::SCALE_FACTORS])
            .enumerate()
        {
            assert_eq!(codebook.codes.len(), codebook.lens.len());
            let mut kraft = 0u64;
            for (i, (&code, &length)) in codebook.codes.iter().zip(codebook.lens).enumerate() {
                assert!((1..=19).contains(&length), "codebook {number} index {i}");
                assert!(u64::from(code) < 1 << length, "codebook {number} index {i}");
                kraft += 1 << (19 - length);
                for (j, (&other, &other_length)) in
                    codebook.codes.iter().zip(codebook.lens).enumerate()
                {
                    if i != j && other_length >= length {
                        assert_ne!(
                            other >> (other_length - length),
                            code,
                            "codebook {number}: {i} is a prefix of {j}"
                        );
                    }
                }
            }
            assert_eq!(kraft, 1 << 19, "codebook {number}");
        }
    }

    /// `parts` of '0' and '1' as bytes, zero-padded to the next byte, and
    /// the bytes a packet of exactly those bits takes.
    fn bits(parts: &[&str]) -> (Vec<u8>, usize) {
        let text: String = parts.concat();
        let bytes = text
            .as_bytes()
            .chunks(8)
            .map(|chunk| {
                chunk
                    .iter()
                    .enumerate()
                    .fold(0u8, |byte, (i, &c)| byte | (u8::from(c == b'1') << (7 - i)))
            })
            .collect();
        (bytes, text.len().div_ceil(8))
    }

    /// The codeword of `index` in `codebook` as '0' and '1'.
    fn code(codebook: &Codebook, index: usize) -> String {
        let (code, length) = (codebook.codes[index], usize::from(codebook.lens[index]));
        format!("{code:0length$b}")
    }

    fn spectrum(book: usize, index: usize) -> String {
        code(&tables::SPECTRUM[book - 1], index)
    }

    /// The scale factor difference `difference` (60 is none).
    fn scale(difference: usize) -> String {
        code(&tables::SCALE_FACTORS, difference)
    }

    /// The bits of a channel's start: SCE, tag 0, global gain 100, then a
    /// long window's `ics_info` with `bands` bands and no predictor.
    fn long(bands: u32) -> String {
        format!("000 0000 01100100 0 00 0 {bands:06b} 0").replace(' ', "")
    }

    /// No pulse, TNS or gain control.
    const NO_TOOLS: &str = "000";
    const END_BITS: &str = "111";

    #[test]
    fn a_long_window_walks_its_sections_scale_factors_and_spectrum() {
        // One band of codebook 1 (signed quads, four lines a codeword).
        let zero_quad = spectrum(1, 40);
        let (packet, length) = bits(&[
            &long(1),
            "0001",
            "00001",
            &scale(60),
            NO_TOOLS,
            &zero_quad,
            END_BITS,
        ]);
        assert_eq!(packet_length(&packet), Ok(length));
        // Two bands, the second of width 4 too: twice the codewords.
        let (packet, length) = bits(&[
            &long(2),
            "0001",
            "00010",
            &scale(60),
            &scale(61),
            NO_TOOLS,
            &zero_quad,
            &zero_quad,
            END_BITS,
        ]);
        assert_eq!(packet_length(&packet), Ok(length));
        // Sections of codebook 0 and 1, the first's length escaped (31 + 10);
        // band 41 (lines 704 to 736) is 32 lines: eight quads.
        let quads = [zero_quad.as_str(); 8];
        let mut parts = vec![
            long(42),
            "0000".into(),
            "11111".into(),
            "01010".into(),
            "0001".into(),
            "00001".into(),
            scale(60),
            NO_TOOLS.into(),
        ];
        parts.extend(quads.iter().map(|q| (*q).to_owned()));
        parts.push(END_BITS.into());
        let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
        let (packet, length) = bits(&parts);
        assert_eq!(packet_length(&packet), Ok(length));
    }

    #[test]
    fn unsigned_codebooks_read_a_sign_per_value_that_is_not_zero() {
        // Codebook 3, index 1 is (0, 0, 0, 1): one sign bit; index 40,
        // (1, 1, 1, 1): four.
        for (index, signs) in [(0, ""), (1, "1"), (40, "0101")] {
            let (packet, length) = bits(&[
                &long(1),
                "0011",
                "00001",
                &scale(60),
                NO_TOOLS,
                &spectrum(3, index),
                signs,
                END_BITS,
            ]);
            assert_eq!(packet_length(&packet), Ok(length), "index {index}");
        }
        // Codebook 7 (pairs 0 to 7), index 9 is (1, 1); codebook 9 (0 to
        // 12), index 14 is (1, 1); codebook 5 is signed: none.
        for (book, index, signs) in [(7, 9, "01"), (9, 14, "10"), (7, 8, "1"), (5, 0, "")] {
            let pair = spectrum(book, index);
            let (packet, length) = bits(&[
                &long(1),
                &format!("{book:04b}"),
                "00001",
                &scale(60),
                NO_TOOLS,
                &pair,
                signs,
                &spectrum(book, if book == 5 { 40 } else { 0 }),
                END_BITS,
            ]);
            assert_eq!(
                packet_length(&packet),
                Ok(length),
                "codebook {book} index {index}"
            );
        }
    }

    #[test]
    fn codebook_11_escapes_a_16() {
        // (16, 0), its sign, an escape of N ones, a zero and N + 4 bits;
        // then (0, 0).
        for (escape, valid) in [
            ("00000", true),
            ("110101010", true),
            ("1111111110000000000000", false),
        ] {
            let (packet, length) = bits(&[
                &long(1),
                "1011",
                "00001",
                &scale(60),
                NO_TOOLS,
                &spectrum(11, 16 * 17),
                "0",
                escape,
                &spectrum(11, 0),
                END_BITS,
            ]);
            if valid {
                assert_eq!(packet_length(&packet), Ok(length), "{escape}");
            } else {
                assert_eq!(
                    packet_length(&packet),
                    Err(PacketError::Invalid("an escape past 8 191"))
                );
            }
        }
        // (16, 16): both signs, then both escapes.
        let (packet, length) = bits(&[
            &long(1),
            "1011",
            "00001",
            &scale(60),
            NO_TOOLS,
            &spectrum(11, 16 * 17 + 16),
            "01",
            "00000",
            "1000000",
            &spectrum(11, 0),
            END_BITS,
        ]);
        assert_eq!(packet_length(&packet), Ok(length));
    }

    #[test]
    fn short_windows_count_each_groups_windows() {
        let short = |max_sfb: &str, grouping: &str| {
            format!("000 0000 01100100 0 10 0 {max_sfb} {grouping}").replace(' ', "")
        };
        let zero_quad = spectrum(1, 40);
        // One group of all eight windows: band 0 is 4 lines in each, so
        // eight quads; three bits per section length.
        let mut parts = vec![
            short("0001", "1111111"),
            "0001".into(),
            "001".into(),
            scale(60),
            NO_TOOLS.into(),
        ];
        parts.extend(std::iter::repeat_n(zero_quad.clone(), 8));
        parts.push(END_BITS.into());
        let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
        let (packet, length) = bits(&refs);
        assert_eq!(packet_length(&packet), Ok(length));
        // Groups of 2 and 6 windows ("1011111"): band 0 of codebook 1 in
        // the first (two quads), of codebook 0 in the second.
        let (packet, length) = bits(&[
            &short("0001", "1011111"),
            "0001",
            "001",
            "0000",
            "001",
            &scale(60),
            NO_TOOLS,
            &zero_quad,
            &zero_quad,
            END_BITS,
        ]);
        assert_eq!(packet_length(&packet), Ok(length));
        // Eight groups of one window: a section each.
        let mut parts = vec![short("0001", "0000000")];
        for _ in 0..8 {
            parts.extend(["0001".to_owned(), "001".to_owned()]);
        }
        parts.extend(std::iter::repeat_n(scale(60), 8));
        parts.push(NO_TOOLS.into());
        parts.extend(std::iter::repeat_n(zero_quad.clone(), 8));
        parts.push(END_BITS.into());
        let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
        let (packet, length) = bits(&refs);
        assert_eq!(packet_length(&packet), Ok(length));
        // 15 bands is one past a short window's 14; pulses are long only.
        let (packet, _) = bits(&[&short("1111", "1111111"), "0000", "111", "111", END_BITS]);
        assert_eq!(
            packet_length(&packet),
            Err(PacketError::Invalid("more bands than the rate has"))
        );
        let (packet, _) = bits(&[
            &short("0000", "1111111"),
            "1",
            "00",
            "000000",
            "000000000",
            "00",
            END_BITS,
        ]);
        assert_eq!(
            packet_length(&packet),
            Err(PacketError::Invalid("pulse data in a short window"))
        );
        // TNS in a short window: one filter bit per window; window 0 has
        // one filter of order 1 (4-bit length, 3-bit order), 3-bit coefficients.
        let (packet, length) = bits(&[
            &short("0000", "1111111"),
            "0",
            "1",
            "1",
            "0",
            "0000",
            "001",
            "0",
            "0",
            "000",
            "0000000",
            "0",
            END_BITS,
        ]);
        assert_eq!(packet_length(&packet), Ok(length));
    }

    #[test]
    fn pulses_tns_noise_and_intensity_take_their_bits() {
        // Two pulses from band 3: their offsets and amplitudes.
        let (packet, length) = bits(&[
            &long(0),
            "1",
            "01",
            "000011",
            "000010000",
            "000110001",
            "0",
            "0",
            END_BITS,
        ]);
        assert_eq!(packet_length(&packet), Ok(length));
        let (packet, _) = bits(&[&long(0), "1", "00", "110001", "000000000", "00", END_BITS]);
        assert_eq!(
            packet_length(&packet),
            Err(PacketError::Invalid("pulses past the last band"))
        );
        // TNS, long: two filters, 4-bit coefficients compressed to 3, orders
        // 2 and 0.
        let (packet, length) = bits(&[
            &long(0),
            "0",
            "1",
            "10",
            "1",
            "000010",
            "00010",
            "1",
            "1",
            "000111",
            "000011",
            "00000",
            "0",
            END_BITS,
        ]);
        assert_eq!(packet_length(&packet), Ok(length));
        let (packet, _) = bits(&[
            &long(0),
            "0",
            "1",
            "01",
            "0",
            "000010",
            "01101",
            "0000000000000000",
            END_BITS,
        ]);
        assert_eq!(
            packet_length(&packet),
            Err(PacketError::Invalid("a TNS filter of too high an order"))
        );
        // Noise in bands 0 and 1 (the first energy 9 bits, the next a
        // codeword), intensity in band 2: no spectral bits for either.
        let (packet, length) = bits(&[
            &long(3),
            "1101",
            "00010",
            "1111",
            "00001",
            "100000000",
            &scale(61),
            &scale(59),
            NO_TOOLS,
            END_BITS,
        ]);
        assert_eq!(packet_length(&packet), Ok(length));
    }

    #[test]
    fn fill_and_data_stream_elements_are_skipped() {
        let silent = "00000000110100000000000000000"; // SCE of a silent long window
        // FIL with an escaped count: 15 + 2 - 1 = 16 bytes.
        let filler = format!("110 1111 00000010 {}", "0".repeat(16 * 8)).replace(' ', "");
        let (packet, length) = bits(&[silent, &filler, END_BITS]);
        assert_eq!(packet_length(&packet), Ok(length));
        let (packet, length) = bits(&[&filler, silent, END_BITS]);
        assert_eq!(packet_length(&packet), Ok(length));
        // An escape count of 0: 14 bytes.
        let (packet, length) = bits(&[
            "110 1111 00000000".replace(' ', "").as_str(),
            &"0".repeat(14 * 8),
            silent,
            END_BITS,
        ]);
        assert_eq!(packet_length(&packet), Ok(length));
        // DSE of 3 bytes, aligned: the bits to the next byte, then the data.
        let data = "100 0000 1 00000011".replace(' ', "");
        let before = silent.len() + data.len();
        let padding = "0".repeat(before.next_multiple_of(8) - before);
        let (packet, length) = bits(&[
            silent,
            &data,
            &padding,
            "101010101010101010101010",
            END_BITS,
        ]);
        assert_eq!(packet_length(&packet), Ok(length));
        // The same unaligned is three bytes later by the padding's bits.
        let (packet, length) = bits(&[
            silent,
            "100 0000 0 00000011".replace(' ', "").as_str(),
            &"0".repeat(24),
            END_BITS,
        ]);
        assert_eq!(packet_length(&packet), Ok(length));
        // A DSE of 255 + 255 bytes, twice, is longer than a packet can be.
        let long_data =
            format!("100 0000 0 11111111 11111111 {}", "0".repeat(510 * 8)).replace(' ', "");
        let (packet, _) = bits(&[silent, &long_data, &long_data, END_BITS]);
        assert_eq!(
            packet_length(&packet),
            Err(PacketError::Invalid("longer than a packet can be"))
        );
    }

    #[test]
    fn what_aac_lc_or_the_phone_never_writes_is_invalid() {
        let invalid = |parts: &[&str], why: &'static str| {
            let (packet, _) = bits(parts);
            assert_eq!(
                packet_length(&packet),
                Err(PacketError::Invalid(why)),
                "{parts:?}"
            );
        };
        let predictor = "000 0000 01100100 0 00 0 000000 1".replace(' ', "");
        invalid(
            &[&predictor, &"0".repeat(32)],
            "predictor data, which AAC-LC has none of",
        );
        invalid(
            &[&long(0), "001", END_BITS],
            "gain control, which AAC-LC has none of",
        );
        invalid(&[&long(1), "1100", "00001"], "the reserved codebook");
        invalid(&[&long(1), "0001", "00000"], "a section past the last band");
        invalid(&[&long(1), "0001", "00010"], "a section past the last band");
        invalid(&[&long(50)], "more bands than the rate has");
        // Global gain 100, then two differences of -60: -20.
        invalid(
            &[&long(2), "0001", "00010", &scale(0), &scale(0)],
            "a scale factor out of range",
        );
        // Global gain 255, then a difference of +60: 315.
        let loud = "000 0000 11111111 0 00 0 000001 0 0001 00001".replace(' ', "");
        invalid(&[&loud, &scale(120)], "a scale factor out of range");
        invalid(&["001"], "an element the phone's recorder does not write");
        invalid(&["011"], "an element the phone's recorder does not write");
        invalid(&["010"], "an element the phone's recorder does not write");
        invalid(&["101"], "an element the phone's recorder does not write");
        invalid(&[&long(0), NO_TOOLS, &long(0)], "a second channel");
        invalid(&[END_BITS], "a packet without a channel");
        let reserved = "000 0000 01100100 1".replace(' ', "");
        invalid(&[&reserved], "the reserved bit is set");
    }

    /// The silent packet Apple's encoder writes for a mono 44.1 kHz stream
    /// of zeros: a long window with no bands, then `END`, four bytes.
    #[test]
    fn a_silent_packet_is_four_bytes() {
        let silent = [0x00, 0xd0, 0x00, 0x07];
        assert_eq!(packet_length(&silent), Ok(4));
        assert_eq!(packet_length(&[0x00, 0xd0, 0x00, 0x07, 0x00, 0xd0]), Ok(4));
        assert_eq!(packet_length(&silent[..3]), Err(PacketError::Truncated));
        assert_eq!(packet_length(&[]), Err(PacketError::Truncated));
    }

    #[test]
    fn other_elements_are_not_the_phones_shape() {
        // A channel pair, then the reserved bit set, then END alone.
        assert!(matches!(
            packet_length(&[0x20, 0, 0, 0]),
            Err(PacketError::Invalid(_))
        ));
        assert!(matches!(
            packet_length(&[0x00, 0xd1, 0, 0x07]),
            Err(PacketError::Invalid(_))
        ));
        assert!(matches!(
            packet_length(&[0xe0]),
            Err(PacketError::Invalid(_))
        ));
        // Zeros: a channel with no bands, then a second channel.
        assert!(matches!(
            packet_length(&[0; 16]),
            Err(PacketError::Invalid(_))
        ));
    }
}
