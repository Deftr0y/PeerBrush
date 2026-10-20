//! Child-only static JPEG XL raster codec. File-data callers must use the
//! isolated parent API; there is no in-process fallback for the application.
//! The callback budget is not a total native/process memory limit; see the
//! reviewed phase/copy contract. No file, engine, history or UI ownership here.
use jpegxl_sys::{
    color::{cms::JxlGetDefaultCms, cms_interface::*, color_encoding::*},
    common::{memory_manager::JxlMemoryManager, types::*},
    decode::*,
    encoder::encode::*,
    metadata::codestream_header::{JxlBasicInfo, JxlExtraChannelInfo, JxlExtraChannelType},
};
use serde::{Deserialize, Serialize};
use std::{
    alloc::{alloc, dealloc, Layout},
    ffi::c_void,
    mem::{align_of, size_of, size_of_val, MaybeUninit},
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

pub const ENCODED_LIMIT: usize = 128 * 1024 * 1024;
const HEADER_LIMIT: usize = 16 * 1024 * 1024;
const PIXEL_LIMIT: usize = 256 * 1024 * 1024;
const CALLBACK_LIMIT: usize = 512 * 1024 * 1024;
const CMS_ROW_LIMIT: usize = 16384;
const ERROR: &str = "JPEG XL is invalid, unsupported or exceeds the codec memory limit";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Info {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u16,
}
#[derive(Debug, PartialEq)]
pub enum Pixels {
    Eight(Vec<u8>),
    Sixteen(Vec<u16>),
}
#[derive(Clone, Copy)]
pub enum PixelsRef<'a> {
    Eight(&'a [u8]),
    Sixteen(&'a [u16]),
}
pub struct Decoded {
    pub info: Info,
    pub pixels: Pixels,
    pub icc_profile: Vec<u8>,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Options {
    pub lossless: bool,
    pub distance: f32,
    pub effort: u32,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            lossless: true,
            distance: 1.,
            effort: 3,
        }
    }
}
impl Options {
    pub fn validate(self) -> Result<(), String> {
        if !self.distance.is_finite()
            || !(0.1..=15.).contains(&self.distance)
            || !(1..=9).contains(&self.effort)
        {
            return Err("JPEG XL distance must be 0.1–15 and effort 1–9".into());
        }
        Ok(())
    }
}
pub fn signature(bytes: &[u8]) -> bool {
    matches!(
        unsafe { JxlSignatureCheck(bytes.as_ptr(), bytes.len().min(12)) },
        JxlSignature::Codestream | JxlSignature::Container
    )
}
fn version() -> Result<(), String> {
    if unsafe { JxlDecoderVersion() == 12000 && JxlEncoderVersion() == 12000 } {
        Ok(())
    } else {
        Err("JPEG XL native library version does not match the reviewed codec".into())
    }
}
fn sample_count(info: Info) -> Result<usize, String> {
    crate::raster::check_size(info.width, info.height)?;
    if !matches!(info.bit_depth, 8 | 16) {
        return Err(ERROR.into());
    }
    let count = (info.width as usize)
        .checked_mul(info.height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or(ERROR)?;
    if count
        .checked_mul(usize::from(info.bit_depth / 8))
        .is_none_or(|n| n > PIXEL_LIMIT)
    {
        return Err(ERROR.into());
    }
    Ok(count)
}
fn format(depth: u16) -> JxlPixelFormat {
    JxlPixelFormat {
        num_channels: 4,
        data_type: if depth == 8 {
            JxlDataType::Uint8
        } else {
            JxlDataType::Uint16
        },
        endianness: JxlEndianness::Native,
        align: 0,
    }
}
fn dec(status: JxlDecoderStatus) -> Result<(), String> {
    if status == JxlDecoderStatus::Success {
        Ok(())
    } else {
        Err(ERROR.into())
    }
}
fn enc(status: JxlEncoderStatus) -> Result<(), String> {
    if status == JxlEncoderStatus::Success {
        Ok(())
    } else {
        Err(ERROR.into())
    }
}

struct Budget {
    limit: AtomicUsize,
    live: AtomicUsize,
}
impl Budget {
    fn new(limit: usize) -> Box<Self> {
        Box::new(Self {
            limit: AtomicUsize::new(limit),
            live: AtomicUsize::new(0),
        })
    }
    fn manager(&mut self) -> JxlMemoryManager {
        JxlMemoryManager {
            opaque: ptr::from_mut(self).cast(),
            alloc: Some(budget_alloc),
            free: Some(budget_free),
        }
    }
}
unsafe extern "C-unwind" fn budget_alloc(opaque: *mut c_void, size: usize) -> *mut c_void {
    let budget = unsafe { &*opaque.cast::<Budget>() };
    let Some(total) = size.max(1).checked_add(64) else {
        return ptr::null_mut();
    };
    let Ok(layout) = Layout::from_size_align(total, 64) else {
        return ptr::null_mut();
    };
    if budget
        .live
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |live| {
            live.checked_add(total)
                .filter(|n| *n <= budget.limit.load(Ordering::SeqCst))
        })
        .is_err()
    {
        return ptr::null_mut();
    }
    let base = unsafe { alloc(layout) };
    if base.is_null() {
        budget.live.fetch_sub(total, Ordering::SeqCst);
        return ptr::null_mut();
    }
    unsafe {
        base.cast::<usize>().write(total);
        base.add(64).cast()
    }
}
unsafe extern "C-unwind" fn budget_free(opaque: *mut c_void, address: *mut c_void) {
    if address.is_null() {
        return;
    }
    let budget = unsafe { &*opaque.cast::<Budget>() };
    let base = unsafe { address.cast::<u8>().sub(64) };
    let total = unsafe { base.cast::<usize>().read() };
    unsafe {
        dealloc(base, Layout::from_size_align_unchecked(total, 64));
    }
    budget.live.fetch_sub(total, Ordering::SeqCst);
}

// jpegxl-sys 0.13 keeps these two public-C-ABI fields private. This view is
// restricted to the exact pinned repr(C) type, with size/alignment assertions.
#[repr(C)]
struct IccView {
    data: *const u8,
    size: usize,
}
const _: () = {
    assert!(size_of::<IccView>() == size_of::<JxlColorProfileIcc>());
    assert!(align_of::<IccView>() == align_of::<JxlColorProfileIcc>());
};
struct Cms {
    native: JxlCmsInterface,
    authorized: bool,
}
impl Cms {
    fn new() -> Result<Box<Self>, String> {
        let native = unsafe { JxlGetDefaultCms().as_ref() }.ok_or(ERROR)?.clone();
        Ok(Box::new(Self {
            native,
            authorized: false,
        }))
    }
    fn interface(&mut self) -> JxlCmsInterface {
        let mut native = self.native.clone();
        native.set_fields_data = ptr::from_mut(self).cast();
        native.set_fields_from_icc = cms_fields;
        native.init_data = ptr::from_mut(self).cast();
        native.init = cms_init;
        native
    }
}
extern "C-unwind" fn cms_fields(
    opaque: *mut c_void,
    data: *const u8,
    size: usize,
    color: *mut JxlColorEncoding,
    cmyk: *mut JxlBool,
) -> JxlBool {
    if opaque.is_null()
        || data.is_null()
        || color.is_null()
        || cmyk.is_null()
        || size == 0
        || size > crate::color_profile::MAX_PROFILE_BYTES
    {
        return JxlBool::False;
    }
    let cms = unsafe { &*opaque.cast::<Cms>() };
    let result =
        (cms.native.set_fields_from_icc)(cms.native.set_fields_data, data, size, color, cmyk);
    if result == JxlBool::True && unsafe { *cmyk == JxlBool::False } && sdr(unsafe { &*color }) {
        JxlBool::True
    } else {
        JxlBool::False
    }
}
fn sdr(color: &JxlColorEncoding) -> bool {
    color.color_space == JxlColorSpace::Rgb
        && !matches!(
            color.transfer_function,
            JxlTransferFunction::PQ | JxlTransferFunction::HLG
        )
        && (color.transfer_function != JxlTransferFunction::Gamma
            || (color.gamma.is_finite() && color.gamma > 0.))
}
fn cms_bounds(
    threads: usize,
    pixels: usize,
    channels: usize,
    profile_size: usize,
    intensity: f32,
) -> bool {
    threads == 1 && (1..=CMS_ROW_LIMIT).contains(&pixels) && channels == 3
        && (1..=crate::color_profile::MAX_PROFILE_BYTES).contains(&profile_size)
        && intensity.is_finite() && intensity > 0. && intensity <= 255.
        // Pinned default CMS pads to four channels and rounds to 32 floats.
        && pixels.checked_mul(4).and_then(|n| n.checked_add(31))
            .and_then(|n| (n / 32 * 32).checked_mul(threads))
            .and_then(|n| n.checked_add(32)).and_then(|n| n.checked_mul(4))
            .is_some_and(|n| n <= (CMS_ROW_LIMIT * 4 + 32) * 4)
}
extern "C-unwind" fn cms_init(
    opaque: *mut c_void,
    threads: usize,
    pixels: usize,
    input: *const JxlColorProfile,
    output: *const JxlColorProfile,
    intensity: f32,
) -> *mut c_void {
    if opaque.is_null() || input.is_null() || output.is_null() {
        return ptr::null_mut();
    }
    let cms = unsafe { &*opaque.cast::<Cms>() };
    if !cms.authorized {
        return ptr::null_mut();
    }
    for profile in [input, output] {
        let profile = unsafe { &*profile };
        let icc = unsafe { &*ptr::from_ref(&profile.icc).cast::<IccView>() };
        if icc.data.is_null()
            || !sdr(&profile.color_encoding)
            || !cms_bounds(threads, pixels, profile.num_channels, icc.size, intensity)
        {
            return ptr::null_mut();
        }
    }
    (cms.native.init)(
        cms.native.init_data,
        threads,
        pixels,
        input,
        output,
        intensity,
    )
}

struct Decoder {
    raw: *mut JxlDecoder,
    budget: Box<Budget>,
    cms: Box<Cms>,
}
impl Decoder {
    fn new() -> Result<Self, String> {
        let mut budget = Budget::new(HEADER_LIMIT);
        let raw = unsafe { JxlDecoderCreate(&budget.manager()) };
        if raw.is_null() {
            return Err(ERROR.into());
        }
        // Destroy raw explicitly if CMS construction fails before RAII owns it.
        let cms = match Cms::new() {
            Ok(cms) => cms,
            Err(error) => {
                unsafe {
                    JxlDecoderDestroy(raw);
                }
                return Err(error);
            }
        };
        let mut decoder = Self { raw, budget, cms };
        unsafe {
            dec(JxlDecoderSetCms(raw, decoder.cms.interface()))?;
            // Default no-runner execution is sequential. CMS init rejects any
            // unexpected thread count, including a changed upstream default.
            dec(JxlDecoderSetUnpremultiplyAlpha(raw, JxlBool::False))?;
        }
        Ok(decoder)
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe {
            JxlDecoderDestroy(self.raw);
        }
    }
}
struct Encoder {
    raw: *mut JxlEncoder,
    _budget: Box<Budget>,
    cms: Box<Cms>,
}
impl Encoder {
    fn new() -> Result<Self, String> {
        let mut budget = Budget::new(CALLBACK_LIMIT);
        let raw = unsafe { JxlEncoderCreate(&budget.manager()) };
        if raw.is_null() {
            return Err(ERROR.into());
        }
        let cms = match Cms::new() {
            Ok(cms) => cms,
            Err(error) => {
                unsafe {
                    JxlEncoderDestroy(raw);
                }
                return Err(error);
            }
        };
        let mut encoder = Self {
            raw,
            _budget: budget,
            cms,
        };
        encoder.cms.authorized = true;
        unsafe {
            JxlEncoderSetCms(raw, encoder.cms.interface());
        }
        Ok(encoder)
    }
}
impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe {
            JxlEncoderDestroy(self.raw);
        }
    }
}

fn basic(info: &JxlBasicInfo) -> Result<Info, String> {
    if !matches!(info.bits_per_sample, 8 | 16)
        || !matches!(info.alpha_bits, 0 | 8 | 16)
        || info.exponent_bits_per_sample != 0
        || info.alpha_exponent_bits != 0
        || info.num_color_channels != 3
        || info.have_animation != JxlBool::False
        || info.alpha_premultiplied != JxlBool::False
        || info.num_extra_channels != u32::from(info.alpha_bits != 0)
        || !info.intensity_target.is_finite()
        || info.intensity_target <= 0.
        || info.intensity_target > 255.
        || !info.min_nits.is_finite()
        || info.min_nits != 0.
        || !info.linear_below.is_finite()
        || info.linear_below != 0.
    {
        return Err("JPEG XL supports static SDR RGB with native 8/16-bit channels and optional straight alpha".into());
    }
    let result = Info {
        width: info.xsize,
        height: info.ysize,
        bit_depth: info.bits_per_sample.max(info.alpha_bits) as u16,
    };
    sample_count(result)?;
    Ok(result)
}
unsafe fn icc_profile(
    raw: *const JxlDecoder,
    target: JxlColorProfileTarget,
) -> Result<Vec<u8>, String> {
    let mut size = 0;
    unsafe {
        dec(JxlDecoderGetICCProfileSize(raw, target, &mut size))?;
    }
    if size == 0 || size > crate::color_profile::MAX_PROFILE_BYTES {
        return Err("JPEG XL ICC profile exceeds the supported 4 MiB limit".into());
    }
    let mut bytes = vec![0; size];
    unsafe {
        dec(JxlDecoderGetColorAsICCProfile(
            raw,
            target,
            bytes.as_mut_ptr(),
            bytes.len(),
        ))?;
    }
    Ok(bytes)
}
fn validate_original_profile(cms: &mut Cms, bytes: &[u8]) -> Result<(), String> {
    crate::color_profile::supported(bytes)?;
    // Raw ICC headers may not expose structured fields. Preserve the RGB,
    // CMYK and HDR checks formerly reached through the output-profile setter.
    let mut color = MaybeUninit::<JxlColorEncoding>::uninit();
    let mut cmyk = JxlBool::False;
    if cms_fields(
        ptr::from_mut(cms).cast(),
        bytes.as_ptr(),
        bytes.len(),
        color.as_mut_ptr(),
        &mut cmyk,
    ) != JxlBool::True
    {
        return Err("HDR or non-RGB JPEG XL color encodings are not supported".into());
    }
    Ok(())
}
unsafe fn profile(decoder: &mut Decoder) -> Result<Vec<u8>, String> {
    let mut color = MaybeUninit::<JxlColorEncoding>::uninit();
    if unsafe {
        JxlDecoderGetColorAsEncodedProfile(
            decoder.raw,
            JxlColorProfileTarget::Original,
            color.as_mut_ptr(),
        )
    } == JxlDecoderStatus::Success
        && !sdr(unsafe { &color.assume_init() })
    {
        return Err("HDR or non-RGB JPEG XL color encodings are not supported".into());
    }
    let original = unsafe { icc_profile(decoder.raw, JxlColorProfileTarget::Original)? };
    validate_original_profile(&mut decoder.cms, &original)?;
    decoder.cms.authorized = true;
    let data = unsafe { icc_profile(decoder.raw, JxlColorProfileTarget::Data)? };
    // Pinned libjxl 0.12 already uses Original for non-XYB samples. Reapplying
    // a valid raw ICC with an unknown structured transfer function can fail.
    if data == original {
        return Ok(original);
    }
    drop(data);
    // XYB defaults can differ from Original. Select representable SDR sRGB,
    // then return the profile of the actual output, never relabel its samples.
    unsafe {
        JxlColorEncodingSetToSRGB(color.as_mut_ptr(), JxlBool::False);
        dec(JxlDecoderSetOutputColorProfile(
            decoder.raw,
            color.as_ptr(),
            ptr::null(),
            0,
        ))?;
    }
    let data = unsafe { icc_profile(decoder.raw, JxlColorProfileTarget::Data)? };
    crate::color_profile::supported(&data)?;
    Ok(data)
}
fn read(
    bytes: &[u8],
    pixels: bool,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Decoded, String> {
    check()?;
    version()?;
    if bytes.len() > ENCODED_LIMIT || !signature(bytes) {
        return Err(ERROR.into());
    }
    // Registered storage is declared first and outlives native destruction on
    // every early return, including cancellation and denied allocations.
    let mut output = Pixels::Eight(Vec::new());
    let mut decoder = Decoder::new()?;
    let mut info = None;
    let mut icc = None;
    let mut registered = false;
    let mut finished = false;
    unsafe {
        let events = JxlDecoderStatus::BasicInfo as i32
            | JxlDecoderStatus::ColorEncoding as i32
            | JxlDecoderStatus::FullImage as i32;
        dec(JxlDecoderSubscribeEvents(decoder.raw, events))?;
        dec(JxlDecoderSetInput(decoder.raw, bytes.as_ptr(), bytes.len()))?;
        JxlDecoderCloseInput(decoder.raw);
        loop {
            check()?;
            match JxlDecoderProcessInput(decoder.raw) {
                JxlDecoderStatus::BasicInfo => {
                    if info.is_some() {
                        return Err(ERROR.into());
                    }
                    let mut raw = MaybeUninit::<JxlBasicInfo>::uninit();
                    dec(JxlDecoderGetBasicInfo(decoder.raw, raw.as_mut_ptr()))?;
                    let raw = raw.assume_init();
                    info = Some(basic(&raw)?);
                    if raw.alpha_bits != 0 {
                        let mut extra = MaybeUninit::<JxlExtraChannelInfo>::uninit();
                        dec(JxlDecoderGetExtraChannelInfo(
                            decoder.raw,
                            0,
                            extra.as_mut_ptr(),
                        ))?;
                        let extra = extra.assume_init();
                        if extra.r#type != JxlExtraChannelType::Alpha
                            || extra.dim_shift != 0
                            || extra.bits_per_sample != raw.alpha_bits
                            || extra.exponent_bits_per_sample != 0
                            || extra.alpha_premultiplied != JxlBool::False
                        {
                            return Err(ERROR.into());
                        }
                    }
                }
                JxlDecoderStatus::ColorEncoding => {
                    if info.is_none() || icc.is_some() {
                        return Err(ERROR.into());
                    }
                    icc = Some(profile(&mut decoder)?);
                    check()?;
                    if !pixels {
                        break;
                    }
                    decoder.budget.limit.store(CALLBACK_LIMIT, Ordering::SeqCst);
                }
                JxlDecoderStatus::NeedImageOutBuffer => {
                    if icc.is_none() || registered {
                        return Err(ERROR.into());
                    }
                    let info = info.ok_or(ERROR)?;
                    let count = sample_count(info)?;
                    let mut size = 0;
                    dec(JxlDecoderImageOutBufferSize(
                        decoder.raw,
                        &format(info.bit_depth),
                        &mut size,
                    ))?;
                    if size != count * usize::from(info.bit_depth / 8) {
                        return Err(ERROR.into());
                    }
                    let pointer = if info.bit_depth == 8 {
                        let mut bytes = Vec::new();
                        bytes.try_reserve_exact(count).map_err(|_| ERROR)?;
                        bytes.resize(count, 0);
                        output = Pixels::Eight(bytes);
                        match &mut output {
                            Pixels::Eight(bytes) => bytes.as_mut_ptr().cast(),
                            _ => unreachable!(),
                        }
                    } else {
                        let mut words = Vec::new();
                        words.try_reserve_exact(count).map_err(|_| ERROR)?;
                        words.resize(count, 0);
                        output = Pixels::Sixteen(words);
                        match &mut output {
                            Pixels::Sixteen(words) => words.as_mut_ptr().cast(),
                            _ => unreachable!(),
                        }
                    };
                    dec(JxlDecoderSetImageOutBuffer(
                        decoder.raw,
                        &format(info.bit_depth),
                        pointer,
                        size,
                    ))?;
                    registered = true;
                }
                JxlDecoderStatus::FullImage => {
                    if !registered || finished {
                        return Err(ERROR.into());
                    }
                    finished = true;
                }
                JxlDecoderStatus::Success if finished => break,
                _ => return Err(ERROR.into()),
            }
        }
    }
    check()?;
    Ok(Decoded {
        info: info.ok_or(ERROR)?,
        pixels: output,
        icc_profile: icc.ok_or(ERROR)?,
    })
}
pub fn inspect(bytes: &[u8], check: &dyn Fn() -> Result<(), String>) -> Result<Info, String> {
    read(bytes, false, check).map(|decoded| decoded.info)
}
pub fn decode(bytes: &[u8], check: &dyn Fn() -> Result<(), String>) -> Result<Decoded, String> {
    read(bytes, true, check)
}
pub fn encode(
    width: u32,
    height: u32,
    pixels: PixelsRef<'_>,
    profile: Option<&[u8]>,
    options: Options,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Vec<u8>, String> {
    check()?;
    version()?;
    options.validate()?;
    let (depth, count, pointer, length) = match pixels {
        PixelsRef::Eight(bytes) => (
            8,
            bytes.len(),
            bytes.as_ptr().cast::<c_void>(),
            size_of_val(bytes),
        ),
        PixelsRef::Sixteen(words) => (
            16,
            words.len(),
            words.as_ptr().cast::<c_void>(),
            size_of_val(words),
        ),
    };
    if sample_count(Info {
        width,
        height,
        bit_depth: depth,
    })? != count
    {
        return Err("JPEG XL RGBA sample count differs from its dimensions".into());
    }
    let profile = profile.unwrap_or_else(|| crate::color_profile::srgb_profile());
    crate::color_profile::supported(profile)?;
    check()?;
    let encoder = Encoder::new()?;
    unsafe {
        let mut info = MaybeUninit::<JxlBasicInfo>::uninit();
        JxlEncoderInitBasicInfo(info.as_mut_ptr());
        let mut info = info.assume_init();
        info.xsize = width;
        info.ysize = height;
        info.bits_per_sample = u32::from(depth);
        info.num_color_channels = 3;
        info.num_extra_channels = 1;
        info.alpha_bits = u32::from(depth);
        info.alpha_premultiplied = JxlBool::False;
        info.uses_original_profile = if options.lossless {
            JxlBool::True
        } else {
            JxlBool::False
        };
        enc(JxlEncoderSetBasicInfo(encoder.raw, &info))?;
        enc(JxlEncoderSetICCProfile(
            encoder.raw,
            profile.as_ptr(),
            profile.len(),
        ))?;
        let settings = JxlEncoderFrameSettingsCreate(encoder.raw, ptr::null());
        if settings.is_null() {
            return Err(ERROR.into());
        }
        enc(JxlEncoderSetFrameLossless(
            settings,
            if options.lossless {
                JxlBool::True
            } else {
                JxlBool::False
            },
        ))?;
        if !options.lossless {
            enc(JxlEncoderSetFrameDistance(settings, options.distance))?;
        }
        enc(JxlEncoderSetExtraChannelDistance(settings, 0, 0.))?;
        enc(JxlEncoderFrameSettingsSetOption(
            settings,
            JxlEncoderFrameSettingId::KeepInvisible,
            1,
        ))?;
        enc(JxlEncoderFrameSettingsSetOption(
            settings,
            JxlEncoderFrameSettingId::Effort,
            i64::from(options.effort),
        ))?;
        check()?;
        enc(JxlEncoderAddImageFrame(
            settings,
            &format(depth),
            pointer,
            length,
        ))?;
        JxlEncoderCloseInput(encoder.raw);
        let mut output = Vec::new();
        loop {
            check()?;
            let mut buffer = [0; 16384];
            let mut next = buffer.as_mut_ptr();
            let mut available = buffer.len();
            let status = JxlEncoderProcessOutput(encoder.raw, &mut next, &mut available);
            let used = buffer.len().checked_sub(available).ok_or(ERROR)?;
            if output
                .len()
                .checked_add(used)
                .is_none_or(|n| n > ENCODED_LIMIT)
            {
                return Err(ERROR.into());
            }
            output.try_reserve(used).map_err(|_| ERROR)?;
            output.extend_from_slice(&buffer[..used]);
            match status {
                JxlEncoderStatus::Success => break,
                JxlEncoderStatus::NeedMoreOutput => {}
                _ => return Err(ERROR.into()),
            }
        }
        check()?;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_allocator_denial_alignment_and_cleanup() {
        let mut budget = Budget::new(4096);
        let manager = budget.manager();
        unsafe {
            assert!(budget_alloc(manager.opaque, usize::MAX).is_null());
            let allocated = budget_alloc(manager.opaque, 1000);
            assert!(!allocated.is_null());
            assert_eq!(allocated as usize % 64, 0);
            budget
                .limit
                .store(budget.live.load(Ordering::SeqCst), Ordering::SeqCst);
            assert!(budget_alloc(manager.opaque, 1).is_null());
            budget_free(manager.opaque, allocated);
        }
        assert_eq!(budget.live.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn cms_and_profile_guards_reject_before_default_cms() {
        assert!(cms_bounds(1, 8192, 3, 1024, 255.));
        for (threads, pixels, channels, size, intensity) in [
            (0, 1, 3, 132, 255.),
            (2, 1, 3, 132, 255.),
            (1, usize::MAX, 3, 132, 255.),
            (1, CMS_ROW_LIMIT + 1, 3, 132, 255.),
            (1, 1, 4, 132, 255.),
            (1, 1, 3, crate::color_profile::MAX_PROFILE_BYTES + 1, 255.),
            (1, 1, 3, 132, f32::NAN),
            (1, 1, 3, 132, 256.),
        ] {
            assert!(!cms_bounds(threads, pixels, channels, size, intensity));
        }
        assert_eq!(
            cms_fields(
                ptr::null_mut(),
                ptr::null(),
                usize::MAX,
                ptr::null_mut(),
                ptr::null_mut()
            ),
            JxlBool::False
        );
        assert!(cms_init(ptr::null_mut(), 1, 1, ptr::null(), ptr::null(), 255.).is_null());
    }
    #[test]
    fn raw_original_profile_guard_rejects_hdr_cmyk_and_non_rgb_before_bypass() {
        extern "C-unwind" fn fields(
            opaque: *mut c_void,
            _: *const u8,
            _: usize,
            color: *mut JxlColorEncoding,
            cmyk: *mut JxlBool,
        ) -> JxlBool {
            unsafe {
                JxlColorEncodingSetToSRGB(color, JxlBool::False);
                *cmyk = JxlBool::False;
                match *opaque.cast::<u8>() {
                    0 => (*color).transfer_function = JxlTransferFunction::Unknown,
                    1 => (*color).transfer_function = JxlTransferFunction::PQ,
                    2 => (*color).transfer_function = JxlTransferFunction::HLG,
                    3 => *cmyk = JxlBool::True,
                    4 => (*color).color_space = JxlColorSpace::Gray,
                    _ => return JxlBool::False,
                }
            }
            JxlBool::True
        }
        let mut cms = Cms::new().unwrap();
        let mut variant = 0_u8;
        cms.native.set_fields_data = ptr::from_mut(&mut variant).cast();
        cms.native.set_fields_from_icc = fields;
        let profile = crate::color_profile::srgb_profile();
        assert!(validate_original_profile(&mut cms, profile).is_ok());
        for rejected in 1..=5 {
            variant = rejected;
            assert!(validate_original_profile(&mut cms, profile).is_err());
        }
        assert!(!cms.authorized);
        assert_eq!(variant, 5);
    }
    #[test]
    fn metadata_rejects_unsupported_precision_features_and_preserves_alpha_depth() {
        let mut raw = MaybeUninit::<JxlBasicInfo>::uninit();
        unsafe {
            JxlEncoderInitBasicInfo(raw.as_mut_ptr());
        }
        let mut raw = unsafe { raw.assume_init() };
        raw.xsize = 3;
        raw.ysize = 2;
        raw.bits_per_sample = 8;
        raw.num_color_channels = 3;
        raw.alpha_bits = 16;
        raw.num_extra_channels = 1;
        raw.intensity_target = 255.;
        assert_eq!(basic(&raw).unwrap().bit_depth, 16);
        for change in 0..9 {
            let mut invalid = raw.clone();
            match change {
                0 => invalid.have_animation = JxlBool::True,
                1 => invalid.alpha_premultiplied = JxlBool::True,
                2 => invalid.num_extra_channels = 2,
                3 => invalid.bits_per_sample = 32,
                4 => invalid.exponent_bits_per_sample = 8,
                5 => invalid.num_color_channels = 1,
                6 => invalid.xsize = 0,
                7 => invalid.intensity_target = 1000.,
                _ => invalid.alpha_exponent_bits = 5,
            }
            assert!(basic(&invalid).is_err());
        }
    }
    // Trusted tiny native fixtures exercise metadata independent of encode's
    // stricter public options. No files or application state are involved.
    fn fixture(change: impl FnOnce(&mut JxlBasicInfo), gray: bool, hdr: bool) -> Vec<u8> {
        let encoder = Encoder::new().unwrap();
        unsafe {
            let mut raw = MaybeUninit::<JxlBasicInfo>::uninit();
            JxlEncoderInitBasicInfo(raw.as_mut_ptr());
            let mut raw = raw.assume_init();
            raw.xsize = 3;
            raw.ysize = 2;
            raw.bits_per_sample = 16;
            raw.num_color_channels = if gray { 1 } else { 3 };
            raw.num_extra_channels = 1;
            raw.alpha_bits = 16;
            raw.uses_original_profile = JxlBool::True;
            change(&mut raw);
            let native = (*JxlGetDefaultCms()).clone();
            JxlEncoderSetCms(encoder.raw, native);
            enc(JxlEncoderSetBasicInfo(encoder.raw, &raw)).unwrap();
            let mut color = MaybeUninit::<JxlColorEncoding>::uninit();
            JxlColorEncodingSetToSRGB(
                color.as_mut_ptr(),
                if gray { JxlBool::True } else { JxlBool::False },
            );
            let mut color = color.assume_init();
            if hdr {
                color.transfer_function = JxlTransferFunction::PQ;
            }
            enc(JxlEncoderSetColorEncoding(encoder.raw, &color)).unwrap();
            let settings = JxlEncoderFrameSettingsCreate(encoder.raw, ptr::null());
            assert!(!settings.is_null());
            enc(JxlEncoderSetFrameLossless(settings, JxlBool::True)).unwrap();
            enc(JxlEncoderFrameSettingsSetOption(
                settings,
                JxlEncoderFrameSettingId::KeepInvisible,
                1,
            ))
            .unwrap();
            let channels = raw.num_color_channels + u32::from(raw.alpha_bits != 0);
            let words: Vec<u16> = (0..6 * channels)
                .map(|i| (i * 1579 + 12347) as u16)
                .collect();
            let mut pixel_format = format(16);
            pixel_format.num_channels = channels;
            enc(JxlEncoderAddImageFrame(
                settings,
                &pixel_format,
                words.as_ptr().cast(),
                size_of_val(words.as_slice()),
            ))
            .unwrap();
            JxlEncoderCloseInput(encoder.raw);
            let mut output = vec![0; 65536];
            let mut next = output.as_mut_ptr();
            let mut available = output.len();
            enc(JxlEncoderProcessOutput(
                encoder.raw,
                &mut next,
                &mut available,
            ))
            .unwrap();
            output.truncate(output.len() - available);
            output
        }
    }
    #[test]
    fn actual_mixed_alpha_depth_uses_native16_and_orientation_is_applied() {
        let encoded = fixture(|raw| raw.bits_per_sample = 8, false, false);
        let decoded = decode(&encoded, &|| Ok(())).unwrap();
        assert_eq!(decoded.info.bit_depth, 16);
        let Pixels::Sixteen(words) = decoded.pixels else {
            panic!("16-bit alpha was projected")
        };
        for (index, alpha) in words.iter().skip(3).step_by(4).enumerate() {
            assert_eq!(*alpha, ((index * 4 + 3) * 1579 + 12347) as u16);
        }
        assert!(words
            .chunks_exact(4)
            .all(|p| p[..3].iter().all(|v| v % 257 == 0)));
        let encoded = fixture(
            |raw| {
                raw.orientation =
                    jpegxl_sys::metadata::codestream_header::JxlOrientation::Rotate90Cw
            },
            false,
            false,
        );
        let decoded = decode(&encoded, &|| Ok(())).unwrap();
        assert_eq!((decoded.info.width, decoded.info.height), (2, 3));
        let Pixels::Sixteen(words) = decoded.pixels else {
            panic!("16-bit orientation was projected")
        };
        // Native orientation transforms all channels, without projecting words.
        let expected: Vec<u16> = [3, 0, 4, 1, 5, 2]
            .into_iter()
            .flat_map(|p| (p * 4..p * 4 + 4).map(|i| (i * 1579 + 12347) as u16))
            .collect();
        assert_eq!(words, expected);
    }
    #[test]
    fn actual_unsupported_gray_hdr_and_associated_alpha_are_explicit_errors() {
        let gray = fixture(|_| {}, true, false);
        assert!(inspect(&gray, &|| Ok(())).is_err());
        let hdr = fixture(|raw| raw.intensity_target = 1000., false, true);
        assert!(inspect(&hdr, &|| Ok(())).is_err());
        let associated = fixture(|raw| raw.alpha_premultiplied = JxlBool::True, false, false);
        assert!(decode(&associated, &|| Ok(())).is_err());
    }
    #[test]
    fn trusted_fixture_cancellation_after_native_output_registration_keeps_storage_alive() {
        use std::cell::Cell;
        let bytes = vec![47_u8; 3 * 2 * 4];
        let words = vec![12347_u16; 3 * 2 * 4];
        let eight = encode(
            3,
            2,
            PixelsRef::Eight(&bytes),
            None,
            Options::default(),
            &|| Ok(()),
        )
        .unwrap();
        let sixteen = encode(
            3,
            2,
            PixelsRef::Sixteen(&words),
            None,
            Options::default(),
            &|| Ok(()),
        )
        .unwrap();
        for payload in [&eight, &sixteen] {
            // Check0 initial,1 BASIC,2 COLOR,3 after validated profile,
            //4 NEED_OUT,5 registered pointer,6 FULL_IMAGE before SUCCESS.
            for stop in [5, 6] {
                let count = Cell::new(0);
                let check = || {
                    let current = count.get();
                    count.set(current + 1);
                    if current >= stop {
                        Err("fixture canceled".into())
                    } else {
                        Ok(())
                    }
                };
                assert!(decode(payload, &check).is_err());
            }
        }
    }
}
