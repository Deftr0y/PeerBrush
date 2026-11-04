use super::{push_native, FontError, NativeFace, FACE_LIMIT};
use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};
use windows::{
    core::Interface,
    Win32::{Foundation::BOOL, Graphics::DirectWrite::*},
};

fn localized(strings: &IDWriteLocalizedStrings) -> Result<String, FontError> {
    unsafe {
        if strings.GetCount() == 0 {
            return Err(FontError::Unavailable);
        }
        let length = strings
            .GetStringLength(0)
            .map_err(|_| FontError::Unavailable)?;
        if length == 0 || length > 512 {
            return Err(FontError::Unavailable);
        }
        let mut value = vec![0u16; length as usize + 1];
        strings
            .GetString(0, &mut value)
            .map_err(|_| FontError::Unavailable)?;
        String::from_utf16(&value[..length as usize]).map_err(|_| FontError::Unavailable)
    }
}
fn location(font: &IDWriteFont) -> Option<(PathBuf, u32)> {
    unsafe {
        let face = font.CreateFontFace().ok()?;
        let mut count = 0;
        face.GetFiles(&mut count, None).ok()?;
        if count != 1 {
            return None;
        }
        let mut file = None;
        face.GetFiles(&mut count, Some(&mut file)).ok()?;
        if count != 1 {
            return None;
        }
        let file = file?;
        // A nonlocal/custom loader is never asked to download or materialize data.
        let loader: IDWriteLocalFontFileLoader = file.GetLoader().ok()?.cast().ok()?;
        let mut key = std::ptr::null_mut();
        let mut key_size = 0;
        file.GetReferenceKey(&mut key, &mut key_size).ok()?;
        if key.is_null() || key_size == 0 || key_size > 65536 {
            return None;
        }
        let length = loader.GetFilePathLengthFromKey(key, key_size).ok()?;
        if length == 0 || length > 32767 {
            return None;
        }
        let mut path = vec![0u16; length as usize + 1];
        loader.GetFilePathFromKey(key, key_size, &mut path).ok()?;
        Some((
            PathBuf::from(OsString::from_wide(&path[..length as usize])),
            face.GetIndex(),
        ))
    }
}
pub(super) fn enumerate() -> Result<Vec<NativeFace>, FontError> {
    unsafe {
        let factory: IDWriteFactory3 =
            DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).map_err(|_| FontError::Unavailable)?;
        let mut collection = None;
        factory
            .GetSystemFontCollection(false, &mut collection, true)
            .map_err(|_| FontError::Unavailable)?;
        let collection: IDWriteFontCollection = collection
            .ok_or(FontError::Unavailable)?
            .cast()
            .map_err(|_| FontError::Unavailable)?;
        let families = collection.GetFontFamilyCount();
        if families as usize > FACE_LIMIT {
            return Err(FontError::CatalogLimit);
        }
        let mut rows = Vec::new();
        let mut bytes = 0;
        for family_index in 0..families {
            let family = collection
                .GetFontFamily(family_index)
                .map_err(|_| FontError::Unavailable)?;
            let name = localized(
                &family
                    .GetFamilyNames()
                    .map_err(|_| FontError::Unavailable)?,
            )?;
            let count = family.GetFontCount();
            if rows.len().saturating_add(count as usize) > FACE_LIMIT {
                return Err(FontError::CatalogLimit);
            }
            for index in 0..count {
                let font = family.GetFont(index).map_err(|_| FontError::Unavailable)?;
                let mut strings = None;
                let mut exists = BOOL(0);
                font.GetInformationalStrings(
                    DWRITE_INFORMATIONAL_STRING_POSTSCRIPT_NAME,
                    &mut strings,
                    &mut exists,
                )
                .map_err(|_| FontError::Unavailable)?;
                let postscript = if exists.as_bool() {
                    localized(&strings.ok_or(FontError::Unavailable)?)?
                } else {
                    String::new()
                };
                let location = location(&font);
                let error = (font.GetSimulations() != DWRITE_FONT_SIMULATIONS_NONE)
                    .then_some(FontError::Unsupported);
                let (path, index) =
                    location.map_or((None, None), |(path, index)| (Some(path), Some(index)));
                push_native(
                    &mut rows,
                    &mut bytes,
                    NativeFace {
                        family: name.clone(),
                        postscript,
                        path,
                        index,
                        error,
                    },
                )?;
            }
        }
        Ok(rows)
    }
}
