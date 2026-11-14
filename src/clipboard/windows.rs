//! Bounded native PNG and validated DIB transport. Editing stays in the shared engine.
use super::*;
use crate::{large_document::ResidentBudget, psd::memory36::MetadataCharge};
use std::sync::Arc;

// Windows writes need a non-null owner. Each clipboard worker retains its own
// message-only window on that thread; all data is rendered eagerly.
type Handle = *mut std::ffi::c_void;
#[repr(C)]
struct Message {
    window: Handle,
    kind: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    point: [i32; 2],
    private: u32,
}
#[link(name = "user32")]
extern "system" {
    fn CreateWindowExW(
        ex: u32,
        class: *const u16,
        name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: Handle,
        menu: Handle,
        instance: Handle,
        parameter: Handle,
    ) -> Handle;
    fn DestroyWindow(window: Handle) -> i32;
    fn PeekMessageW(message: *mut Message, window: Handle, min: u32, max: u32, remove: u32) -> i32;
    fn TranslateMessage(message: *const Message) -> i32;
    fn DispatchMessageW(message: *const Message) -> isize;
    fn SetClipboardData(format: u32, memory: Handle) -> Handle;
}
#[link(name = "kernel32")]
extern "system" {
    fn GlobalAlloc(flags: u32, size: usize) -> Handle;
    fn GlobalFree(memory: Handle) -> Handle;
    fn GlobalLock(memory: Handle) -> Handle;
    fn GlobalUnlock(memory: Handle) -> i32;
    fn GlobalSize(memory: Handle) -> usize;
}

struct ClipboardMemory {
    handle: Option<std::ptr::NonNull<std::ffi::c_void>>,
    charge: Option<Arc<MetadataCharge>>,
}
impl ClipboardMemory {
    fn prepare(
        size: usize,
        budget: Option<&ResidentBudget>,
        check: &dyn Fn() -> Result<(), String>,
        fill: impl FnOnce(&mut [u8]) -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        if size == 0 || size > isize::MAX as usize {
            return Err("Invalid Windows clipboard allocation size".into());
        }
        // GlobalSize may include slack. Admit it before allocating and check
        // the actual block before locking or copying any bytes.
        let charge = budget.map(|budget| MetadataCharge::reserve(budget, size + 64 * 1024, check))
            .transpose()?;
        // GHND combines movable storage (required for transfer) and zero initialization.
        const GHND: u32 = 0x0042;
        let handle = unsafe { GlobalAlloc(GHND, size) };
        let handle = std::ptr::NonNull::new(handle).ok_or_else(|| {
            format!("Cannot allocate Windows clipboard data: {}", std::io::Error::last_os_error())
        })?;
        let mut memory = Self { handle: Some(handle), charge };
        let allocated = memory.size();
        if allocated < size || memory.charge.as_ref().is_some_and(|charge| allocated > charge.bytes()) {
            return Err("Windows clipboard allocation differs from its admitted size".into());
        }
        let (lock, target) = memory.lock()?;
        // The fresh block is zero-initialized, uniquely owned, and stays locked
        // for the lifetime of the slice passed to fill.
        let result = fill(unsafe { std::slice::from_raw_parts_mut(target.as_ptr(), size) });
        let unlocked = lock.unlock();
        result?;
        unlocked?;
        Ok(memory)
    }
    fn handle(&self) -> Handle {
        self.handle.unwrap().as_ptr()
    }
    fn lock(&mut self) -> Result<(ClipboardLock<'_>, std::ptr::NonNull<u8>), String> {
        let target = unsafe { GlobalLock(self.handle()) }.cast::<u8>();
        let target = std::ptr::NonNull::new(target).ok_or_else(|| {
            format!("Cannot lock Windows clipboard data: {}", std::io::Error::last_os_error())
        })?;
        Ok((ClipboardLock { memory: self }, target))
    }
    fn size(&self) -> usize {
        unsafe { GlobalSize(self.handle()) }
    }
    fn publish(mut self, format: u32) -> Result<(), std::io::Error> {
        if unsafe { SetClipboardData(format, self.handle()) }.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        // Windows owns a successfully published handle.
        self.handle = None;
        Ok(())
    }
}
impl Drop for ClipboardMemory {
    fn drop(&mut self) {
        if let Some(handle) = self.handle {
            unsafe { GlobalFree(handle.as_ptr()) };
        }
    }
}
struct ClipboardLock<'a> {
    memory: &'a ClipboardMemory,
}
impl ClipboardLock<'_> {
    fn unlock(self) -> Result<(), String> {
        let handle = self.memory.handle();
        std::mem::forget(self);
        let still_locked = unsafe { GlobalUnlock(handle) };
        let status = std::io::Error::last_os_error();
        if still_locked != 0 || status.raw_os_error() != Some(0) {
            return Err("Cannot unlock prepared Windows clipboard data".into());
        }
        Ok(())
    }
}
impl Drop for ClipboardLock<'_> {
    fn drop(&mut self) {
        unsafe { GlobalUnlock(self.memory.handle()) };
    }
}
struct Owner(Handle);
impl Owner {
    fn new() -> Self {
        let class = [83u16, 84, 65, 84, 73, 67, 0]; // Built-in STATIC class.
        Self(unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                0,
                -3isize as Handle,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        })
    }
    fn open(&self) -> Result<clipboard_win::Clipboard, String> {
        if self.0.is_null() {
            return Err("Cannot create the Windows clipboard owner".into());
        }
        for attempt in 0..10 {
            if let Ok(clipboard) = clipboard_win::Clipboard::new_for(self.0) {
                return Ok(clipboard);
            }
            if attempt < 9 {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        Err("Windows clipboard is busy; retry after the other operation finishes".into())
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                DestroyWindow(self.0);
            }
        }
    }
}
thread_local! { static OWNER: std::cell::OnceCell<Owner> = const { std::cell::OnceCell::new() }; }
pub(super) fn pump() {
    OWNER.with(|owner| {
        let Some(owner) = owner.get().filter(|owner| !owner.0.is_null()) else {
            return;
        };
        let mut message = std::mem::MaybeUninit::<Message>::uninit();
        unsafe {
            while PeekMessageW(message.as_mut_ptr(), owner.0, 0, 0, 1) != 0 {
                TranslateMessage(message.as_ptr());
                DispatchMessageW(message.as_ptr());
            }
        }
    });
}

pub(super) fn write_text(text: &str) -> Result<(), String> {
    OWNER.with(|owner| {
        let _clipboard = owner.get_or_init(Owner::new).open()?;
        clipboard_win::raw::set_string(text)
            .map_err(|e| format!("Cannot write clipboard text: {e}"))
    })
}
pub(super) fn write_image(
    image: &Image,
    budget: Option<&ResidentBudget>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Option<ImageIdentity>, String> {
    check()?;
    let format = clipboard_win::register_format("PNG")
        .ok_or("Cannot register the Windows PNG clipboard format")?;
    let PreparedImage { png, dib, png_size, digest } = prepare_image(image, budget, check)?;
    check()?;
    let generation = OWNER.with(|owner| -> Result<_, String> {
        let _clipboard = owner.get_or_init(Owner::new).open()?;
        check()?;
        clipboard_win::raw::empty()
            .map_err(|e| format!("Cannot clear clipboard for image copy: {e}"))?;
        png.publish(format.get())
            .map_err(|e| format!("Cannot write clipboard PNG: {e}"))?;
        dib.publish(clipboard_win::formats::CF_DIBV5)
            .map_err(|e| format!("Cannot write clipboard bitmap: {e}"))?;
        if clipboard_win::size(format.get()).map(|value| value.get()) != Some(png_size) {
            return Err("Cannot verify the written Windows clipboard PNG".into());
        }
        Ok(clipboard_win::seq_num().map(|sequence| sequence.get() as u64))
    })?;
    Ok(generation.map(|generation| ImageIdentity::Png { digest, generation: Some(generation) }))
}

struct PreparedImage {
    png: ClipboardMemory,
    dib: ClipboardMemory,
    png_size: usize,
    digest: [u8; 32],
}
fn prepare_image(
    image: &Image,
    budget: Option<&ResidentBudget>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<PreparedImage, String> {
    let encoded = super::png_transport::encode_admitted(
        image, None, super::png_transport::LIMIT, budget, check,
    )?;
    let png = ClipboardMemory::prepare(encoded.bytes().len(), budget, check, |target| {
        target.copy_from_slice(encoded.bytes());
        Ok(())
    })?;
    let png_size = png.size();
    let digest = owned_png_digest(encoded.bytes(), png_size)?;
    drop(encoded);
    let dib = bitmap(image, budget, check)?;
    Ok(PreparedImage { png, dib, png_size, digest })
}

fn owned_png_digest(png: &[u8], allocated: usize) -> Result<[u8; 32], String> {
    let mut padding = allocated.checked_sub(png.len())
        .filter(|_| allocated <= super::png_transport::LIMIT)
        .ok_or("Written Windows clipboard PNG exceeds its transport bound")?;
    // GlobalSize can include allocator padding. GHND initializes it to zero.
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    digest.update(png);
    let zeros = [0; 4096];
    while padding > 0 {
        let size = padding.min(zeros.len());
        digest.update(&zeros[..size]);
        padding -= size;
    }
    Ok(digest.finish().as_ref().try_into().unwrap())
}
const DIB_HEADER: usize = 124;
fn bitmap(
    image: &Image,
    budget: Option<&ResidentBudget>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<ClipboardMemory, String> {
    check()?;
    image.validate()?;
    ClipboardMemory::prepare(DIB_HEADER + image.bytes.len(), budget, check, |dib| {
        let (header, pixels) = dib.split_at_mut(DIB_HEADER);
        let mut put = |offset: usize, value: u32| {
            header[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        };
        put(0, DIB_HEADER as u32);
        put(4, image.width);
        put(8, (-(image.height as i32)) as u32); // Top-down BGRA.
        put(12, (32 << 16) | 1); // One plane, 32 bits per pixel.
        put(16, 3); // BI_BITFIELDS, explicit RGBA masks.
        put(20, image.bytes.len() as u32);
        for (offset, value) in [
            (40, 0x00ff0000),
            (44, 0x0000ff00),
            (48, 0x000000ff),
            (52, 0xff000000),
            (56, 0x73524742),
        ] {
            put(offset, value);
        }
        let row_bytes = image.width as usize * 4;
        for (row, output) in image.bytes.chunks_exact(row_bytes).zip(pixels.chunks_exact_mut(row_bytes)) {
            check()?;
            for (pixel, target) in row.chunks_exact(4).zip(output.chunks_exact_mut(4)) {
                target.copy_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
            }
        }
        check()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_memory(mut memory: ClipboardMemory, length: usize) -> Vec<u8> {
        assert!(memory.size() >= length);
        let (lock, address) = memory.lock().unwrap();
        let bytes = unsafe { std::slice::from_raw_parts(address.as_ptr(), length) }.to_vec();
        lock.unlock().unwrap();
        bytes
    }

    #[test]
    fn native_windows_preparation_admits_live_buffers_and_releases_every_charge() {
        let (width, height) = (16000, 64);
        let channels = width as usize * height as usize * 4;
        let image = Image {
            width, height, bytes: vec![0; channels],
            samples16: Some(vec![0; channels]), origin: None,
        };
        let active = || Ok(());
        let budget = ResidentBudget::new(32 * 1024 * 1024).unwrap();
        let _pixels = MetadataCharge::reserve(&budget, 3 * channels, &active).unwrap();
        let baseline = budget.used();
        assert!(baseline + 16 * 1024 * 1024 + 20 * channels > budget.limit());
        let prepared = prepare_image(&image, Some(&budget), &active).unwrap();
        let png_charge = prepared.png.charge.as_ref().unwrap().bytes();
        let dib_charge = prepared.dib.charge.as_ref().unwrap().bytes();
        assert_eq!(budget.used(), baseline + png_charge + dib_charge);
        let allocation = read_memory(prepared.png, prepared.png_size);
        assert_eq!(png_digest(&allocation), prepared.digest);
        let decoded = png_offer(allocation).unwrap();
        assert_eq!(crate::image_import::prepared_png::clipboard_dimensions(
            decoded.payload(), &mut || Ok(()),
        ).unwrap(), [width, height]);
        drop(prepared.dib);
        assert_eq!(budget.used(), baseline);

        let _held = budget.reserve_bytes(5 * 1024 * 1024, 5 * 1024 * 1024).unwrap();
        let used = budget.used();
        assert!(prepare_image(&image, Some(&budget), &active).is_err());
        assert_eq!(budget.used(), used);
    }

    #[test]
    fn prepared_global_memory_keeps_the_exact_payload_and_zero_padding() {
        let bytes = [17, 73, 149, 255].repeat(1027);
        let memory = ClipboardMemory::prepare(bytes.len(), None, &|| Ok(()), |target| {
            target.copy_from_slice(&bytes);
            Ok(())
        }).unwrap();
        let size = memory.size();
        let observed = png_digest(&read_memory(memory, size));
        assert_eq!(observed, owned_png_digest(&bytes, size).unwrap());
        assert!(ClipboardMemory::prepare(0, None, &|| Ok(()), |_| panic!("Empty allocation")).is_err());
    }

    #[test]
    fn written_png_digest_matches_the_zero_padded_clipboard_allocation() {
        let png = b"encoded bytes";
        let mut allocated = png.to_vec();
        allocated.resize(png.len() + 9, 0);
        assert_eq!(owned_png_digest(png, allocated.len()).unwrap(), png_digest(&allocated));
        assert!(owned_png_digest(png, png.len() - 1).is_err());
        assert!(owned_png_digest(png, super::super::png_transport::LIMIT + 1).is_err());
    }

    #[test]
    fn windows_png_offers_keep_allocation_bytes_but_decode_only_the_framed_payload() {
        let words = [12347, 33559, 51237, 27001];
        let png = crate::raster::png16(1, 1, &words).unwrap();
        let mut padded = png.clone();
        padded.extend_from_slice(&[0; 13]);
        let offer = png_offer(padded.clone()).unwrap();
        assert_eq!(offer.payload(), png);
        assert_eq!(offer.allocation, padded);
        let size = crate::image_import::prepared_png::clipboard_dimensions(
            offer.payload(), &mut || Ok(()),
        ).unwrap();
        assert_eq!(size, [1, 1]);
        assert_eq!(ClipboardImage::Png(offer).decode().unwrap().samples16.unwrap(), words);
        assert!(Image::png_bytes(padded.clone()).is_err());

        let mut bad_checksum = padded.clone();
        bad_checksum[png.len() - 1] ^= 1;
        let corrupt = png_offer(bad_checksum).unwrap();
        assert!(ClipboardImage::Png(corrupt).decode().is_err());
        let last = padded.len() - 1;
        padded[last] = 1;
        assert!(png_offer(padded).is_err());
        assert!(png_offer(png[..png.len() - 1].to_vec()).is_err());
    }

    #[test]
    fn wide_bitmap_and_png_keep_dimensions_orientation_alpha_and_native_words() {
        for depth in [8, 16] {
            let words = [12347, 33559, 51237, 0, 54321, 10987, 32109, 49153].repeat(16000);
            let image = Image {
                width: 16000, height: 2,
                bytes: words.iter().copied().map(crate::raster::project16).collect(),
                samples16: (depth == 16).then_some(words),
                origin: None,
            };
            let png = Image::png_bytes(image.png().unwrap()).unwrap();
            assert_eq!((png.width, png.height), (16000, 2));
            assert_eq!(png.bytes, image.bytes);
            assert_eq!(png.samples16, image.samples16);
            let dib = bitmap(&image, None, &|| Ok(())).unwrap();
            let dib = decode_dib(read_memory(dib, DIB_HEADER + image.bytes.len())).unwrap();
            assert_eq!((dib.width, dib.height), (16000, 2));
            assert_eq!(dib.bytes, image.bytes);
            assert!(dib.samples16.is_none());
        }
    }

    #[test]
    fn copied_bitmap_is_independently_decodable_with_exact_orientation_color_and_alpha() {
        let image = Image {
            width: 2,
            height: 3,
            bytes: vec![
                12, 34, 56, 255, 78, 90, 123, 128, 44, 55, 66, 0, 33, 22, 11, 1, 210, 180, 150,
                254, 1, 2, 3, 255,
            ],
            samples16: None,
            origin: Some([7, 9]),
        };
        let memory = bitmap(&image, None, &|| Ok(())).unwrap();
        let dib = read_memory(memory, DIB_HEADER + image.bytes.len());
        let checks = std::cell::Cell::new(0);
        let interrupted = bitmap(&image, None, &|| {
            checks.set(checks.get() + 1);
            if checks.get() == 4 { Err("Bitmap preparation cancelled".into()) } else { Ok(()) }
        });
        assert_eq!(interrupted.err().unwrap(), "Bitmap preparation cancelled");
        let clipboard = decode_dib(dib.clone()).unwrap();
        assert_eq!(clipboard.bytes, image.bytes);
        assert_eq!((clipboard.width, clipboard.height), (2, 3));
        // A BMP file header explicitly identifies the pixels after the V5 header;
        // this also avoids the image decoder's legacy no-file-header mask offset.
        let mut file = b"BM".to_vec();
        file.extend_from_slice(&((14 + dib.len()) as u32).to_le_bytes());
        file.extend_from_slice(&[0; 4]);
        file.extend_from_slice(&138u32.to_le_bytes());
        file.extend_from_slice(&dib);
        let decoder = image::codecs::bmp::BmpDecoder::new(std::io::Cursor::new(file)).unwrap();
        let decoded = Image::decoder(decoder, 1024 * 1024).unwrap();
        assert_eq!((decoded.width, decoded.height), (2, 3));
        assert_eq!(decoded.bytes, image.bytes);
    }

    #[test]
    fn dib_reader_locates_core_info_and_v4_v5_pixels_without_dropping_prefix_samples() {
        for (dib, bits, compression, alpha) in [
            (12, 4, 0, false),
            (12, 24, 0, false),
            (40, 8, 0, false),
            (40, 16, 3, false),
            (56, 16, 3, true),
            (108, 32, 3, true),
            (124, 32, 3, true),
        ] {
            let bytes = crate::image_import::bmp::tests::fixture(
                7,
                3,
                dib,
                bits,
                compression,
                false,
                alpha,
            );
            let offset = u32::from_le_bytes(bytes[10..14].try_into().unwrap()) as usize;
            // The BMP fixture has a 13-byte file-only gap before its pixel array.
            let mut packed = bytes[14..offset - 13].to_vec();
            packed.extend_from_slice(&bytes[offset..]);
            let expected = image::load_from_memory_with_format(&bytes, image::ImageFormat::Bmp)
                .unwrap()
                .to_rgba8()
                .into_raw();
            let decoded = decode_dib(packed).unwrap();
            assert_eq!(decoded.bytes, expected);
            assert_eq!((decoded.width, decoded.height), (7, 3));
        }
    }

    #[test]
    fn windows_format_preference_preserves_png16_and_never_bypasses_refused_dib() {
        const PNG: u32 = 0xc001;
        let words = [12347, 33559, 51237, 60001];
        let png = crate::raster::png16(1, 1, &words).unwrap();
        let result = preferred_image(PNG, [clipboard_win::formats::CF_DIBV5], |format| {
            assert_eq!(format, PNG);
            Ok(Some(png.clone()))
        })
        .unwrap()
        .unwrap()
        .into_image()
        .unwrap()
        .decode()
        .unwrap();
        assert_eq!(result.samples16.unwrap(), words);
        let wide = crate::image_import::bmp::tests::wide_channels()[14..].to_vec();
        for refused in [
            clipboard_win::formats::CF_DIBV5,
            clipboard_win::formats::CF_DIB,
        ] {
            let mut requested = Vec::new();
            let other = if refused == clipboard_win::formats::CF_DIBV5 {
                clipboard_win::formats::CF_DIB
            } else {
                clipboard_win::formats::CF_DIBV5
            };
            let reason = preferred_image(PNG, [refused, other], |format| {
                requested.push(format);
                assert_ne!(format, other);
                Ok((format == refused).then(|| wide.clone()))
            })
            .unwrap()
            .unwrap()
            .into_image()
            .err()
            .unwrap();
            assert!(reason.contains("wider than 8 bits"), "{reason}");
            assert_eq!(requested.last(), Some(&refused));
            assert_eq!(requested.len(), 2);
        }
        let mut requested = Vec::new();
        assert!(
            preferred_image(PNG, [clipboard_win::formats::CF_DIBV5], |format| {
                requested.push(format);
                if format == clipboard_win::formats::CF_DIBV5 {
                    Err("clipboard read changed".into())
                } else {
                    Ok(None)
                }
            })
            .err()
            .unwrap()
            .contains("clipboard read changed")
        );
        assert_eq!(requested, [PNG, clipboard_win::formats::CF_DIBV5]);
        assert!(preferred_image(PNG, [], |_| Ok(None)).unwrap().is_none());
        assert!(preferred_image(PNG, [clipboard_win::formats::CF_DIB], |_| Ok(None)).is_err());
    }
}

fn read(format: u32) -> Result<Option<Vec<u8>>, String> {
    if !clipboard_win::is_format_avail(format) {
        return Ok(None);
    }
    let size = clipboard_win::size(format)
        .ok_or("Cannot read the Windows clipboard image")?
        .get();
    if size > 128 * 1024 * 1024 {
        return Err("Clipboard image exceeds 128 MiB".into());
    }
    let mut bytes = vec![0; size];
    let copied = clipboard_win::raw::get(format, &mut bytes)
        .map_err(|e| format!("Cannot read clipboard image: {e}"))?;
    if copied != size {
        return Err("Clipboard image changed during the read; try again".into());
    }
    Ok(Some(bytes))
}
pub(super) fn image() -> Result<Option<ClipboardImage>, String> {
    let format = clipboard_win::register_format("PNG")
        .ok_or("Cannot access the Windows PNG clipboard format")?;
    let encoded = {
        let _clipboard = clipboard_win::Clipboard::new_attempts(10)
            .map_err(|_| "Windows clipboard is busy; retry after the other operation finishes")?;
        let encoded = preferred_image(format.get(), clipboard_win::EnumFormats::new(), read)?;
        if encoded.is_none()
            && [
                clipboard_win::formats::CF_DIBV5,
                clipboard_win::formats::CF_DIB,
            ]
            .into_iter()
            .any(clipboard_win::is_format_avail)
        {
            return Err("Cannot inspect the original Windows bitmap format".into());
        }
        encoded
    };
    // Release the OS clipboard before decoding or allocating output pixels.
    encoded.map(EncodedImage::into_image).transpose()
}
enum EncodedImage {
    Png(Vec<u8>),
    Dib(Vec<u8>),
}
impl EncodedImage {
    fn into_image(self) -> Result<ClipboardImage, String> {
        match self {
            Self::Png(bytes) => png_offer(bytes).map(ClipboardImage::Png),
            Self::Dib(bytes) => decode_dib(bytes).map(ClipboardImage::Bitmap),
        }
    }
}

pub(super) fn png_offer(allocation: Vec<u8>) -> Result<ClipboardPng, String> {
    let mut remaining = allocation.strip_prefix(b"\x89PNG\r\n\x1a\n")
        .ok_or("Invalid Windows clipboard PNG signature")?;
    // Walk the framing to distinguish allocation padding from zero bytes in
    // a chunk. The shared decoder still verifies the complete PNG payload.
    while let Some(header) = remaining.get(..8) {
        let length = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let tail = length.checked_add(12).and_then(|end| remaining.get(end..))
            .ok_or("Truncated Windows clipboard PNG chunk")?;
        if &header[4..] == b"IEND" {
            if length != 0 || tail.iter().any(|&byte| byte != 0) {
                return Err("Invalid Windows clipboard PNG end or allocation padding".into());
            }
            let payload_len = allocation.len() - tail.len();
            return Ok(ClipboardPng { allocation, payload_len });
        }
        remaining = tail;
    }
    Err("Windows clipboard PNG is missing its end chunk".into())
}

fn preferred_image(
    png: u32,
    formats: impl IntoIterator<Item = u32>,
    mut read: impl FnMut(u32) -> Result<Option<Vec<u8>>, String>,
) -> Result<Option<EncodedImage>, String> {
    if let Some(bytes) = read(png)? {
        return Ok(Some(EncodedImage::Png(bytes)));
    }
    // Windows enumerates an original format before its synthesized alternatives.
    for format in formats {
        if matches!(
            format,
            clipboard_win::formats::CF_DIBV5 | clipboard_win::formats::CF_DIB
        ) {
            return read(format)?
                .map(EncodedImage::Dib)
                .map(Some)
                .ok_or_else(|| "Original Windows bitmap data is unavailable".into());
        }
    }
    Ok(None)
}
fn decode_dib(mut bytes: Vec<u8>) -> Result<Image, String> {
    crate::image_import::bmp::validate_dib(&bytes)?;
    let size = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    let bits_at = if size == 12 { 10 } else { 14 };
    let bits = u16::from_le_bytes(bytes[bits_at..bits_at + 2].try_into().unwrap());
    let compression = if size == 12 {
        0
    } else {
        u32::from_le_bytes(bytes[16..20].try_into().unwrap())
    };
    let used = if size == 12 {
        0
    } else {
        u32::from_le_bytes(bytes[32..36].try_into().unwrap()) as usize
    };
    let colors = if used == 0 && matches!(bits, 1 | 2 | 4 | 8) {
        1usize << bits
    } else {
        used
    };
    if colors > 256 {
        return Err("Clipboard bitmap palette exceeds admission".into());
    }
    let offset = size
        + usize::from(size == 40 && compression == 3) * 12
        + colors * if size == 12 { 3 } else { 4 };
    if offset > bytes.len() {
        return Err("Truncated clipboard bitmap metadata".into());
    }
    // Explicit pixels avoid the codec's legacy V4/V5 no-file-header mask offset.
    let original = bytes.len();
    bytes
        .try_reserve_exact(14)
        .map_err(|_| "Clipboard bitmap header allocation refused")?;
    bytes.resize(original + 14, 0);
    bytes.copy_within(..original, 14);
    bytes[..14].fill(0);
    bytes[..2].copy_from_slice(b"BM");
    bytes[2..6].copy_from_slice(&((original + 14) as u32).to_le_bytes());
    bytes[10..14].copy_from_slice(&((offset + 14) as u32).to_le_bytes());
    let decoder = image::codecs::bmp::BmpDecoder::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("Cannot decode clipboard bitmap: {e}"))?;
    Image::decoder(decoder, 256 * 1024 * 1024)
}
