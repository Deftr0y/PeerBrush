//! Bounded native PNG and legacy DIB transport. Editing stays in the shared engine.
use super::*;

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
