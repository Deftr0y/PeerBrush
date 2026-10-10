//! Bounded native PNG and legacy DIB transport. Editing stays in the shared engine.
use super::*;

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
thread_local! { static OWNER: Owner = Owner::new(); }
pub(super) fn pump() {
    OWNER.with(|owner| {
        if owner.0.is_null() {
            return;
        }
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
        let _clipboard = owner.open()?;
        clipboard_win::raw::set_string(text)
            .map_err(|e| format!("Cannot write clipboard text: {e}"))
    })
}
pub(super) fn write_image(image: &Image) -> Result<(), String> {
    check_size(image.width, image.height)?;
    if image.bytes.len() != image.width as usize * image.height as usize * 4 {
        return Err("Wrong clipboard image size".into());
    }
    // Encode before opening/clearing, and retain native16 in the registered PNG.
    let png = if let Some(words) = &image.samples16 {
        crate::raster::png16(image.width, image.height, words)?
    } else {
        crate::raster::png(image.width, image.height, &image.bytes)?
    };
    let format = clipboard_win::register_format("PNG")
        .ok_or("Cannot register the Windows PNG clipboard format")?;
    let dib = bitmap(image);
    OWNER.with(|owner| {
        let _clipboard = owner.open()?;
        clipboard_win::raw::empty()
            .map_err(|e| format!("Cannot clear clipboard for image copy: {e}"))?;
        clipboard_win::raw::set_without_clear(format.get(), &png)
            .map_err(|e| format!("Cannot write clipboard PNG: {e}"))?;
        clipboard_win::raw::set_without_clear(clipboard_win::formats::CF_DIBV5, &dib)
            .map_err(|e| format!("Cannot write clipboard bitmap: {e}"))
    })
}
fn bitmap(image: &Image) -> Vec<u8> {
    let mut dib = vec![0; 124];
    let put = |bytes: &mut [u8], offset: usize, value: u32| {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes())
    };
    put(&mut dib, 0, 124);
    put(&mut dib, 4, image.width);
    put(&mut dib, 8, (-(image.height as i32)) as u32); // Top-down BGRA.
    dib[12..14].copy_from_slice(&1u16.to_le_bytes());
    dib[14..16].copy_from_slice(&32u16.to_le_bytes());
    put(&mut dib, 16, 3); // BI_BITFIELDS, explicit RGBA masks.
    put(&mut dib, 20, image.bytes.len() as u32);
    for (offset, value) in [
        (40, 0x00ff0000),
        (44, 0x0000ff00),
        (48, 0x000000ff),
        (52, 0xff000000),
        (56, 0x73524742),
    ] {
        put(&mut dib, offset, value);
    }
    for pixel in image.bytes.chunks_exact(4) {
        dib.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
    }
    dib
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let dib = bitmap(&image);
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
}

fn read(format: u32) -> Result<Option<Vec<u8>>, String> {
    let _clipboard = clipboard_win::Clipboard::new_attempts(10)
        .map_err(|_| "Windows clipboard is busy; retry after the other operation finishes")?;
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
pub(super) fn png() -> Result<Option<Image>, String> {
    let format = clipboard_win::register_format("PNG")
        .ok_or("Cannot access the Windows PNG clipboard format")?;
    read(format.get())?.map(Image::png_bytes).transpose()
}
pub(super) fn dib() -> Result<Option<Image>, String> {
    let Some(bytes) = read(clipboard_win::formats::CF_DIB)? else {
        return Ok(None);
    };
    let decoder =
        image::codecs::bmp::BmpDecoder::new_without_file_header(std::io::Cursor::new(bytes))
            .map_err(|e| format!("Cannot decode clipboard bitmap: {e}"))?;
    Image::decoder(decoder, 256 * 1024 * 1024).map(Some)
}
