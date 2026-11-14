//! Native PNG transport with a display TIFF for pasteboard clients that require it.
use super::{png_digest, Image, ImageIdentity};
use objc2::{
    class, msg_send,
    rc::{autoreleasepool, Retained},
    runtime::{AnyObject, Bool},
};
use objc2_foundation::{NSArray, NSData, NSString};

pub(super) fn read_files() -> Result<Vec<std::path::PathBuf>, String> {
    autoreleasepool(|_| {
        let board = board()?;
        let items: Option<Retained<NSArray<AnyObject>>> =
            unsafe { msg_send![&*board, pasteboardItems] };
        let Some(items) = items else {
            return Ok(Vec::new());
        };
        if items.len() > 4096 {
            return Err("macOS clipboard contains too many items".into());
        }
        let kind = NSString::from_str("public.file-url");
        let mut paths = Vec::new();
        for item in items.iter() {
            let types: Retained<NSArray<NSString>> = unsafe { msg_send![&*item, types] };
            let offered: Bool = unsafe { msg_send![&*types, containsObject: &*kind] };
            if !offered.as_bool() {
                continue;
            }
            if paths.len() == super::MAX_PASTE_FILES {
                return Err("Paste up to 16 image files at a time".into());
            }
            let url: Option<Retained<NSString>> =
                unsafe { msg_send![&*item, stringForType: &*kind] };
            let url = url
                .filter(|url| url.len() <= super::paste::MAX_FILE_URI)
                .ok_or("Cannot read the macOS clipboard file URI")?;
            paths.push(super::paste::local_file_uri(&url.to_string())?);
        }
        Ok(paths)
    })
}

fn board() -> Result<Retained<AnyObject>, String> {
    let value: Option<Retained<AnyObject>> =
        unsafe { msg_send![class!(NSPasteboard), generalPasteboard] };
    value.ok_or_else(|| "macOS pasteboard is unavailable".into())
}

fn count(board: &AnyObject) -> u64 {
    let count: isize = unsafe { msg_send![board, changeCount] };
    count as u64
}

pub(super) fn sequence() -> Option<u64> {
    autoreleasepool(|_| board().ok().map(|board| count(&board)))
}

pub(super) fn write_image(
    image: &Image,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<ImageIdentity, String> {
    autoreleasepool(|_| {
        let board = board()?;
        write_to(&board, image, check)
    })
}

fn write_to(
    board: &AnyObject,
    image: &Image,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<ImageIdentity, String> {
    let png = image.png_checked(check)?;
    let digest = png_digest(&png);
    let tiff = image.display_tiff()?;
    let token = uuid::Uuid::new_v4();
    let item: Retained<AnyObject> = unsafe { msg_send![class!(NSPasteboardItem), new] };
    let identity = NSString::from_str("com.peerbrush.clipboard-image-identity");
    for (kind, bytes) in [
        (NSString::from_str("public.png"), png.as_slice()),
        (NSString::from_str("public.tiff"), tiff.as_slice()),
        (identity.clone(), token.as_bytes().as_slice()),
    ] {
        let data = NSData::with_bytes(bytes);
        let accepted: Bool = unsafe { msg_send![&*item, setData: &*data, forType: &*kind] };
        if !accepted.as_bool() {
            return Err("Cannot prepare macOS clipboard image".into());
        }
    }
    let objects = NSArray::from_retained_slice(&[item]);
    check()?;
    let _: isize = unsafe { msg_send![board, clearContents] };
    let written: Bool = unsafe { msg_send![board, writeObjects: &*objects] };
    if !written.as_bool() {
        return Err("Cannot write macOS clipboard image".into());
    }
    let sequence = count(board);
    let marker: Option<Retained<NSData>> = unsafe { msg_send![board, dataForType: &*identity] };
    if !marker
        .is_some_and(|marker| marker.len() == 16 && marker.to_vec().as_slice() == token.as_bytes())
        || count(board) != sequence
    {
        return Err("macOS clipboard changed during image copy".into());
    }
    Ok(ImageIdentity::Png { digest, generation: Some(sequence) })
}

pub(super) fn image() -> Result<Option<Vec<u8>>, String> {
    autoreleasepool(|_| {
        let board = board()?;
        read_from(&board)
    })
}

fn read_from(board: &AnyObject) -> Result<Option<Vec<u8>>, String> {
    let kind = NSString::from_str("public.png");
    let data: Option<Retained<NSData>> = unsafe { msg_send![board, dataForType: &*kind] };
    data.map(|data| {
        if data.len() > super::png_transport::LIMIT {
            return Err("Clipboard PNG exceeds 128 MiB".into());
        }
        Ok(data.to_vec())
    })
    .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PrivateBoard(Retained<AnyObject>);
    impl Drop for PrivateBoard {
        fn drop(&mut self) {
            let _: () = unsafe { msg_send![&*self.0, releaseGlobally] };
        }
    }

    #[test]
    #[ignore = "requires a macOS desktop pasteboard service"]
    fn native_pasteboard_keeps_png16_and_tiff_in_one_item() {
        autoreleasepool(|_| {
            let board: Option<Retained<AnyObject>> =
                unsafe { msg_send![class!(NSPasteboard), pasteboardWithUniqueName] };
            let board = PrivateBoard(board.expect("private macOS pasteboard"));
            let words = vec![12347, 33559, 51237, 27001];
            let bytes = words
                .iter()
                .copied()
                .map(crate::raster::project16)
                .collect::<Vec<_>>();
            let mut image = Image {
                width: 1,
                height: 1,
                bytes: bytes.clone(),
                samples16: Some(words.clone()),
                origin: None,
            };
            let identity = write_to(&board.0, &image, &|| Ok(())).unwrap();
            let png = read_from(&board.0).unwrap().unwrap();
            assert_eq!(identity, ImageIdentity::Png { digest: png_digest(&png), generation: Some(count(&board.0)) });
            assert_eq!(Image::png_bytes(png).unwrap().samples16, Some(words));
            let kind = NSString::from_str("public.tiff");
            let tiff: Retained<NSData> = unsafe { msg_send![&*board.0, dataForType: &*kind] };
            assert_eq!(
                image::load_from_memory_with_format(&tiff.to_vec(), image::ImageFormat::Tiff)
                    .unwrap()
                    .into_rgba8()
                    .into_raw(),
                bytes
            );
            image.samples16 = None;
            assert_ne!(write_to(&board.0, &image, &|| Ok(())).unwrap(), identity);
            assert!(Image::png_bytes(read_from(&board.0).unwrap().unwrap()).unwrap().samples16.is_none());
        });
    }
}
