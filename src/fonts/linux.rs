use super::{push_native, FontError, NativeFace, FACE_LIMIT};
use std::{
    ffi::{c_char, c_int, c_void},
    os::unix::ffi::OsStringExt,
    path::PathBuf,
};

type Ptr = *mut c_void;
#[repr(C)]
struct FontSet {
    nfont: c_int,
    sfont: c_int,
    fonts: *mut Ptr,
}
#[link(name = "dl")]
extern "C" {
    fn dlopen(name: *const c_char, flags: c_int) -> Ptr;
    fn dlsym(handle: Ptr, name: *const c_char) -> Ptr;
    fn dlclose(handle: Ptr) -> c_int;
}
struct Library(Ptr);
impl Drop for Library {
    fn drop(&mut self) {
        unsafe {
            dlclose(self.0);
        }
    }
}
struct Api {
    config: unsafe extern "C" fn() -> Ptr,
    config_drop: unsafe extern "C" fn(Ptr),
    pattern: unsafe extern "C" fn() -> Ptr,
    pattern_drop: unsafe extern "C" fn(Ptr),
    objects: unsafe extern "C" fn() -> Ptr,
    object_add: unsafe extern "C" fn(Ptr, *const c_char) -> c_int,
    objects_drop: unsafe extern "C" fn(Ptr),
    list: unsafe extern "C" fn(Ptr, Ptr, Ptr) -> *mut FontSet,
    set_drop: unsafe extern "C" fn(Ptr),
    string: unsafe extern "C" fn(Ptr, *const c_char, c_int, *mut *mut u8) -> c_int,
    integer: unsafe extern "C" fn(Ptr, *const c_char, c_int, *mut c_int) -> c_int,
}
impl Api {
    unsafe fn load(library: &Library) -> Result<Self, FontError> {
        macro_rules! symbol {
            ($name:literal, $ty:ty) => {{
                let pointer = dlsym(library.0, concat!($name, "\0").as_ptr().cast());
                if pointer.is_null() {
                    return Err(FontError::Unavailable);
                }
                std::mem::transmute::<Ptr, $ty>(pointer)
            }};
        }
        Ok(Self {
            config: symbol!("FcInitLoadConfigAndFonts", unsafe extern "C" fn() -> Ptr),
            config_drop: symbol!("FcConfigDestroy", unsafe extern "C" fn(Ptr)),
            pattern: symbol!("FcPatternCreate", unsafe extern "C" fn() -> Ptr),
            pattern_drop: symbol!("FcPatternDestroy", unsafe extern "C" fn(Ptr)),
            objects: symbol!("FcObjectSetCreate", unsafe extern "C" fn() -> Ptr),
            object_add: symbol!(
                "FcObjectSetAdd",
                unsafe extern "C" fn(Ptr, *const c_char) -> c_int
            ),
            objects_drop: symbol!("FcObjectSetDestroy", unsafe extern "C" fn(Ptr)),
            list: symbol!(
                "FcFontList",
                unsafe extern "C" fn(Ptr, Ptr, Ptr) -> *mut FontSet
            ),
            set_drop: symbol!("FcFontSetDestroy", unsafe extern "C" fn(Ptr)),
            string: symbol!(
                "FcPatternGetString",
                unsafe extern "C" fn(Ptr, *const c_char, c_int, *mut *mut u8) -> c_int
            ),
            integer: symbol!(
                "FcPatternGetInteger",
                unsafe extern "C" fn(Ptr, *const c_char, c_int, *mut c_int) -> c_int
            ),
        })
    }
    unsafe fn get_string(
        &self,
        pattern: Ptr,
        key: &[u8],
        limit: usize,
    ) -> Result<Option<Vec<u8>>, FontError> {
        let mut pointer = std::ptr::null_mut();
        let result = (self.string)(pattern, key.as_ptr().cast(), 0, &mut pointer);
        if result == 1 {
            return Ok(None);
        } // FcResultNoMatch
        if result != 0 || pointer.is_null() {
            return Err(FontError::Unavailable);
        }
        let mut value = Vec::new();
        for i in 0..=limit {
            let byte = *pointer.add(i);
            if byte == 0 {
                return Ok(Some(value));
            }
            value.push(byte);
        }
        Err(FontError::CatalogLimit)
    }
}
struct Resources<'a> {
    api: &'a Api,
    config: Ptr,
    pattern: Ptr,
    objects: Ptr,
    set: *mut FontSet,
}
impl Drop for Resources<'_> {
    fn drop(&mut self) {
        unsafe {
            if !self.set.is_null() {
                (self.api.set_drop)(self.set.cast());
            }
            if !self.objects.is_null() {
                (self.api.objects_drop)(self.objects);
            }
            if !self.pattern.is_null() {
                (self.api.pattern_drop)(self.pattern);
            }
            if !self.config.is_null() {
                (self.api.config_drop)(self.config);
            }
        }
    }
}
pub(super) fn enumerate() -> Result<Vec<NativeFace>, FontError> {
    unsafe {
        // A fixed system soname; absence is explicit, without a directory-scan substitute.
        let handle = dlopen(b"libfontconfig.so.1\0".as_ptr().cast(), 2); // RTLD_NOW | RTLD_LOCAL
        if handle.is_null() {
            return Err(FontError::Unavailable);
        }
        let library = Library(handle);
        let api = Api::load(&library)?;
        let mut resources = Resources {
            api: &api,
            config: (api.config)(),
            pattern: std::ptr::null_mut(),
            objects: std::ptr::null_mut(),
            set: std::ptr::null_mut(),
        };
        resources.pattern = (api.pattern)();
        resources.objects = (api.objects)();
        if resources.config.is_null() || resources.pattern.is_null() || resources.objects.is_null()
        {
            return Err(FontError::Unavailable);
        }
        for key in [
            b"file\0".as_slice(),
            b"index\0",
            b"family\0",
            b"postscriptname\0",
        ] {
            if (api.object_add)(resources.objects, key.as_ptr().cast()) == 0 {
                return Err(FontError::Unavailable);
            }
        }
        resources.set = (api.list)(resources.config, resources.pattern, resources.objects);
        if resources.set.is_null() {
            return Err(FontError::Unavailable);
        }
        let set = &*resources.set;
        if set.nfont < 0 || set.nfont as usize > FACE_LIMIT || set.sfont < set.nfont {
            return Err(FontError::CatalogLimit);
        }
        if set.nfont > 0 && set.fonts.is_null() {
            return Err(FontError::Unavailable);
        }
        let mut rows = Vec::new();
        let mut bytes = 0;
        for i in 0..set.nfont as usize {
            let pattern = *set.fonts.add(i);
            if pattern.is_null() {
                return Err(FontError::Unavailable);
            }
            let family = String::from_utf8(
                api.get_string(pattern, b"family\0", 512)?
                    .ok_or(FontError::Unavailable)?,
            )
            .map_err(|_| FontError::Unavailable)?;
            let postscript = String::from_utf8(
                api.get_string(pattern, b"postscriptname\0", 512)?
                    .unwrap_or_default(),
            )
            .map_err(|_| FontError::Unavailable)?;
            let path = api
                .get_string(pattern, b"file\0", 32768)?
                .map(|bytes| PathBuf::from(std::ffi::OsString::from_vec(bytes)));
            let mut index = 0;
            let index = ((api.integer)(pattern, b"index\0".as_ptr().cast(), 0, &mut index) == 0)
                .then_some(index);
            let error = index
                .filter(|i| *i < 0 || *i as u32 >= super::FILE_FACES)
                .map(|_| FontError::Variable);
            push_native(
                &mut rows,
                &mut bytes,
                NativeFace {
                    family,
                    postscript,
                    path,
                    index: index.and_then(|i| u32::try_from(i).ok()),
                    error,
                },
            )?;
        }
        Ok(rows)
    }
}
