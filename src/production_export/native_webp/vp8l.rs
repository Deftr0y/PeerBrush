//! Bounded row adaptation of image-webp 0.2.4 lossless RGBA encoding.
//! The subtract-green, predictor2, Huffman and 4096-repeat policy is unchanged.
//! Upstream src/encoder.rs SHA256:
//! 724d54d1f91f472211cc6595ae24a2975f878abb6d2ef03f2feb2d4f8259ea88
/*
Adapted from image-webp 0.2.4 (image-rs contributors), under its MIT license:
MIT License

Permission is hereby granted, free of charge, to any
person obtaining a copy of this software and associated
documentation files (the "Software"), to deal in the
Software without restriction, including without
limitation the rights to use, copy, modify, merge,
publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software
is furnished to do so, subject to the following
conditions:

The above copyright notice and this permission notice
shall be included in all copies or substantial portions
of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
DEALINGS IN THE SOFTWARE.
*/
use std::{
    collections::BinaryHeap,
    io::{self, Write},
};
struct BitWriter<W> {
    writer: W,
    buffer: u64,
    nbits: u8,
    output: [u8; 64 * 1024],
    used: usize,
}

impl<W: Write> BitWriter<W> {
    fn write_bits(&mut self, bits: u64, nbits: u8) -> io::Result<()> {
        debug_assert!(nbits <= 64);

        self.buffer |= bits << self.nbits;
        self.nbits += nbits;

        if self.nbits >= 64 {
            self.write_bytes(&self.buffer.to_le_bytes())?;
            self.nbits -= 64;
            self.buffer = bits.checked_shr(u32::from(nbits - self.nbits)).unwrap_or(0);
        }
        debug_assert!(self.nbits < 64);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.nbits % 8 != 0 {
            self.write_bits(0, 8 - self.nbits % 8)?;
        }
        if self.nbits > 0 {
            self.write_bytes(&self.buffer.to_le_bytes()[..self.nbits as usize / 8])?;
            self.buffer = 0;
            self.nbits = 0;
        }
        self.writer.write_all(&self.output[..self.used])?;
        self.used = 0;
        Ok(())
    }
    // No Drop flushing: a failed/canceled frame must never issue later writes.
    fn write_bytes(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let take = bytes.len().min(self.output.len() - self.used);
            self.output[self.used..self.used + take].copy_from_slice(&bytes[..take]);
            self.used += take;
            bytes = &bytes[take..];
            if self.used == self.output.len() {
                self.writer.write_all(&self.output)?;
                self.used = 0;
            }
        }
        Ok(())
    }
}

fn write_single_entry_huffman_tree<W: Write>(w: &mut BitWriter<W>, symbol: u8) -> io::Result<()> {
    w.write_bits(1, 2)?;
    if symbol <= 1 {
        w.write_bits(0, 1)?;
        w.write_bits(u64::from(symbol), 1)?;
    } else {
        w.write_bits(1, 1)?;
        w.write_bits(u64::from(symbol), 8)?;
    }
    Ok(())
}

fn build_huffman_tree(
    frequencies: &[u32],
    lengths: &mut [u8],
    codes: &mut [u16],
    length_limit: u8,
) -> bool {
    assert_eq!(frequencies.len(), lengths.len());
    assert_eq!(frequencies.len(), codes.len());

    if frequencies.iter().filter(|&&f| f > 0).count() <= 1 {
        lengths.fill(0);
        codes.fill(0);
        return false;
    }

    #[derive(Eq, PartialEq, Copy, Clone, Debug)]
    struct Item(u32, u16);
    impl Ord for Item {
        fn cmp(&self, other: &Self) -> std::cmp::Ordering {
            other.0.cmp(&self.0)
        }
    }
    impl PartialOrd for Item {
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }

    // Build a huffman tree
    let mut internal_nodes = Vec::new();
    let mut nodes = BinaryHeap::from_iter(
        frequencies
            .iter()
            .enumerate()
            .filter(|(_, &frequency)| frequency > 0)
            .map(|(i, &frequency)| Item(frequency, i as u16)),
    );
    while nodes.len() > 1 {
        let Item(frequency1, index1) = nodes.pop().unwrap();
        let mut root = nodes.peek_mut().unwrap();
        internal_nodes.push((index1, root.1));
        *root = Item(
            frequency1 + root.0,
            internal_nodes.len() as u16 + frequencies.len() as u16 - 1,
        );
    }

    // Walk the tree to assign code lengths
    lengths.fill(0);
    let mut stack = Vec::new();
    stack.push((nodes.pop().unwrap().1, 0));
    while let Some((node, depth)) = stack.pop() {
        let node = node as usize;
        if node < frequencies.len() {
            lengths[node] = depth as u8;
        } else {
            let (left, right) = internal_nodes[node - frequencies.len()];
            stack.push((left, depth + 1));
            stack.push((right, depth + 1));
        }
    }

    // Limit the codes to length length_limit
    let mut max_length = 0;
    for &length in lengths.iter() {
        max_length = max_length.max(length);
    }
    if max_length > length_limit {
        let mut counts = [0u32; 16];
        for &length in lengths.iter() {
            counts[length.min(length_limit) as usize] += 1;
        }

        let mut total = 0;
        for (i, count) in counts
            .iter()
            .enumerate()
            .skip(1)
            .take(length_limit as usize)
        {
            total += count << (length_limit as usize - i);
        }

        while total > 1u32 << length_limit {
            let mut i = length_limit as usize - 1;
            while counts[i] == 0 {
                i -= 1;
            }
            counts[i] -= 1;
            counts[length_limit as usize] -= 1;
            counts[i + 1] += 2;
            total -= 1;
        }

        // assign new lengths
        let mut len = length_limit;
        let mut indexes = frequencies.iter().copied().enumerate().collect::<Vec<_>>();
        indexes.sort_unstable_by_key(|&(_, frequency)| frequency);
        for &(i, frequency) in &indexes {
            if frequency > 0 {
                while counts[len as usize] == 0 {
                    len -= 1;
                }
                lengths[i] = len;
                counts[len as usize] -= 1;
            }
        }
    }

    // Assign codes
    codes.fill(0);
    let mut code = 0u32;
    for len in 1..=length_limit {
        for (i, &length) in lengths.iter().enumerate() {
            if length == len {
                codes[i] = (code as u16).reverse_bits() >> (16 - len);
                code += 1;
            }
        }
        code <<= 1;
    }
    assert_eq!(code, 2 << length_limit);

    true
}

fn write_huffman_tree<W: Write>(
    w: &mut BitWriter<W>,
    frequencies: &[u32],
    lengths: &mut [u8],
    codes: &mut [u16],
) -> io::Result<()> {
    if !build_huffman_tree(frequencies, lengths, codes, 15) {
        let symbol = frequencies
            .iter()
            .position(|&frequency| frequency > 0)
            .unwrap_or(0);
        return write_single_entry_huffman_tree(w, symbol as u8);
    }

    let mut code_length_lengths = [0u8; 16];
    let mut code_length_codes = [0u16; 16];
    let mut code_length_frequencies = [0u32; 16];
    for &length in lengths.iter() {
        code_length_frequencies[length as usize] += 1;
    }
    let single_code_length_length = !build_huffman_tree(
        &code_length_frequencies,
        &mut code_length_lengths,
        &mut code_length_codes,
        7,
    );

    const CODE_LENGTH_ORDER: [usize; 19] = [
        17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
    ];

    // Write the huffman tree
    w.write_bits(0, 1)?; // normal huffman tree
    w.write_bits(19 - 4, 4)?; // num_code_lengths - 4

    for i in CODE_LENGTH_ORDER {
        if i > 15 || code_length_frequencies[i] == 0 {
            w.write_bits(0, 3)?;
        } else if single_code_length_length {
            w.write_bits(1, 3)?;
        } else {
            w.write_bits(u64::from(code_length_lengths[i]), 3)?;
        }
    }

    match lengths.len() {
        256 => {
            w.write_bits(1, 1)?; // max_symbol is stored
            w.write_bits(3, 3)?; // max_symbol_nbits / 2 - 2
            w.write_bits(254, 8)?; // max_symbol - 2
        }
        280 => w.write_bits(0, 1)?,
        _ => unreachable!(),
    }

    // Write the huffman codes
    if !single_code_length_length {
        for &len in lengths.iter() {
            w.write_bits(
                u64::from(code_length_codes[len as usize]),
                code_length_lengths[len as usize],
            )?;
        }
    }

    Ok(())
}

const fn length_to_symbol(len: u16) -> (u16, u8) {
    let len = len - 1;
    let highest_bit = len.ilog2() as u16;
    let second_highest_bit = (len >> (highest_bit - 1)) & 1;
    let extra_bits = highest_bit - 1;
    let symbol = 2 * highest_bit + second_highest_bit;
    (symbol, extra_bits as u8)
}

pub(super) struct Frequencies {
    channels: [[u32; 256]; 3],
    green: [u32; 280],
}
impl Frequencies {
    fn new() -> Self {
        Self {
            channels: [[0; 256]; 3],
            green: [0; 280],
        }
    }
    fn add(&mut self, pixel: [u8; 4], repeat: u16) {
        self.channels[0][pixel[0] as usize] += 1;
        self.green[pixel[1] as usize] += 1;
        self.channels[1][pixel[2] as usize] += 1;
        self.channels[2][pixel[3] as usize] += 1;
        if repeat != 0 {
            self.green[256 + run_symbol(repeat).0] += 1;
        }
    }
}
fn run_symbol(repeat: u16) -> (usize, u8) {
    if repeat <= 4 {
        (repeat as usize - 1, 0)
    } else {
        let (symbol, extra) = length_to_symbol(repeat);
        (symbol as usize, extra)
    }
}

/// Complete source visits are mandatory on every pass. `previous` is a charged
/// width*4 buffer owned by the caller; no full frame or encoded frame is kept.
fn transformed(
    width: u32,
    height: u32,
    previous: &mut [u8],
    visit: &mut dyn FnMut(&mut dyn FnMut([u8; 4]) -> io::Result<()>) -> io::Result<()>,
    event: &mut dyn FnMut([u8; 4], u16) -> io::Result<()>,
) -> io::Result<()> {
    if width == 0
        || height == 0
        || width > 16000
        || height > 16000
        || previous.len() != width as usize * 4
    {
        return Err(io::Error::other(
            "Native VP8L dimensions or row owner differ",
        ));
    }
    previous.fill(0);
    let total = u64::from(width) * u64::from(height);
    let mut count = 0u64;
    let mut left = [0u8; 4];
    let mut pending = None;
    let mut repeat = 0u16;
    visit(&mut |mut pixel| {
        if count == total {
            return Err(io::Error::other("Native VP8L source has excess pixels"));
        }
        pixel[0] = pixel[0].wrapping_sub(pixel[1]);
        pixel[2] = pixel[2].wrapping_sub(pixel[1]);
        let x = (count % u64::from(width)) as usize;
        let y = count / u64::from(width);
        let mut residual = pixel;
        if y == 0 {
            if x == 0 {
                residual[3] = residual[3].wrapping_sub(255);
            } else {
                for c in 0..4 {
                    residual[c] = residual[c].wrapping_sub(left[c]);
                }
            }
        } else {
            for c in 0..4 {
                residual[c] = residual[c].wrapping_sub(previous[x * 4 + c]);
            }
        }
        previous[x * 4..x * 4 + 4].copy_from_slice(&pixel);
        left = pixel;
        match pending {
            Some(value) if value == residual && repeat < 4096 => {
                repeat += 1;
            }
            Some(value) => {
                event(value, repeat)?;
                pending = Some(residual);
                repeat = 0;
            }
            None => {
                pending = Some(residual);
            }
        }
        count += 1;
        Ok(())
    })?;
    if count != total {
        return Err(io::Error::other("Native VP8L source is incomplete"));
    }
    if let Some(value) = pending {
        event(value, repeat)?;
    }
    Ok(())
}
pub(super) fn analyze(
    width: u32,
    height: u32,
    previous: &mut [u8],
    visit: &mut dyn FnMut(&mut dyn FnMut([u8; 4]) -> io::Result<()>) -> io::Result<()>,
) -> io::Result<Frequencies> {
    let mut frequencies = Frequencies::new();
    transformed(width, height, previous, visit, &mut |pixel, repeat| {
        frequencies.add(pixel, repeat);
        Ok(())
    })?;
    Ok(frequencies)
}
pub(super) fn write_frame(
    width: u32,
    height: u32,
    frequencies: &Frequencies,
    previous: &mut [u8],
    visit: &mut dyn FnMut(&mut dyn FnMut([u8; 4]) -> io::Result<()>) -> io::Result<()>,
    output: &mut dyn Write,
) -> io::Result<()> {
    if width == 0 || height == 0 || width > 16000 || height > 16000 {
        return Err(io::Error::other(
            "Native VP8L geometry exceeds its envelope",
        ));
    }
    let w = &mut BitWriter {
        writer: output,
        buffer: 0,
        nbits: 0,
        output: [0; 64 * 1024],
        used: 0,
    };
    w.write_bits(0x2f, 8)?;
    w.write_bits(u64::from(width) - 1, 14)?;
    w.write_bits(u64::from(height) - 1, 14)?;
    w.write_bits(1, 1)?;
    w.write_bits(0, 3)?;
    w.write_bits(0b101, 3)?;
    w.write_bits(0b111001, 6)?;
    w.write_bits(0, 1)?;
    write_single_entry_huffman_tree(w, 2)?;
    for _ in 0..4 {
        write_single_entry_huffman_tree(w, 0)?;
    }
    w.write_bits(0, 3)?;
    let mut lengths = [[0u8; 256]; 3];
    let mut codes = [[0u16; 256]; 3];
    let mut green_lengths = [0u8; 280];
    let mut green_codes = [0u16; 280];
    write_huffman_tree(w, &frequencies.green, &mut green_lengths, &mut green_codes)?;
    for c in 0..3 {
        write_huffman_tree(w, &frequencies.channels[c], &mut lengths[c], &mut codes[c])?;
    }
    write_single_entry_huffman_tree(w, 1)?;
    transformed(width, height, previous, visit, &mut |pixel, repeat| {
        let g = pixel[1] as usize;
        w.write_bits(u64::from(green_codes[g]), green_lengths[g])?;
        for (c, channel) in [0, 2, 3].into_iter().enumerate() {
            let value = pixel[channel] as usize;
            w.write_bits(u64::from(codes[c][value]), lengths[c][value])?;
        }
        if repeat != 0 {
            let (symbol, extra) = run_symbol(repeat);
            w.write_bits(
                u64::from(green_codes[256 + symbol]),
                green_lengths[256 + symbol],
            )?;
            if extra != 0 {
                w.write_bits(u64::from(repeat - 1) & ((1 << extra) - 1), extra)?;
            }
        }
        Ok(())
    })?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reference(data: &[u8], width: u32, height: u32) -> Vec<u8> {
        use image::ImageEncoder;
        let mut encoded = vec![];
        image::codecs::webp::WebPEncoder::new_lossless(&mut encoded)
            .write_image(data, width, height, image::ExtendedColorType::Rgba8)
            .unwrap();
        assert_eq!(&encoded[12..16], b"VP8L");
        let count = u32::from_le_bytes(encoded[16..20].try_into().unwrap()) as usize;
        encoded[20..20 + count].to_vec()
    }
    #[test]
    fn bounded_rows_match_pinned_lossless_frame_including_run_boundaries_and_hidden_rgb() {
        for (width, height) in [(1, 1), (1, 515), (259, 515), (8193, 2)] {
            for mode in 0..3 {
                let data: Vec<u8> = (0..width * height)
                    .flat_map(|n| {
                        if mode == 0 {
                            [101, 203, 57, 0]
                        } else if mode == 1 {
                            [
                                n as u8,
                                (n * 71) as u8,
                                (n * 29 + 3) as u8,
                                if n % 7 == 0 { 0 } else { 177 },
                            ]
                        } else {
                            [((n / 4097) * 17) as u8, ((n / 4097) * 71) as u8, 33, 255]
                        }
                    })
                    .collect();
                let mut previous = vec![0; width as usize * 4];
                let mut visit = |emit: &mut dyn FnMut([u8; 4]) -> io::Result<()>| {
                    for p in data.chunks_exact(4) {
                        emit(p.try_into().unwrap())?;
                    }
                    Ok(())
                };
                let f = analyze(width, height, &mut previous, &mut visit).unwrap();
                let mut encoded = vec![];
                write_frame(width, height, &f, &mut previous, &mut visit, &mut encoded).unwrap();
                assert_eq!(
                    encoded,
                    reference(&data, width, height),
                    "{width}x{height} mode{mode}"
                );
            }
        }
    }
    #[test]
    fn incomplete_excess_and_transport_failures_propagate_without_flush_panic() {
        let mut row = [0; 4];
        let mut empty = |_: &mut dyn FnMut([u8; 4]) -> io::Result<()>| Ok(());
        assert!(analyze(1, 1, &mut row, &mut empty).is_err());
        let mut excess = |f: &mut dyn FnMut([u8; 4]) -> io::Result<()>| {
            f([0; 4])?;
            f([0; 4])
        };
        assert!(analyze(1, 1, &mut row, &mut excess).is_err());
        let mut one = |f: &mut dyn FnMut([u8; 4]) -> io::Result<()>| f([0; 4]);
        let f = analyze(1, 1, &mut row, &mut one).unwrap();
        struct Refuse;
        impl Write for Refuse {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("injected output refusal"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert!(write_frame(1, 1, &f, &mut row, &mut one, &mut Refuse).is_err());
    }
}
