//! Decoding and Encoding of WebP Images

#![forbid(unsafe_code)]
#![deny(missing_docs)]
// Increase recursion limit for the `quick_error!` macro.
#![recursion_limit = "256"]
// Enable nightly benchmark functionality if "_benchmarks" feature is enabled.
#![cfg_attr(all(test, feature = "_benchmarks"), feature(test))]
#[cfg(all(test, feature = "_benchmarks"))]
extern crate test;

pub use self::decoder::{
    decode_lossless_rgba_rows, AnimationFrameHeader, DecodingError, LoopCount, UpsamplingMethod,
    WebPDecodeOptions, WebPDecoder,
};
pub use self::alpha_blending::do_alpha_blending as blend_animation_pixel;
pub use self::encoder::{ColorType, EncoderParams, EncodingError, WebPEncoder};
pub use self::extended::{compressed_alpha_scratch_bytes, decode_compressed_alpha_residual_rows, RawAlphaRows};
pub use self::lossless::row_scratch_bytes as lossless_row_scratch_bytes;

mod alpha_blending;
mod decoder;
mod encoder;
mod extended;
mod huffman;
mod loop_filter;
mod lossless;
mod lossless_transform;
mod transform;
mod vp8_arithmetic_decoder;
mod yuv;

pub mod vp8;
