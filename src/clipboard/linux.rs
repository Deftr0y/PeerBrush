//! Keep the bytes offered as image/png intact on both Linux clipboard backends.
use super::{png_digest, ClipboardImage, Image, ImageIdentity};
use arboard::{GetExtLinux, SetExtLinux};

pub(super) fn write_image(
    clipboard: &mut arboard::Clipboard,
    image: &Image,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<ImageIdentity, String> {
    let bytes = image.identified_png(check)?;
    let identity = ImageIdentity::Png { digest: png_digest(&bytes), generation: None };
    check()?;
    clipboard
        .set()
        .png(bytes)
        .map_err(|error| error.to_string())?;
    Ok(identity)
}

pub(super) fn read_image(
    clipboard: &mut arboard::Clipboard,
) -> Result<(ClipboardImage, Option<ImageIdentity>), String> {
    let bytes = clipboard
        .get()
        .png(super::png_transport::LIMIT)
        .map_err(|error| error.to_string())?;
    Ok((ClipboardImage::Png(bytes.into()), None))
}

pub(super) fn read_files(
    clipboard: &mut arboard::Clipboard,
) -> Result<Vec<std::path::PathBuf>, String> {
    match clipboard.get().file_uris() {
        Ok(offer) => super::paste::local_file_uris(&offer),
        Err(arboard::Error::ContentNotAvailable) => Ok(Vec::new()),
        Err(error) => Err(format!("Cannot read clipboard files: {error}")),
    }
}
