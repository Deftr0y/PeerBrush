//! Child-only bounded compatible JPEG XL encoder. No application-process route.
use super::{
    native,
    stream_protocol::{self as wire, Configuration, Direction, Frame, Message, Rectangle, State},
};
use jpegxl_sys::{common::types::*, encoder::encode::*, metadata::codestream_header::JxlBasicInfo};
use ring::digest::{digest, SHA256};
use std::{
    cell::UnsafeCell,
    ffi::c_void,
    io::{self, Cursor, Read, Write},
    mem::{align_of, offset_of, size_of, MaybeUninit},
    panic::{catch_unwind, AssertUnwindSafe},
    ptr,
    sync::atomic::{AtomicBool, Ordering},
    thread::{self, ThreadId},
    time::Instant,
};
type Result<T> = std::result::Result<T, String>;
const ERROR: &str = "JPEG XL streaming encoder failed; current work was preserved";

// The exact pinned jpegxl-sys 0.13 repr(C) fields are private. These views match
// libjxl 0.12.0's public encode.h callback signatures and field offsets; conversion
// is restricted to these two types, with layout checks before native use.
// Pinned encode.h SHA256:
// dcef8b08c430d4c15b10268eb9f6d328a8145ba8ea27435826908622f3cbead5
#[repr(C)]
struct OutputView {
    opaque: *mut c_void,
    get_buffer: extern "C-unwind" fn(*mut c_void, *mut usize) -> *mut c_void,
    release_buffer: extern "C-unwind" fn(*mut c_void, usize),
    seek: Option<extern "C-unwind" fn(*mut c_void, u64)>,
    set_finalized_position: extern "C-unwind" fn(*mut c_void, u64),
}
#[repr(C)]
struct InputView {
    opaque: *mut c_void,
    get_format: extern "C-unwind" fn(*mut c_void, *mut JxlPixelFormat),
    get_data:
        extern "C-unwind" fn(*mut c_void, usize, usize, usize, usize, *mut usize) -> *const c_void,
    get_extra_format: extern "C-unwind" fn(*mut c_void, usize, *mut JxlPixelFormat),
    get_extra_data: extern "C-unwind" fn(
        *mut c_void,
        usize,
        usize,
        usize,
        usize,
        usize,
        *mut usize,
    ) -> *const c_void,
    release: extern "C-unwind" fn(*mut c_void, *const c_void),
}
const _: () = {
    let p = size_of::<*mut c_void>();
    assert!(size_of::<OutputView>() == size_of::<JxlEncoderOutputProcessor>());
    assert!(align_of::<OutputView>() == align_of::<JxlEncoderOutputProcessor>());
    assert!(size_of::<OutputView>() == 5 * p);
    assert!(offset_of!(OutputView, opaque) == 0 && offset_of!(OutputView, get_buffer) == p);
    assert!(
        offset_of!(OutputView, release_buffer) == 2 * p && offset_of!(OutputView, seek) == 3 * p
    );
    assert!(offset_of!(OutputView, set_finalized_position) == 4 * p);
    assert!(size_of::<InputView>() == size_of::<JxlChunkedFrameInputSource>());
    assert!(align_of::<InputView>() == align_of::<JxlChunkedFrameInputSource>());
    assert!(size_of::<InputView>() == 6 * p);
    assert!(offset_of!(InputView, opaque) == 0 && offset_of!(InputView, get_format) == p);
    assert!(
        offset_of!(InputView, get_data) == 2 * p
            && offset_of!(InputView, get_extra_format) == 3 * p
    );
    assert!(
        offset_of!(InputView, get_extra_data) == 4 * p && offset_of!(InputView, release) == 5 * p
    );
};
struct Held {
    ids: [u64; wire::MAX_HELD_BUFFERS as usize],
    id_count: usize,
    words: Vec<u64>,
    bytes: usize,
}
impl Held {
    fn pointer(&self) -> *const c_void {
        self.words.as_ptr().cast()
    }
    fn allocated(&self) -> usize {
        self.words.capacity() * size_of::<u64>()
    }
    fn bytes_mut(&mut self) -> &mut [u8] {
        // Initialized u64 backing gives native16 callbacks suitable alignment.
        unsafe { std::slice::from_raw_parts_mut(self.words.as_mut_ptr().cast(), self.bytes) }
    }
}
struct Inner<'a> {
    configuration: Configuration,
    state: State,
    input: &'a mut dyn Read,
    output: &'a mut dyn Write,
    check: &'a dyn Fn() -> Result<()>,
    started: Instant,
    scratch: [u8; wire::BUFFER_BYTES],
    buffer: [u8; wire::BUFFER_BYTES],
    output_active: bool,
    position: u64,
    highest: u64,
    records: u32,
    last_id: u64,
    format_called: bool,
    extra_format_called: bool,
    held: [Option<Held>; wire::MAX_HELD_BUFFERS as usize],
    held_bytes: usize,
    error: Option<String>,
}
struct Callbacks<'a> {
    owner: ThreadId,
    busy: AtomicBool,
    failed: AtomicBool,
    inner: UnsafeCell<Inner<'a>>,
}
impl Inner<'_> {
    fn active(&self) -> Result<()> {
        (self.check)()?;
        if self.started.elapsed().as_millis() > u128::from(self.configuration.max_elapsed_ms) {
            return Err("JPEG XL streaming deadline exceeded".into());
        }
        Ok(())
    }
    fn send(&mut self, message: Message) -> Result<()> {
        self.active()?;
        let started = self.started;
        let deadline = self.configuration.max_elapsed_ms;
        let check = self.check;
        let active = || {
            check()?;
            if started.elapsed().as_millis() > u128::from(deadline) {
                return Err("JPEG XL streaming deadline exceeded".into());
            }
            Ok(())
        };
        self.state.send(
            Direction::Child,
            message,
            self.output,
            &mut io::empty(),
            &mut self.scratch,
            &active,
        )
    }
    fn ack(&mut self) -> Result<()> {
        self.active()?;
        let started = self.started;
        let deadline = self.configuration.max_elapsed_ms;
        let check = self.check;
        let active = || {
            check()?;
            if started.elapsed().as_millis() > u128::from(deadline) {
                return Err("JPEG XL streaming deadline exceeded".into());
            }
            Ok(())
        };
        let message = self.state.receive(
            Direction::Parent,
            self.input,
            &mut self.scratch,
            &active,
            &mut |_| Err(ERROR.into()),
        )?;
        if !matches!(message, Message::Continue { .. }) {
            return Err(ERROR.into());
        }
        self.active()
    }
    fn event(&mut self, message: Message) -> Result<()> {
        self.send(message)?;
        self.ack()
    }
    fn get_rectangle(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        stride: &mut usize,
        alpha_only: bool,
    ) -> Result<*const c_void> {
        self.active()?;
        let info = self.configuration.info;
        if width == 0
            || height == 0
            || width > 2064
            || height > 2064
            || x % 8 != 0
            || y % 8 != 0
            || x.checked_add(width).is_none_or(|n| n > info.width as usize)
            || y.checked_add(height)
                .is_none_or(|n| n > info.height as usize)
            || (width % 8 != 0 && x + width != info.width as usize)
            || (height % 8 != 0 && y + height != info.height as usize)
        {
            return Err("JPEG XL requested an invalid native input rectangle".into());
        }
        let native = usize::from(info.bit_depth / 8);
        let pixel = 4 * native;
        let channels = if alpha_only { 1 } else { 3 };
        let destination_pixel = channels * native;
        let bytes = width
            .checked_mul(height)
            .and_then(|n| n.checked_mul(destination_pixel))
            .filter(|n| *n <= wire::MAX_RECT_BYTES as usize)
            .ok_or("JPEG XL native input rectangle exceeds bounded owner")?;
        // Pinned enc_frame.cc adds8-pixel halos to2048 groups. RGB3 and separate
        // alpha1 owners each stay <=32MiB at2064 edges. Never assembleRGBA2064;
        // all wire windows remain <=2048 with SAME8IDs/128MiB accounting.
        let parts = width
            .div_ceil(2048)
            .checked_mul(height.div_ceil(2048))
            .ok_or(ERROR)?;
        let held_ids: usize = self.held.iter().flatten().map(|h| h.id_count).sum();
        if parts == 0
            || parts > wire::MAX_HELD_BUFFERS as usize
            || held_ids
                .checked_add(parts)
                .is_none_or(|n| n > usize::from(self.configuration.max_held_buffers))
        {
            return Err(ERROR.into());
        }
        let slot = self.held.iter().position(Option::is_none).ok_or(ERROR)?;
        let words = bytes.checked_add(7).ok_or(ERROR)? / 8;
        let allocation = words.checked_mul(8).ok_or(ERROR)?;
        if self.held_bytes.checked_add(allocation).is_none_or(|n| {
            n.checked_add(2 * wire::BUFFER_BYTES)
                .is_none_or(|total| total > self.configuration.max_held_bytes as usize)
        }) {
            return Err(ERROR.into());
        }
        let mut data = Vec::new();
        data.try_reserve_exact(words).map_err(|_| ERROR)?;
        data.resize(words, 0u64);
        let mut held = Held {
            ids: [0; wire::MAX_HELD_BUFFERS as usize],
            id_count: 0,
            words: data,
            bytes,
        };
        if held.allocated() != allocation {
            return Err(ERROR.into());
        }
        for top in (0..height).step_by(2048) {
            for left in (0..width).step_by(2048) {
                self.active()?;
                let id = self.last_id.checked_add(1).ok_or(ERROR)?;
                let w = (width - left).min(2048);
                let h = (height - top).min(2048);
                let rectangle = Rectangle {
                    id,
                    x: (x + left).try_into().map_err(|_| ERROR)?,
                    y: (y + top).try_into().map_err(|_| ERROR)?,
                    width: w.try_into().map_err(|_| ERROR)?,
                    height: h.try_into().map_err(|_| ERROR)?,
                };
                let count =
                    usize::try_from(rectangle.bytes(&self.configuration)?).map_err(|_| ERROR)?;
                self.send(Message::NeedRectangle(rectangle))?;
                let mut copied = 0usize;
                let started = self.started;
                let deadline = self.configuration.max_elapsed_ms;
                let check = self.check;
                let active = || {
                    check()?;
                    if started.elapsed().as_millis() > u128::from(deadline) {
                        return Err("JPEG XL streaming deadline exceeded".into());
                    }
                    Ok(())
                };
                let target = held.bytes_mut();
                let message = self.state.receive(
                    Direction::Parent,
                    self.input,
                    &mut self.scratch,
                    &active,
                    &mut |chunk| {
                        if copied.checked_add(chunk.len()).is_none_or(|n| n > count)
                            || copied % pixel != 0
                            || chunk.len() % pixel != 0
                        {
                            return Err(ERROR.into());
                        }
                        for rgba in chunk.chunks_exact(pixel) {
                            let ordinal = copied / pixel;
                            let line = ordinal / w;
                            let column = ordinal % w;
                            let start = ((top + line) * width + left + column) * destination_pixel;
                            let end = start
                                .checked_add(destination_pixel)
                                .filter(|n| *n <= target.len())
                                .ok_or(ERROR)?;
                            target[start..end].copy_from_slice(if alpha_only {
                                &rgba[3 * native..]
                            } else {
                                &rgba[..3 * native]
                            });
                            copied += pixel;
                        }
                        Ok(())
                    },
                )?;
                if !matches!(message,Message::RectangleData{id:received,..}if received==id)
                    || copied != count
                {
                    return Err(ERROR.into());
                }
                held.ids[held.id_count] = id;
                held.id_count += 1;
                self.last_id = id;
            }
        }
        self.active()?;
        if held.id_count != parts {
            return Err(ERROR.into());
        }
        let pointer = held.pointer();
        self.held_bytes += allocation;
        self.held[slot] = Some(held);
        *stride = width.checked_mul(destination_pixel).ok_or(ERROR)?;
        Ok(pointer)
    }
    fn release(&mut self, pointer: *const c_void, notify: bool) -> Result<()> {
        let slot = self
            .held
            .iter()
            .position(|h| h.as_ref().is_some_and(|h| h.pointer() == pointer))
            .ok_or(ERROR)?;
        let held = self.held[slot].take().ok_or(ERROR)?;
        self.held_bytes = self.held_bytes.checked_sub(held.allocated()).ok_or(ERROR)?;
        let ids = held.ids;
        let count = held.id_count;
        drop(held);
        if notify {
            for &id in &ids[..count] {
                self.event(Message::Release { id })?;
            }
        }
        Ok(())
    }
}
/// All callbacks use the default sequential runner. Unexpected threads or
/// reentry fail through atomics before any mutable context access is attempted.
fn callback<R>(
    opaque: *mut c_void,
    fallback: R,
    cleanup: bool,
    call: impl FnOnce(&mut Inner<'_>, bool) -> Result<R>,
) -> R {
    if opaque.is_null() {
        return fallback;
    }
    let callbacks = unsafe { &*opaque.cast::<Callbacks<'_>>() };
    if thread::current().id() != callbacks.owner
        || callbacks
            .busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
    {
        callbacks.failed.store(true, Ordering::SeqCst);
        return fallback;
    }
    struct Busy<'a>(&'a AtomicBool);
    impl Drop for Busy<'_> {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }
    let _busy = Busy(&callbacks.busy);
    let failed = callbacks.failed.load(Ordering::SeqCst);
    if failed && !cleanup {
        return fallback;
    }
    let inner = unsafe { &mut *callbacks.inner.get() };
    let result = catch_unwind(AssertUnwindSafe(|| {
        if !failed && !cleanup {
            inner.active()?;
        }
        call(inner, !failed)
    }));
    match result {
        Ok(Ok(value)) => value,
        other => {
            callbacks.failed.store(true, Ordering::SeqCst);
            if inner.error.is_none() {
                inner.error = Some(match other {
                    Ok(Err(e)) => e,
                    _ => ERROR.into(),
                });
            }
            fallback
        }
    }
}
extern "C-unwind" fn get_format(opaque: *mut c_void, format: *mut JxlPixelFormat) {
    callback(opaque, (), false, |inner, _| {
        if format.is_null() {
            return Err(ERROR.into());
        }
        // Pinned encode.cc and enc_frame.cc both query this immutable format,
        // despite the public header's once-only wording. Return the same value.
        let mut value = native::format(inner.configuration.info.bit_depth);
        value.num_channels = 3;
        value.endianness = JxlEndianness::Big;
        unsafe {
            format.write(value);
        }
        inner.format_called = true;
        Ok(())
    });
}
extern "C-unwind" fn get_data(
    opaque: *mut c_void,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    stride: *mut usize,
) -> *const c_void {
    callback(opaque, ptr::null(), false, |inner, _| {
        if stride.is_null() || !inner.format_called {
            return Err(ERROR.into());
        }
        inner.get_rectangle(x, y, width, height, unsafe { &mut *stride }, false)
    })
}
extern "C-unwind" fn extra_format(opaque: *mut c_void, index: usize, format: *mut JxlPixelFormat) {
    callback(opaque, (), false, |inner, _| {
        if index != 0 || format.is_null() {
            return Err(ERROR.into());
        }
        let mut value = native::format(inner.configuration.info.bit_depth);
        value.num_channels = 1;
        value.endianness = JxlEndianness::Big;
        unsafe {
            format.write(value);
        }
        inner.extra_format_called = true;
        Ok(())
    });
}
extern "C-unwind" fn extra_data(
    opaque: *mut c_void,
    index: usize,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    stride: *mut usize,
) -> *const c_void {
    callback(opaque, ptr::null(), false, |inner, _| {
        if index != 0 || stride.is_null() || !inner.extra_format_called {
            return Err(ERROR.into());
        }
        inner.get_rectangle(x, y, width, height, unsafe { &mut *stride }, true)
    })
}
extern "C-unwind" fn release_input(opaque: *mut c_void, pointer: *const c_void) {
    callback(opaque, (), true, |inner, notify| {
        inner.release(pointer, notify)
    });
}
extern "C-unwind" fn get_output(opaque: *mut c_void, size: *mut usize) -> *mut c_void {
    if !size.is_null() {
        unsafe {
            size.write(0);
        }
    }
    callback(opaque, ptr::null_mut(), false, |inner, _| {
        if size.is_null() || inner.output_active {
            return Err(ERROR.into());
        }
        inner.output_active = true;
        unsafe {
            size.write(wire::BUFFER_BYTES);
        }
        Ok(inner.buffer.as_mut_ptr().cast())
    })
}
extern "C-unwind" fn release_output(opaque: *mut c_void, bytes: usize) {
    callback(opaque, (), true, |inner, notify| {
        if !inner.output_active || bytes > wire::BUFFER_BYTES {
            return Err(ERROR.into());
        }
        inner.output_active = false;
        if !notify || bytes == 0 {
            return Ok(());
        }
        let sha256 = digest(&SHA256, &inner.buffer[..bytes])
            .as_ref()
            .try_into()
            .unwrap();
        let message = Message::Segment {
            offset: inner.position,
            bytes: bytes as u32,
            sha256,
        };
        let started = inner.started;
        let deadline = inner.configuration.max_elapsed_ms;
        let check = inner.check;
        let active = || {
            check()?;
            if started.elapsed().as_millis() > u128::from(deadline) {
                return Err("JPEG XL streaming deadline exceeded".into());
            }
            Ok(())
        };
        inner.state.send(
            Direction::Child,
            message,
            inner.output,
            &mut Cursor::new(&inner.buffer[..bytes]),
            &mut inner.scratch,
            &active,
        )?;
        inner.ack()?;
        inner.position = inner.position.checked_add(bytes as u64).ok_or(ERROR)?;
        inner.highest = inner.highest.max(inner.position);
        inner.records = inner.records.checked_add(1).ok_or(ERROR)?;
        Ok(())
    });
}
extern "C-unwind" fn seek_output(opaque: *mut c_void, position: u64) {
    callback(opaque, (), false, |inner, _| {
        if inner.output_active {
            return Err(ERROR.into());
        }
        inner.event(Message::Seek { position })?;
        inner.position = position;
        Ok(())
    });
}
extern "C-unwind" fn finalize_output(opaque: *mut c_void, position: u64) {
    callback(opaque, (), false, |inner, _| {
        if inner.output_active {
            return Err(ERROR.into());
        }
        inner.event(Message::Finalized { position })
    });
}

/// The caller's actual executable must expose this only as the isolated child
/// entrypoint. `expected_build` is computed independently from its own build.
/// Parent-held resident admissions cover profile+128MiB held input+512MiB native
/// allocator+fixed callback buffers, alongside the existing process guard.
pub(super) fn run(
    input: &mut dyn Read,
    output: &mut dyn Write,
    expected_build: wire::Digest,
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    let started = Instant::now();
    check()?;
    let first = Frame::read(input, check)?;
    let Message::Start(configuration) = first.message() else {
        return Err(ERROR.into());
    };
    configuration.validate()?;
    if configuration.build_sha256 != expected_build {
        return Err(ERROR.into());
    }
    // Parent sends Start only after attaching its process guard. Blocking for
    // the first frame therefore avoids racing job assignment at child startup.
    super::process_limits::streaming_worker(configuration.max_elapsed_ms.div_ceil(1000))?;
    native::version()?;
    let mut state = State::new(configuration, first.nonce())?;
    let mut profile = Vec::new();
    profile
        .try_reserve_exact(configuration.profile_bytes as usize)
        .map_err(|_| ERROR)?;
    profile.resize(configuration.profile_bytes as usize, 0);
    let mut offset = 0usize;
    let mut scratch = [0; wire::BUFFER_BYTES];
    state.complete(
        Direction::Parent,
        first,
        input,
        &mut scratch,
        check,
        &mut |bytes| {
            let end = offset
                .checked_add(bytes.len())
                .filter(|n| *n <= profile.len())
                .ok_or(ERROR)?;
            profile[offset..end].copy_from_slice(bytes);
            offset = end;
            Ok(())
        },
    )?;
    if offset != profile.len() {
        return Err(ERROR.into());
    }
    crate::color_profile::supported(&profile)?;
    let callbacks = Box::new(Callbacks {
        owner: thread::current().id(),
        busy: AtomicBool::new(false),
        failed: AtomicBool::new(false),
        inner: UnsafeCell::new(Inner {
            configuration,
            state,
            input,
            output,
            check,
            started,
            scratch,
            buffer: [0; wire::BUFFER_BYTES],
            output_active: false,
            position: 0,
            highest: 0,
            records: 0,
            last_id: 0,
            format_called: false,
            extra_format_called: false,
            held: std::array::from_fn(|_| None),
            held_bytes: 0,
            error: None,
        }),
    });
    let opaque = ptr::from_ref(callbacks.as_ref())
        .cast_mut()
        .cast::<c_void>();
    let encoder = native::Encoder::new()?;
    let outcome = (|| -> Result<()> {
        unsafe {
            let raw = encoder.raw();
            let mut info = MaybeUninit::<JxlBasicInfo>::uninit();
            JxlEncoderInitBasicInfo(info.as_mut_ptr());
            let mut info = info.assume_init();
            info.xsize = configuration.info.width;
            info.ysize = configuration.info.height;
            info.bits_per_sample = u32::from(configuration.info.bit_depth);
            info.num_color_channels = 3;
            info.num_extra_channels = 1;
            info.alpha_bits = u32::from(configuration.info.bit_depth);
            info.alpha_premultiplied = JxlBool::False;
            info.uses_original_profile = if configuration.options.lossless {
                JxlBool::True
            } else {
                JxlBool::False
            };
            native::enc(JxlEncoderSetBasicInfo(raw, &info))?;
            native::enc(JxlEncoderSetICCProfile(
                raw,
                profile.as_ptr(),
                profile.len(),
            ))?;
            let settings = JxlEncoderFrameSettingsCreate(raw, ptr::null());
            if settings.is_null() {
                return Err(ERROR.into());
            }
            native::enc(JxlEncoderSetFrameLossless(
                settings,
                if configuration.options.lossless {
                    JxlBool::True
                } else {
                    JxlBool::False
                },
            ))?;
            if !configuration.options.lossless {
                native::enc(JxlEncoderSetFrameDistance(
                    settings,
                    configuration.options.distance,
                ))?;
            }
            native::enc(JxlEncoderSetExtraChannelDistance(settings, 0, 0.))?;
            for (key, value) in [
                (JxlEncoderFrameSettingId::KeepInvisible, 1),
                (
                    JxlEncoderFrameSettingId::Effort,
                    i64::from(configuration.options.effort),
                ),
                (JxlEncoderFrameSettingId::Buffering, 2),
                (JxlEncoderFrameSettingId::OutputMode, 1),
            ] {
                native::enc(JxlEncoderFrameSettingsSetOption(settings, key, value))?;
            }
            let output_view = OutputView {
                opaque,
                get_buffer: get_output,
                release_buffer: release_output,
                seek: Some(seek_output),
                set_finalized_position: finalize_output,
            };
            native::enc(JxlEncoderSetOutputProcessor(
                raw,
                std::mem::transmute::<OutputView, JxlEncoderOutputProcessor>(output_view),
            ))?;
            let inner = &mut *callbacks.inner.get();
            inner.event(Message::Ready)?;
            let input_view = InputView {
                opaque,
                get_format,
                get_data,
                get_extra_format: extra_format,
                get_extra_data: extra_data,
                release: release_input,
            };
            native::enc(JxlEncoderAddChunkedFrame(
                settings,
                JxlBool::True,
                std::mem::transmute::<InputView, JxlChunkedFrameInputSource>(input_view),
            ))?;
            JxlEncoderCloseInput(raw);
            native::enc(JxlEncoderFlushInput(raw))?;
        }
        if callbacks.failed.load(Ordering::SeqCst) {
            return Err(ERROR.into());
        }
        Ok(())
    })();
    // Native references are gone before any held-buffer cleanup/context drop.
    if outcome.is_err() {
        callbacks.failed.store(true, Ordering::SeqCst);
    }
    drop(encoder);
    let inner = unsafe { &mut *callbacks.inner.get() };
    outcome.map_err(|error| inner.error.take().unwrap_or(error))?;
    if callbacks.failed.load(Ordering::SeqCst)
        || inner.output_active
        || inner.held_bytes != 0
        || inner.held.iter().any(Option::is_some)
    {
        return Err(inner.error.take().unwrap_or_else(|| ERROR.into()));
    }
    inner.active()?;
    inner.send(Message::Done {
        bytes: inner.highest,
        records: inner.records,
    })?;
    wire::exact_eof(inner.input, check)?;
    if !inner.state.successful() {
        return Err(ERROR.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> Configuration {
        Configuration {
            info: super::super::Info {
                width: 8,
                height: 8,
                bit_depth: 16,
            },
            options: super::super::Options::default(),
            profile_bytes: 132,
            raw_bytes: 512,
            max_input_bytes: 8 * 512 + wire::MAX_RECT_BYTES,
            max_output_bytes: 65536,
            max_log_bytes: 65536,
            max_records: 16,
            max_held_bytes: 1024,
            max_held_buffers: 2,
            max_elapsed_ms: 30000,
            source_binding: [1; 32],
            output_binding: [2; 32],
            raw_sha256: [3; 32],
            profile_sha256: [4; 32],
            build_sha256: [5; 32],
        }
    }
    fn context<'a>(
        input: &'a mut dyn Read,
        output: &'a mut dyn Write,
        check: &'a dyn Fn() -> Result<()>,
    ) -> Box<Callbacks<'a>> {
        let c = config();
        Box::new(Callbacks {
            owner: thread::current().id(),
            busy: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            inner: UnsafeCell::new(Inner {
                configuration: c,
                state: State::new(c, [6; 16]).unwrap(),
                input,
                output,
                check,
                started: Instant::now(),
                scratch: [0; wire::BUFFER_BYTES],
                buffer: [0; wire::BUFFER_BYTES],
                output_active: false,
                position: 0,
                highest: 0,
                records: 0,
                last_id: 0,
                format_called: false,
                extra_format_called: false,
                held: std::array::from_fn(|_| None),
                held_bytes: 0,
                error: None,
            }),
        })
    }
    #[test]
    fn failed_callback_cleanup_drains_only_owned_aligned_buffers_without_transport() {
        let mut input = io::empty();
        let mut output = vec![];
        let callbacks = context(&mut input, &mut output, &|| Err("canceled owner".into()));
        let opaque = ptr::from_ref(callbacks.as_ref()).cast_mut().cast();
        let inner = unsafe { &mut *callbacks.inner.get() };
        let held = Held {
            ids: [1, 0, 0, 0, 0, 0, 0, 0],
            id_count: 1,
            words: vec![0u64; 64],
            bytes: 512,
        };
        let pointer = held.pointer();
        assert_eq!(pointer as usize % 8, 0);
        inner.held_bytes = held.allocated();
        inner.held[0] = Some(held);
        let other = Held {
            ids: [2, 0, 0, 0, 0, 0, 0, 0],
            id_count: 1,
            words: vec![0u64; 32],
            bytes: 256,
        };
        let other_pointer = other.pointer();
        inner.held_bytes += other.allocated();
        inner.held[1] = Some(other);
        callbacks.failed.store(true, Ordering::SeqCst);
        release_input(opaque, unsafe { pointer.cast::<u8>().add(8) }.cast());
        assert_eq!(unsafe { &*callbacks.inner.get() }.held_bytes, 768);
        release_input(opaque, other_pointer);
        release_input(opaque, pointer);
        let inner = unsafe { &*callbacks.inner.get() };
        assert_eq!(inner.held_bytes, 0);
        assert!(inner.held.iter().all(Option::is_none));
        release_input(opaque, pointer);
        assert!(callbacks.failed.load(Ordering::SeqCst));
        drop(callbacks);
        assert!(output.is_empty());
    }
    #[test]
    fn output_reentry_wrong_thread_and_cancel_latch_before_any_late_native_write() {
        for mode in 0..4 {
            let mut input = io::empty();
            let mut output = vec![];
            let active = || {
                if mode == 3 {
                    Err("explicit callback cancellation".into())
                } else {
                    Ok(())
                }
            };
            let mut callbacks = context(&mut input, &mut output, &active);
            if mode == 0 {
                callbacks.busy.store(true, Ordering::SeqCst);
            }
            if mode == 1 {
                callbacks.owner = thread::spawn(|| thread::current().id()).join().unwrap();
            }
            if mode == 2 {
                callbacks.failed.store(true, Ordering::SeqCst);
            }
            let opaque = ptr::from_ref(callbacks.as_ref()).cast_mut().cast();
            let mut size = 123;
            assert!(get_output(opaque, &mut size).is_null());
            assert_eq!(size, 0);
            assert!(callbacks.failed.load(Ordering::SeqCst));
            if mode == 0 {
                callbacks.busy.store(false, Ordering::SeqCst);
            }
            let inner = unsafe { &mut *callbacks.inner.get() };
            inner.output_active = true;
            release_output(opaque, 1);
            if mode != 1 {
                assert!(!unsafe { &*callbacks.inner.get() }.output_active);
            }
            drop(callbacks);
            assert!(output.is_empty());
        }
    }
    #[test]
    fn repeated_pinned_native_format_queries_remain_identical_big_endian() {
        let mut input = io::empty();
        let mut output = vec![];
        let callbacks = context(&mut input, &mut output, &|| Ok(()));
        let opaque = ptr::from_ref(callbacks.as_ref()).cast_mut().cast();
        for _ in 0..3 {
            let mut format = native::format(8);
            get_format(opaque, &mut format);
            assert_eq!(format.num_channels, 3);
            assert_eq!(format.data_type, JxlDataType::Uint16);
            assert_eq!(format.endianness, JxlEndianness::Big);
            extra_format(opaque, 0, &mut format);
            assert_eq!(format.num_channels, 1);
            assert_eq!(format.endianness, JxlEndianness::Big);
        }
        assert!(!callbacks.failed.load(Ordering::SeqCst));
        drop(callbacks);
        assert!(output.is_empty());
    }
    #[test]
    fn callback_envelope_and_unqualified_whole_16000_request_refuse_before_io_or_allocation() {
        let mut input = io::empty();
        let mut output = vec![];
        let callbacks = context(&mut input, &mut output, &|| Ok(()));
        let inner = unsafe { &mut *callbacks.inner.get() };
        let mut stride = 0;
        // The fixed input/output transfer buffers count inside callback quota.
        assert!(inner.get_rectangle(0, 0, 8, 8, &mut stride, false).is_err());
        inner.configuration.info = super::super::Info {
            width: 16000,
            height: 16000,
            bit_depth: 16,
        };
        inner.configuration.raw_bytes = 2_048_000_000;
        inner.configuration.max_held_bytes = wire::MAX_HELD_BYTES;
        inner.state = State::new(inner.configuration, [6; 16]).unwrap();
        // Offset/envelope proof only: no dense buffer, source file or decoder.
        assert!(inner
            .get_rectangle(0, 0, 16000, 16000, &mut stride, false)
            .is_err());
        assert!(inner
            .get_rectangle(0, 0, 2065, 8, &mut stride, false)
            .is_err());
        assert!(inner
            .get_rectangle(usize::MAX, 0, 8, 8, &mut stride, true)
            .is_err());
        assert_eq!(inner.held_bytes, 0);
        assert!(inner.held.iter().all(Option::is_none));
        drop(callbacks);
        assert!(output.is_empty());
    }
}
