use super::{push_native, FontError, NativeFace, FACE_LIMIT};
use std::{ffi::c_void, os::unix::ffi::OsStringExt, path::PathBuf};

type Ref = *const c_void;
#[repr(C)]
struct Range {
    location: isize,
    length: isize,
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(value: Ref);
    fn CFGetTypeID(value: Ref) -> usize;
    fn CFArrayGetTypeID() -> usize;
    fn CFArrayGetCount(array: Ref) -> isize;
    fn CFArrayGetValueAtIndex(array: Ref, index: isize) -> Ref;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFDictionaryGetCount(dictionary: Ref) -> isize;
    fn CFStringGetTypeID() -> usize;
    fn CFStringGetLength(string: Ref) -> isize;
    fn CFStringGetCharacters(string: Ref, range: Range, characters: *mut u16);
    fn CFURLGetTypeID() -> usize;
    fn CFURLCopyScheme(url: Ref) -> Ref;
    fn CFURLGetFileSystemRepresentation(url: Ref, resolve: u8, buffer: *mut u8, size: isize) -> u8;
}
#[link(name = "CoreText", kind = "framework")]
extern "C" {
    fn CTFontCollectionCreateFromAvailableFonts(options: Ref) -> Ref;
    fn CTFontCollectionCreateMatchingFontDescriptors(collection: Ref) -> Ref;
    fn CTFontDescriptorCopyAttribute(descriptor: Ref, attribute: Ref) -> Ref;
    static kCTFontURLAttribute: Ref;
    static kCTFontNameAttribute: Ref;
    static kCTFontFamilyNameAttribute: Ref;
    static kCTFontVariationAttribute: Ref;
}
struct Owned(Ref);
impl Owned {
    fn new(value: Ref) -> Result<Self, FontError> {
        if value.is_null() {
            Err(FontError::Unavailable)
        } else {
            Ok(Self(value))
        }
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        unsafe {
            CFRelease(self.0);
        }
    }
}
fn string(value: Ref) -> Result<String, FontError> {
    unsafe {
        if value.is_null() || CFGetTypeID(value) != CFStringGetTypeID() {
            return Err(FontError::Unavailable);
        }
        let length = CFStringGetLength(value);
        if !(1..=512).contains(&length) {
            return Err(FontError::Unavailable);
        }
        let mut data = vec![0; length as usize];
        CFStringGetCharacters(
            value,
            Range {
                location: 0,
                length,
            },
            data.as_mut_ptr(),
        );
        String::from_utf16(&data).map_err(|_| FontError::Unavailable)
    }
}
fn attribute(descriptor: Ref, key: Ref) -> Result<String, FontError> {
    let value = Owned::new(unsafe { CTFontDescriptorCopyAttribute(descriptor, key) })?;
    string(value.0)
}
fn path(descriptor: Ref) -> Option<PathBuf> {
    unsafe {
        let url = Owned::new(CTFontDescriptorCopyAttribute(
            descriptor,
            kCTFontURLAttribute,
        ))
        .ok()?;
        if CFGetTypeID(url.0) != CFURLGetTypeID() {
            return None;
        }
        let scheme = Owned::new(CFURLCopyScheme(url.0)).ok()?;
        if string(scheme.0).ok()? != "file" {
            return None;
        }
        let mut buffer = vec![0u8; 32768];
        if CFURLGetFileSystemRepresentation(url.0, 1, buffer.as_mut_ptr(), buffer.len() as isize)
            == 0
        {
            return None;
        }
        let length = buffer.iter().position(|b| *b == 0)?;
        buffer.truncate(length);
        Some(PathBuf::from(std::ffi::OsString::from_vec(buffer)))
    }
}
pub(super) fn enumerate() -> Result<Vec<NativeFace>, FontError> {
    unsafe {
        // Enumerate available descriptors only: never register or activate fonts.
        let collection = Owned::new(CTFontCollectionCreateFromAvailableFonts(std::ptr::null()))?;
        let descriptors = Owned::new(CTFontCollectionCreateMatchingFontDescriptors(collection.0))?;
        if CFGetTypeID(descriptors.0) != CFArrayGetTypeID() {
            return Err(FontError::Unavailable);
        }
        let count = CFArrayGetCount(descriptors.0);
        if count < 0 || count as usize > FACE_LIMIT {
            return Err(FontError::CatalogLimit);
        }
        let mut rows = Vec::new();
        let mut bytes = 0;
        for i in 0..count {
            let descriptor = CFArrayGetValueAtIndex(descriptors.0, i);
            if descriptor.is_null() {
                return Err(FontError::Unavailable);
            }
            let variation = Owned::new(CTFontDescriptorCopyAttribute(
                descriptor,
                kCTFontVariationAttribute,
            ))
            .ok();
            let variable = variation.as_ref().is_some_and(|value| {
                CFGetTypeID(value.0) == CFDictionaryGetTypeID() && CFDictionaryGetCount(value.0) > 0
            });
            push_native(
                &mut rows,
                &mut bytes,
                NativeFace {
                    family: attribute(descriptor, kCTFontFamilyNameAttribute)?,
                    postscript: attribute(descriptor, kCTFontNameAttribute)?,
                    path: path(descriptor),
                    index: None,
                    error: variable.then_some(FontError::Variable),
                },
            )?;
        }
        Ok(rows)
    }
}
