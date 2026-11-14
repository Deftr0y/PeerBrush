//! Decoding of lossless WebP images
//!
//! [Lossless spec](https://developers.google.com/speed/webp/docs/webp_lossless_bitstream_specification)

use std::io::BufRead;
use std::mem;

use crate::decoder::DecodingError;
use crate::lossless_transform::{
    apply_color_indexing_transform, apply_color_transform, apply_predictor_row, apply_predictor_transform,
    apply_subtract_green_transform,
};

use super::huffman::HuffmanTree;
use super::lossless_transform::TransformType;

const CODE_LENGTH_CODES: usize = 19;
const CODE_LENGTH_CODE_ORDER: [usize; CODE_LENGTH_CODES] = [
    17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
];

#[rustfmt::skip]
const DISTANCE_MAP: [(i8, i8); 120] = [
    (0, 1),  (1, 0),  (1, 1),  (-1, 1), (0, 2),  (2, 0),  (1, 2),  (-1, 2),
    (2, 1),  (-2, 1), (2, 2),  (-2, 2), (0, 3),  (3, 0),  (1, 3),  (-1, 3),
    (3, 1),  (-3, 1), (2, 3),  (-2, 3), (3, 2),  (-3, 2), (0, 4),  (4, 0),
    (1, 4),  (-1, 4), (4, 1),  (-4, 1), (3, 3),  (-3, 3), (2, 4),  (-2, 4),
    (4, 2),  (-4, 2), (0, 5),  (3, 4),  (-3, 4), (4, 3),  (-4, 3), (5, 0),
    (1, 5),  (-1, 5), (5, 1),  (-5, 1), (2, 5),  (-2, 5), (5, 2),  (-5, 2),
    (4, 4),  (-4, 4), (3, 5),  (-3, 5), (5, 3),  (-5, 3), (0, 6),  (6, 0),
    (1, 6),  (-1, 6), (6, 1),  (-6, 1), (2, 6),  (-2, 6), (6, 2),  (-6, 2),
    (4, 5),  (-4, 5), (5, 4),  (-5, 4), (3, 6),  (-3, 6), (6, 3),  (-6, 3),
    (0, 7),  (7, 0),  (1, 7),  (-1, 7), (5, 5),  (-5, 5), (7, 1),  (-7, 1),
    (4, 6),  (-4, 6), (6, 4),  (-6, 4), (2, 7),  (-2, 7), (7, 2),  (-7, 2),
    (3, 7),  (-3, 7), (7, 3),  (-7, 3), (5, 6),  (-5, 6), (6, 5),  (-6, 5),
    (8, 0),  (4, 7),  (-4, 7), (7, 4),  (-7, 4), (8, 1),  (8, 2),  (6, 6),
    (-6, 6), (8, 3),  (5, 7),  (-5, 7), (7, 5),  (-7, 5), (8, 4),  (6, 7),
    (-6, 7), (7, 6),  (-7, 6), (8, 5),  (7, 7),  (-7, 7), (8, 6),  (8, 7)
];

const GREEN: usize = 0;
const RED: usize = 1;
const BLUE: usize = 2;
const ALPHA: usize = 3;
const DIST: usize = 4;

const HUFFMAN_CODES_PER_META_CODE: usize = 5;

type HuffmanCodeGroup = [HuffmanTree; HUFFMAN_CODES_PER_META_CODE];

const ALPHABET_SIZE: [u16; HUFFMAN_CODES_PER_META_CODE] = [256 + 24, 256, 256, 256, 40];

#[inline]
pub(crate) fn subsample_size(size: u16, bits: u8) -> u16 {
    ((u32::from(size) + (1u32 << bits) - 1) >> bits)
        .try_into()
        .unwrap()
}

const NUM_TRANSFORM_TYPES: usize = 4;
const HISTORY_PIXELS: usize = 1 << 20;

/// Returns the scratch requirement for a row-decoded VP8L frame.
pub fn row_scratch_bytes(width: u32, height: u32) -> Option<usize> {
    if !(1..=16384).contains(&width) || !(1..=16384).contains(&height) {
        return None;
    }
    let width = width as usize;
    let pixels = width.checked_mul(height as usize)?;
    let history = pixels.min(HISTORY_PIXELS).next_power_of_two();
    width.checked_mul(3)?.checked_add(history)?.checked_mul(4)
}

trait PixelOutput {
    fn write(&mut self, at: usize, value: [u8; 4]) -> Result<(), DecodingError>;

    fn repeat(&mut self, at: usize, count: usize, value: [u8; 4]) -> Result<(), DecodingError> {
        for index in at..at + count {
            self.write(index, value)?;
        }
        Ok(())
    }

    fn copy(
        &mut self, at: usize, length: usize, distance: usize, total: usize,
        cache: Option<&mut ColorCache>,
    ) -> Result<(), DecodingError>;
}

impl PixelOutput for [u8] {
    #[inline]
    fn write(&mut self, at: usize, value: [u8; 4]) -> Result<(), DecodingError> {
        self[at * 4..][..4].copy_from_slice(&value);
        Ok(())
    }

    fn copy(
        &mut self, at: usize, length: usize, distance: usize, total: usize,
        cache: Option<&mut ColorCache>,
    ) -> Result<(), DecodingError> {
        if distance == 1 {
            let value = self[(at - 1) * 4..][..4].try_into().unwrap();
            return self.repeat(at, length, value);
        }
        if at + length + 3 <= total {
            let start = (at - distance) * 4;
            self.copy_within(start..start + 16, at * 4);
            if length > 4 || distance < 4 {
                for i in (0..length * 4).step_by((distance * 4).min(16)).skip(1) {
                    self.copy_within(start + i..start + i + 16, at * 4 + i);
                }
            }
        } else {
            for i in 0..length * 4 {
                self[at * 4 + i] = self[(at - distance) * 4 + i];
            }
        }
        if let Some(cache) = cache {
            for pixel in self[at * 4..][..length * 4].chunks_exact(4) {
                cache.insert(pixel.try_into().unwrap());
            }
        }
        Ok(())
    }
}

struct RowOutput<'a, F> {
    history: &'a mut [u8],
    row: &'a mut [u8],
    prediction: &'a mut [u8],
    transforms: &'a [Option<TransformType>; NUM_TRANSFORM_TYPES],
    order: &'a [u8],
    transformed_width: u16,
    width: u16,
    emit: F,
}

impl<F: FnMut(u32, &mut [u8]) -> Result<(), DecodingError>> RowOutput<'_, F> {
    fn emit_row(&mut self, y: usize) -> Result<(), DecodingError> {
        let mut width = self.transformed_width;
        for &index in self.order.iter().rev() {
            let stride = usize::from(width) * 4;
            match self.transforms[usize::from(index)].as_ref().unwrap() {
                TransformType::PredictorTransform { size_bits, predictor_data } => {
                    let rows = &mut self.prediction[..2 * stride];
                    rows[stride..].copy_from_slice(&self.row[..stride]);
                    apply_predictor_row(rows, width, 1, y, *size_bits, predictor_data);
                    rows.copy_within(stride.., 0);
                    self.row[..stride].copy_from_slice(&rows[..stride]);
                }
                TransformType::ColorTransform { size_bits, transform_data } => {
                    let at = (y >> *size_bits) * usize::from(subsample_size(width, *size_bits)) * 4;
                    apply_color_transform(&mut self.row[..stride], width, *size_bits, &transform_data[at..]);
                }
                TransformType::SubtractGreen => apply_subtract_green_transform(&mut self.row[..stride]),
                TransformType::ColorIndexingTransform { table_size, table_data } => {
                    width = self.width;
                    let bits = match table_size {
                        1..=2 => 1,
                        3..=4 => 2,
                        5..=16 => 4,
                        _ => 8,
                    };
                    let group = 8 / bits;
                    let mask = ((1u16 << bits) - 1) as u8;
                    for x in (0..usize::from(width)).rev() {
                        let packed = self.row[(x / group) * 4 + 1];
                        let index = usize::from((packed >> ((x % group) * bits)) & mask);
                        let color = table_data.get(index * 4..index * 4 + 4).unwrap_or(&[0; 4]);
                        self.row[x * 4..][..4].copy_from_slice(color);
                    }
                }
            }
        }
        (self.emit)(y as u32, &mut self.row[..usize::from(self.width) * 4])
    }
}

impl<F: FnMut(u32, &mut [u8]) -> Result<(), DecodingError>> PixelOutput for RowOutput<'_, F> {
    fn write(&mut self, at: usize, value: [u8; 4]) -> Result<(), DecodingError> {
        let offset = (at * 4) & (self.history.len() - 1);
        self.history[offset..][..4].copy_from_slice(&value);
        let width = usize::from(self.transformed_width);
        let x = at % width;
        self.row[x * 4..][..4].copy_from_slice(&value);
        if x + 1 == width {
            self.emit_row(at / width)?;
        }
        Ok(())
    }

    fn copy(
        &mut self, at: usize, length: usize, distance: usize, _: usize,
        mut cache: Option<&mut ColorCache>,
    ) -> Result<(), DecodingError> {
        if distance > self.history.len() / 4 {
            return Err(DecodingError::BitStreamError);
        }
        for index in at..at + length {
            let offset = ((index - distance) * 4) & (self.history.len() - 1);
            let value = self.history[offset..][..4].try_into().unwrap();
            self.write(index, value)?;
            if distance != 1 {
                if let Some(cache) = cache.as_deref_mut() {
                    cache.insert(value);
                }
            }
        }
        Ok(())
    }
}

//Decodes lossless WebP images
#[derive(Debug)]
pub(crate) struct LosslessDecoder<R> {
    huffman_limit: usize,
    bit_reader: BitReader<R>,
    transforms: [Option<TransformType>; NUM_TRANSFORM_TYPES],
    transform_order: Vec<u8>,
    width: u16,
    height: u16,
}

impl<R: BufRead> LosslessDecoder<R> {
    /// Create a decoder with separately admitted lossless Huffman storage.
    pub(crate) const fn with_huffman_limit(r: R, huffman_limit: usize) -> Self {
        Self {
            huffman_limit,
            bit_reader: BitReader::new(r),
            transforms: [None, None, None, None],
            transform_order: Vec::new(),
            width: 0,
            height: 0,
        }
    }

    fn read_frame_header(
        &mut self,
        width: u32,
        height: u32,
        implicit_dimensions: bool,
    ) -> Result<(), DecodingError> {
        if implicit_dimensions {
            self.width = width as u16;
            self.height = height as u16;
        } else {
            let signature = self.bit_reader.read_bits::<u8>(8)?;
            if signature != 0x2f {
                return Err(DecodingError::LosslessSignatureInvalid(signature));
            }

            self.width = self.bit_reader.read_bits::<u16>(14)? + 1;
            self.height = self.bit_reader.read_bits::<u16>(14)? + 1;
            if u32::from(self.width) != width || u32::from(self.height) != height {
                return Err(DecodingError::InconsistentImageSizes);
            }

            let _alpha_used = self.bit_reader.read_bits::<u8>(1)?;
            let version_num = self.bit_reader.read_bits::<u8>(3)?;
            if version_num != 0 {
                return Err(DecodingError::VersionNumberInvalid(version_num));
            }
        }

        Ok(())
    }

    /// Decodes a frame. The VP8L stream in an alpha chunk has implicit dimensions.
    pub(crate) fn decode_frame(
        &mut self, width: u32, height: u32, implicit_dimensions: bool, buf: &mut [u8],
    ) -> Result<(), DecodingError> {
        self.read_frame_header(width, height, implicit_dimensions)?;
        let transformed_width = self.read_transforms()?;
        let transformed_size = usize::from(transformed_width) * usize::from(self.height) * 4;
        self.decode_image_stream(
            transformed_width,
            self.height,
            true,
            &mut buf[..transformed_size],
        )?;

        let mut image_size = transformed_size;
        let mut width = transformed_width;
        for &trans_index in self.transform_order.iter().rev() {
            let transform = self.transforms[usize::from(trans_index)].as_ref().unwrap();
            match transform {
                TransformType::PredictorTransform {
                    size_bits,
                    predictor_data,
                } => apply_predictor_transform(
                    &mut buf[..image_size],
                    width,
                    self.height,
                    *size_bits,
                    predictor_data,
                )?,
                TransformType::ColorTransform {
                    size_bits,
                    transform_data,
                } => {
                    apply_color_transform(
                        &mut buf[..image_size],
                        width,
                        *size_bits,
                        transform_data,
                    );
                }
                TransformType::SubtractGreen => {
                    apply_subtract_green_transform(&mut buf[..image_size]);
                }
                TransformType::ColorIndexingTransform {
                    table_size,
                    table_data,
                } => {
                    width = self.width;
                    image_size = usize::from(width) * usize::from(self.height) * 4;
                    apply_color_indexing_transform(
                        buf,
                        width,
                        self.height,
                        *table_size,
                        table_data,
                    );
                }
            }
        }

        Ok(())
    }

    pub(crate) fn decode_rows(
        &mut self, width: u32, height: u32, implicit_dimensions: bool, scratch: &mut [u8],
        emit: impl FnMut(u32, &mut [u8]) -> Result<(), DecodingError>,
    ) -> Result<(), DecodingError> {
        if width == 0 || height == 0 || row_scratch_bytes(width, height) != Some(scratch.len()) {
            return Err(DecodingError::InconsistentImageSizes);
        }
        self.read_frame_header(width, height, implicit_dimensions)?;
        let transformed_width = self.read_transforms()?;
        let transforms = std::mem::take(&mut self.transforms);
        let order = std::mem::take(&mut self.transform_order);
        let (history, rows) = scratch.split_at_mut(scratch.len() - width as usize * 12);
        let (row, prediction) = rows.split_at_mut(width as usize * 4);
        let mut output = RowOutput {
            history, row, prediction, transforms: &transforms, order: &order,
            transformed_width, width: self.width, emit,
        };
        self.decode_image_stream_to(transformed_width, self.height, true, &mut output)
    }

    /// Reads Image data from the bitstream
    ///
    /// Can be in any of the 5 roles described in the Specification. ARGB Image role has different
    /// behaviour to the other 4. xsize and ysize describe the size of the blocks where each block
    /// has its own entropy code
    fn decode_image_stream(
        &mut self,
        xsize: u16,
        ysize: u16,
        is_argb_img: bool,
        data: &mut [u8],
    ) -> Result<(), DecodingError> {
        self.decode_image_stream_to(xsize, ysize, is_argb_img, data)
    }

    fn decode_image_stream_to(
        &mut self, xsize: u16, ysize: u16, is_argb_img: bool, data: &mut (impl PixelOutput + ?Sized),
    ) -> Result<(), DecodingError> {
        let color_cache_bits = self.read_color_cache()?;
        let color_cache = color_cache_bits.map(|bits| ColorCache {
            color_cache_bits: bits,
            color_cache: vec![[0; 4]; 1 << bits],
        });

        let huffman_info = self.read_huffman_codes(is_argb_img, xsize, ysize, color_cache)?;
        self.decode_image_data(xsize, ysize, huffman_info, data)
    }

    /// Reads transforms and their data from the bitstream
    fn read_transforms(&mut self) -> Result<u16, DecodingError> {
        let mut xsize = self.width;

        while self.bit_reader.read_bits::<u8>(1)? == 1 {
            let transform_type_val = self.bit_reader.read_bits::<u8>(2)?;

            if self.transforms[usize::from(transform_type_val)].is_some() {
                //can only have one of each transform, error
                return Err(DecodingError::TransformError);
            }

            self.transform_order.push(transform_type_val);

            let transform_type = match transform_type_val {
                0 => {
                    //predictor

                    let size_bits = self.bit_reader.read_bits::<u8>(3)? + 2;

                    let block_xsize = subsample_size(xsize, size_bits);
                    let block_ysize = subsample_size(self.height, size_bits);

                    let mut predictor_data =
                        vec![0; usize::from(block_xsize) * usize::from(block_ysize) * 4];
                    self.decode_image_stream(block_xsize, block_ysize, false, &mut predictor_data)?;

                    TransformType::PredictorTransform {
                        size_bits,
                        predictor_data,
                    }
                }
                1 => {
                    //color transform

                    let size_bits = self.bit_reader.read_bits::<u8>(3)? + 2;

                    let block_xsize = subsample_size(xsize, size_bits);
                    let block_ysize = subsample_size(self.height, size_bits);

                    let mut transform_data =
                        vec![0; usize::from(block_xsize) * usize::from(block_ysize) * 4];
                    self.decode_image_stream(block_xsize, block_ysize, false, &mut transform_data)?;

                    TransformType::ColorTransform {
                        size_bits,
                        transform_data,
                    }
                }
                2 => {
                    //subtract green

                    TransformType::SubtractGreen
                }
                3 => {
                    let color_table_size = self.bit_reader.read_bits::<u16>(8)? + 1;

                    let mut color_map = vec![0; usize::from(color_table_size) * 4];
                    self.decode_image_stream(color_table_size, 1, false, &mut color_map)?;

                    let bits = if color_table_size <= 2 {
                        3
                    } else if color_table_size <= 4 {
                        2
                    } else if color_table_size <= 16 {
                        1
                    } else {
                        0
                    };
                    xsize = subsample_size(xsize, bits);

                    Self::adjust_color_map(&mut color_map);

                    TransformType::ColorIndexingTransform {
                        table_size: color_table_size,
                        table_data: color_map,
                    }
                }
                _ => unreachable!(),
            };

            self.transforms[usize::from(transform_type_val)] = Some(transform_type);
        }

        Ok(xsize)
    }

    /// Adjusts the color map since it's subtraction coded
    fn adjust_color_map(color_map: &mut [u8]) {
        for i in 4..color_map.len() {
            color_map[i] = color_map[i].wrapping_add(color_map[i - 4]);
        }
    }

    /// Reads huffman codes associated with an image
    fn read_huffman_codes(
        &mut self,
        read_meta: bool,
        xsize: u16,
        ysize: u16,
        color_cache: Option<ColorCache>,
    ) -> Result<HuffmanInfo, DecodingError> {
        let mut num_huff_groups = 1u32;

        let mut huffman_bits = 0;
        let mut huffman_xsize = 1;
        let mut huffman_ysize = 1;
        let mut entropy_image = Vec::new();

        if read_meta && self.bit_reader.read_bits::<u8>(1)? == 1 {
            //meta huffman codes
            huffman_bits = self.bit_reader.read_bits::<u8>(3)? + 2;
            huffman_xsize = subsample_size(xsize, huffman_bits);
            huffman_ysize = subsample_size(ysize, huffman_bits);

            let mut data = vec![0; usize::from(huffman_xsize) * usize::from(huffman_ysize) * 4];
            self.decode_image_stream(huffman_xsize, huffman_ysize, false, &mut data)?;

            entropy_image = data
                .chunks_exact(4)
                .map(|pixel| {
                    let meta_huff_code = (u16::from(pixel[0]) << 8) | u16::from(pixel[1]);
                    if u32::from(meta_huff_code) >= num_huff_groups {
                        num_huff_groups = u32::from(meta_huff_code) + 1;
                    }
                    meta_huff_code
                })
                .collect::<Vec<u16>>();
        }

        // Recursive metadata images finish and release their trees before this
        // vector is allocated. The fixed scratch allowance covers code-length
        // vectors and their temporary tree while persistent groups accumulate.
        let group_bytes = (num_huff_groups as usize)
            .checked_mul(std::mem::size_of::<HuffmanCodeGroup>())
            .ok_or(DecodingError::MemoryLimitExceeded)?;
        let mut remaining = self
            .huffman_limit
            .checked_sub(128 * 1024)
            .and_then(|n| n.checked_sub(group_bytes))
            .ok_or(DecodingError::MemoryLimitExceeded)?;
        let mut hufftree_groups = Vec::new();
        hufftree_groups
            .try_reserve_exact(num_huff_groups as usize)
            .map_err(|_| DecodingError::MemoryLimitExceeded)?;

        for _i in 0..num_huff_groups {
            let mut group: HuffmanCodeGroup = Default::default();
            for j in 0..HUFFMAN_CODES_PER_META_CODE {
                let mut alphabet_size = ALPHABET_SIZE[j];
                if j == 0 {
                    if let Some(color_cache) = color_cache.as_ref() {
                        alphabet_size += 1 << color_cache.color_cache_bits;
                    }
                }

                let tree = self.read_huffman_code(alphabet_size, remaining)?;
                remaining = remaining
                    .checked_sub(tree.allocated_bytes())
                    .ok_or(DecodingError::MemoryLimitExceeded)?;
                group[j] = tree;
            }
            hufftree_groups.push(group);
        }

        let huffman_mask = if huffman_bits == 0 {
            !0
        } else {
            (1 << huffman_bits) - 1
        };

        let info = HuffmanInfo {
            xsize: huffman_xsize,
            _ysize: huffman_ysize,
            color_cache,
            image: entropy_image,
            bits: huffman_bits,
            mask: huffman_mask,
            huffman_code_groups: hufftree_groups,
        };

        Ok(info)
    }

    /// Decodes and returns a single huffman tree
    fn read_huffman_code(
        &mut self,
        alphabet_size: u16,
        limit: usize,
    ) -> Result<HuffmanTree, DecodingError> {
        let simple = self.bit_reader.read_bits::<u8>(1)? == 1;

        if simple {
            let num_symbols = self.bit_reader.read_bits::<u8>(1)? + 1;

            let is_first_8bits = self.bit_reader.read_bits::<u8>(1)?;
            let zero_symbol = self.bit_reader.read_bits::<u16>(1 + 7 * is_first_8bits)?;

            if zero_symbol >= alphabet_size {
                return Err(DecodingError::BitStreamError);
            }

            if num_symbols == 1 {
                Ok(HuffmanTree::build_single_node(zero_symbol))
            } else {
                let one_symbol = self.bit_reader.read_bits::<u16>(8)?;
                if one_symbol >= alphabet_size {
                    return Err(DecodingError::BitStreamError);
                }
                HuffmanTree::build_two_node_limited(zero_symbol, one_symbol, limit)
            }
        } else {
            let mut code_length_code_lengths = vec![0; CODE_LENGTH_CODES];

            let num_code_lengths = 4 + self.bit_reader.read_bits::<usize>(4)?;
            for i in 0..num_code_lengths {
                code_length_code_lengths[CODE_LENGTH_CODE_ORDER[i]] =
                    self.bit_reader.read_bits(3)?;
            }

            let new_code_lengths =
                self.read_huffman_code_lengths(code_length_code_lengths, alphabet_size)?;

            HuffmanTree::build_implicit_limited(new_code_lengths, limit)
        }
    }

    /// Reads huffman code lengths
    fn read_huffman_code_lengths(
        &mut self,
        code_length_code_lengths: Vec<u16>,
        num_symbols: u16,
    ) -> Result<Vec<u16>, DecodingError> {
        let table = HuffmanTree::build_implicit(code_length_code_lengths)?;

        let mut max_symbol = if self.bit_reader.read_bits::<u8>(1)? == 1 {
            let length_nbits = 2 + 2 * self.bit_reader.read_bits::<u8>(3)?;
            let max_minus_two = self.bit_reader.read_bits::<u16>(length_nbits)?;
            if max_minus_two > num_symbols - 2 {
                return Err(DecodingError::BitStreamError);
            }
            2 + max_minus_two
        } else {
            num_symbols
        };

        let mut code_lengths = vec![0; usize::from(num_symbols)];
        let mut prev_code_len = 8; //default code length

        let mut symbol = 0;
        while symbol < num_symbols {
            if max_symbol == 0 {
                break;
            }
            max_symbol -= 1;

            self.bit_reader.fill()?;
            let code_len = table.read_symbol(&mut self.bit_reader)?;

            if code_len < 16 {
                code_lengths[usize::from(symbol)] = code_len;
                symbol += 1;
                if code_len != 0 {
                    prev_code_len = code_len;
                }
            } else {
                let use_prev = code_len == 16;
                let slot = code_len - 16;
                let extra_bits = match slot {
                    0 => 2,
                    1 => 3,
                    2 => 7,
                    _ => return Err(DecodingError::BitStreamError),
                };
                let repeat_offset = match slot {
                    0 | 1 => 3,
                    2 => 11,
                    _ => return Err(DecodingError::BitStreamError),
                };

                let mut repeat = self.bit_reader.read_bits::<u16>(extra_bits)? + repeat_offset;

                if symbol + repeat > num_symbols {
                    return Err(DecodingError::BitStreamError);
                }

                let length = if use_prev { prev_code_len } else { 0 };
                while repeat > 0 {
                    repeat -= 1;
                    code_lengths[usize::from(symbol)] = length;
                    symbol += 1;
                }
            }
        }

        Ok(code_lengths)
    }

    /// Decodes the image data using the huffman trees and either of the 3 methods of decoding
    fn decode_image_data(
        &mut self,
        width: u16,
        height: u16,
        mut huffman_info: HuffmanInfo,
        data: &mut (impl PixelOutput + ?Sized),
    ) -> Result<(), DecodingError> {
        let num_values = usize::from(width) * usize::from(height);

        let huff_index = huffman_info.get_huff_index(0, 0);
        let mut tree = &huffman_info.huffman_code_groups[huff_index];
        let mut index = 0;

        let mut next_block_start = 0;
        while index < num_values {
            self.bit_reader.fill()?;

            if index >= next_block_start {
                let x = index % usize::from(width);
                let y = index / usize::from(width);
                next_block_start = (x | usize::from(huffman_info.mask)).min(usize::from(width - 1))
                    + y * usize::from(width)
                    + 1;

                let huff_index = huffman_info.get_huff_index(x as u16, y as u16);
                tree = &huffman_info.huffman_code_groups[huff_index];

                // Fast path: If all the codes each contain only a single
                // symbol, then the pixel data isn't written to the bitstream
                // and we can just fill the output buffer with the symbol
                // directly.
                if tree[..4].iter().all(|t| t.is_single_node()) {
                    let code = tree[GREEN].read_symbol(&mut self.bit_reader)?;
                    if code < 256 {
                        let n = if huffman_info.bits == 0 {
                            num_values
                        } else {
                            next_block_start - index
                        };

                        let red = tree[RED].read_symbol(&mut self.bit_reader)?;
                        let blue = tree[BLUE].read_symbol(&mut self.bit_reader)?;
                        let alpha = tree[ALPHA].read_symbol(&mut self.bit_reader)?;
                        let value = [red as u8, code as u8, blue as u8, alpha as u8];

                        data.repeat(index, n, value)?;

                        if let Some(color_cache) = huffman_info.color_cache.as_mut() {
                            color_cache.insert(value);
                        }

                        index += n;
                        continue;
                    }
                }
            }

            let code = tree[GREEN].read_symbol(&mut self.bit_reader)?;

            //check code
            if code < 256 {
                //literal, so just use huffman codes and read as argb
                let green = code as u8;
                let red = tree[RED].read_symbol(&mut self.bit_reader)? as u8;
                let blue = tree[BLUE].read_symbol(&mut self.bit_reader)? as u8;
                if self.bit_reader.nbits < 15 {
                    self.bit_reader.fill()?;
                }
                let alpha = tree[ALPHA].read_symbol(&mut self.bit_reader)? as u8;

                data.write(index, [red, green, blue, alpha])?;

                if let Some(color_cache) = huffman_info.color_cache.as_mut() {
                    color_cache.insert([red, green, blue, alpha]);
                }
                index += 1;
            } else if code < 256 + 24 {
                //backward reference, so go back and use that to add image data
                let length_symbol = code - 256;
                let length = Self::get_copy_distance(&mut self.bit_reader, length_symbol)?;

                let dist_symbol = tree[DIST].read_symbol(&mut self.bit_reader)?;
                let dist_code = Self::get_copy_distance(&mut self.bit_reader, dist_symbol)?;
                let dist = Self::plane_code_to_distance(width, dist_code);

                if index < dist || num_values - index < length {
                    return Err(DecodingError::BitStreamError);
                }

                data.copy(index, length, dist, num_values, huffman_info.color_cache.as_mut())?;
                index += length;
            } else {
                //color cache, so use previously stored pixels to get this pixel
                let color_cache = huffman_info
                    .color_cache
                    .as_mut()
                    .ok_or(DecodingError::BitStreamError)?;
                let color = color_cache.lookup((code - 280).into());
                data.write(index, color)?;
                index += 1;

                if index < next_block_start {
                    if let Some((bits, code)) = tree[GREEN].peek_symbol(&self.bit_reader) {
                        if code >= 280 {
                            self.bit_reader.consume(bits)?;
                            data.write(index, color_cache.lookup((code - 280).into()))?;
                            index += 1;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Reads color cache data from the bitstream
    fn read_color_cache(&mut self) -> Result<Option<u8>, DecodingError> {
        if self.bit_reader.read_bits::<u8>(1)? == 1 {
            let code_bits = self.bit_reader.read_bits::<u8>(4)?;

            if !(1..=11).contains(&code_bits) {
                return Err(DecodingError::InvalidColorCacheBits(code_bits));
            }

            Ok(Some(code_bits))
        } else {
            Ok(None)
        }
    }

    /// Gets the copy distance from the prefix code and bitstream
    fn get_copy_distance(
        bit_reader: &mut BitReader<R>,
        prefix_code: u16,
    ) -> Result<usize, DecodingError> {
        if prefix_code < 4 {
            return Ok(usize::from(prefix_code + 1));
        }
        let extra_bits: u8 = ((prefix_code - 2) >> 1).try_into().unwrap();
        let offset = (2 + (usize::from(prefix_code) & 1)) << extra_bits;

        let bits = bit_reader.peek(extra_bits) as usize;
        bit_reader.consume(extra_bits)?;

        Ok(offset + bits + 1)
    }

    /// Gets distance to pixel
    fn plane_code_to_distance(xsize: u16, plane_code: usize) -> usize {
        if plane_code > 120 {
            plane_code - 120
        } else {
            let (xoffset, yoffset) = DISTANCE_MAP[plane_code - 1];

            let dist = i32::from(xoffset) + i32::from(yoffset) * i32::from(xsize);
            if dist < 1 {
                return 1;
            }
            dist.try_into().unwrap()
        }
    }
}

#[derive(Debug, Clone)]
struct HuffmanInfo {
    xsize: u16,
    _ysize: u16,
    color_cache: Option<ColorCache>,
    image: Vec<u16>,
    bits: u8,
    mask: u16,
    huffman_code_groups: Vec<HuffmanCodeGroup>,
}

impl HuffmanInfo {
    fn get_huff_index(&self, x: u16, y: u16) -> usize {
        if self.bits == 0 {
            return 0;
        }
        let position =
            usize::from(y >> self.bits) * usize::from(self.xsize) + usize::from(x >> self.bits);
        let meta_huff_code: usize = usize::from(self.image[position]);
        meta_huff_code
    }
}

#[derive(Debug, Clone)]
struct ColorCache {
    color_cache_bits: u8,
    color_cache: Vec<[u8; 4]>,
}

impl ColorCache {
    #[inline(always)]
    fn insert(&mut self, color: [u8; 4]) {
        let [r, g, b, a] = color;
        let color_u32 =
            (u32::from(r) << 16) | (u32::from(g) << 8) | (u32::from(b)) | (u32::from(a) << 24);
        let index = (0x1e35a7bdu32.wrapping_mul(color_u32)) >> (32 - self.color_cache_bits);
        self.color_cache[index as usize] = color;
    }

    #[inline(always)]
    fn lookup(&self, index: usize) -> [u8; 4] {
        self.color_cache[index]
    }
}

#[derive(Debug, Clone)]
pub(crate) struct BitReader<R> {
    reader: R,
    buffer: u64,
    nbits: u8,
}

impl<R: BufRead> BitReader<R> {
    const fn new(reader: R) -> Self {
        Self {
            reader,
            buffer: 0,
            nbits: 0,
        }
    }

    /// Fills the buffer with bits from the input stream.
    ///
    /// After this function, the internal buffer will contain 64-bits or have reached the end of
    /// the input stream.
    pub(crate) fn fill(&mut self) -> Result<(), DecodingError> {
        debug_assert!(self.nbits < 64);

        let mut buf = self.reader.fill_buf()?;
        if buf.len() >= 8 {
            let lookahead = u64::from_le_bytes(buf[..8].try_into().unwrap());
            self.reader.consume(usize::from((63 - self.nbits) / 8));
            self.buffer |= lookahead << self.nbits;
            self.nbits |= 56;
        } else {
            while !buf.is_empty() && self.nbits < 56 {
                self.buffer |= u64::from(buf[0]) << self.nbits;
                self.nbits += 8;
                self.reader.consume(1);
                buf = self.reader.fill_buf()?;
            }
        }

        Ok(())
    }

    /// Peeks at the next `num` bits in the buffer.
    pub(crate) const fn peek(&self, num: u8) -> u64 {
        self.buffer & ((1 << num) - 1)
    }

    /// Peeks at the full buffer.
    pub(crate) const fn peek_full(&self) -> u64 {
        self.buffer
    }

    /// Consumes `num` bits from the buffer returning an error if there are not enough bits.
    pub(crate) fn consume(&mut self, num: u8) -> Result<(), DecodingError> {
        if self.nbits < num {
            return Err(DecodingError::BitStreamError);
        }

        self.buffer >>= num;
        self.nbits -= num;
        Ok(())
    }

    /// Convenience function to read a number of bits and convert them to a type.
    pub(crate) fn read_bits<T: TryFrom<u32>>(&mut self, num: u8) -> Result<T, DecodingError> {
        debug_assert!(num as usize <= 8 * mem::size_of::<T>());
        debug_assert!(num <= 32);

        if self.nbits < num {
            self.fill()?;
        }
        let value = self.peek(num) as u32;
        self.consume(num)?;

        value.try_into().map_err(|_| {
            debug_assert!(false, "Value too large to fit in type");
            DecodingError::BitStreamError
        })
    }
}

#[cfg(test)]
mod test {

    use std::io::Cursor;

    use super::BitReader;

    #[test]
    fn bit_read_test() {
        //10011100 01000001 11100001
        let mut bit_reader = BitReader::new(Cursor::new(vec![0x9C, 0x41, 0xE1]));

        assert_eq!(bit_reader.read_bits::<u8>(3).unwrap(), 4); //100
        assert_eq!(bit_reader.read_bits::<u8>(2).unwrap(), 3); //11
        assert_eq!(bit_reader.read_bits::<u8>(6).unwrap(), 12); //001100
        assert_eq!(bit_reader.read_bits::<u16>(10).unwrap(), 40); //0000101000
        assert_eq!(bit_reader.read_bits::<u8>(3).unwrap(), 7); //111
    }

    #[test]
    fn bit_read_error_test() {
        //01101010
        let mut bit_reader = BitReader::new(Cursor::new(vec![0x6A]));

        assert_eq!(bit_reader.read_bits::<u8>(3).unwrap(), 2); //010
        assert_eq!(bit_reader.read_bits::<u8>(5).unwrap(), 13); //01101
        assert!(bit_reader.read_bits::<u8>(4).is_err()); //error
    }
}
