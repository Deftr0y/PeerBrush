use crate::{
    file_safety::{self, owned_rename},
    large_document::{Reservation, ResidentBudget},
};
use ring::digest::{Context, SHA256};
use std::{
    cell::Cell,
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    rc::Rc,
};
const DISK: u64 = 1024 * 1024 * 1024;
pub(super) struct Files {
    pub pool: ResidentBudget,
    pub root: PathBuf,
    state: PathBuf,
    root_guard: File,
    used: Rc<Cell<u64>>,
    images: Vec<Image>,
    directory: Option<(PathBuf, File)>,
    _memory: Reservation,
}
struct Image {
    spool: Spool,
    path: PathBuf,
    _memory: Reservation,
}
pub(super) struct Spool {
    file: File,
    path: PathBuf,
    identity: (u64, u64),
    bytes: u64,
    hash: Context,
    sealed: Option<owned_rename::Sealed>,
    used: Rc<Cell<u64>>,
    published: bool,
    modified: Option<std::time::SystemTime>,
}
fn regular(path: &Path) -> Result<(), String> {
    let m = fs::symlink_metadata(path).map_err(|_| "CLI staging path unavailable")?;
    if m.file_type().is_symlink() || !m.is_file() {
        return Err("CLI staging path changed".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if m.file_attributes() & 0x400 != 0 {
            return Err("CLI staging alias refused".into());
        }
    }
    Ok(())
}
impl Files {
    pub(super) fn new(state: &Path) -> Result<Self, String> {
        let pool = ResidentBudget::new(16 * 1024 * 1024).map_err(|e| e.to_string())?;
        // Input/output buffers, bounded parser stack and at most128 file owners.
        let memory = pool
            .reserve_bytes(1024 * 1024, 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let parent = fs::canonicalize(std::env::temp_dir())
            .map_err(|_| "CLI temporary directory unavailable")?;
        let root = parent.join(format!("peerbrush-cli-{}", crate::engine::id()));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&root)
            .map_err(|_| "CLI temporary directory creation refused")?;
        let mut images = Vec::new();
        images
            .try_reserve_exact(128)
            .map_err(|_| "CLI image index allocation refused")?;
        let root_guard = owned_rename::open_directory(&root)?;
        Ok(Self {
            pool,
            root,
            root_guard,
            state: state.to_owned(),
            used: Rc::new(Cell::new(0)),
            images,
            directory: None,
            _memory: memory,
        })
    }
    pub(super) fn ensure_directory(&mut self) -> Result<(), String> {
        if self.directory.is_none() {
            let path = self.state.join("previews");
            fs::create_dir_all(&path).map_err(|_| "CLI preview directory unavailable")?;
            let m = fs::symlink_metadata(&path).map_err(|_| "CLI preview directory unavailable")?;
            if !m.is_dir() || m.file_type().is_symlink() {
                return Err("CLI preview directory alias refused".into());
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if m.file_attributes() & 0x400 != 0 {
                    return Err("CLI preview directory alias refused".into());
                }
            }
            let canonical =
                fs::canonicalize(&path).map_err(|_| "CLI preview directory unavailable")?;
            let held = owned_rename::open_directory(&canonical)?;
            self.directory = Some((canonical, held));
        }
        Ok(())
    }
    fn check_root(&self) -> Result<(), String> {
        let named = owned_rename::open_directory(&self.root)?;
        if file_safety::identity(&named)? != file_safety::identity(&self.root_guard)? {
            return Err("CLI temporary directory ownership changed".into());
        }
        Ok(())
    }
    pub(super) fn image(&mut self) -> Result<(usize, String), String> {
        if self.images.len() == 128 {
            return Err("CLI image count exceeds its bound".into());
        }
        self.ensure_directory()?;
        let name = format!("{}.png", crate::engine::id());
        let display = self.state.join("previews").join(&name);
        let text = display
            .to_str()
            .ok_or("CLI preview path is not valid text")?;
        let memory = self
            .pool
            .reserve_bytes(
                text.len()
                    .checked_mul(8)
                    .and_then(|n| n.checked_add(4096))
                    .ok_or("CLI path admission overflow")?,
                1024 * 1024,
            )
            .map_err(|e| e.to_string())?;
        let path = self.directory.as_ref().unwrap().0.join(name);
        let spool = Spool::create(
            &path.with_extension(format!("{}.tmp", crate::engine::id())),
            self.used.clone(),
        )?;
        let text = text.to_owned();
        let index = self.images.len();
        self.images.push(Image {
            spool,
            path,
            _memory: memory,
        });
        Ok((index, text))
    }
    pub(super) fn image_writer(&mut self, index: usize) -> &mut Spool {
        &mut self.images[index].spool
    }
    pub(super) fn publish(
        &mut self,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        for image in &mut self.images {
            image.spool.seal(&self.pool, check)?;
        }
        for image in &mut self.images {
            check()?;
            let (directory, held) = self.directory.as_ref().unwrap();
            owned_rename::publish_new(
                directory,
                held,
                image.spool.path.file_name().unwrap().as_ref(),
                image.path.file_name().unwrap().as_ref(),
                &image.spool.file,
                image.spool.sealed.as_ref().unwrap(),
                check,
            )?;
            image.spool.published = true;
        }
        Ok(())
    }
}
impl Drop for Files {
    fn drop(&mut self) {
        self.images.clear();
        if self.check_root().is_ok() {
            let _ = fs::remove_dir(&self.root);
        }
    }
}
impl Spool {
    pub(super) fn temporary(files: &Files, name: &str) -> Result<Self, String> {
        files.check_root()?;
        let spool = Self::create(&files.root.join(name), files.used.clone())?;
        files.check_root()?;
        Ok(spool)
    }
    fn create(path: &Path, used: Rc<Cell<u64>>) -> Result<Self, String> {
        let file = owned_rename::staging_options()
            .open(path)
            .map_err(|_| "CLI staging file creation refused")?;
        let identity = file_safety::identity(&file)?;
        Ok(Self {
            file,
            path: path.to_owned(),
            identity,
            bytes: 0,
            hash: Context::new(&SHA256),
            sealed: None,
            used,
            published: false,
            modified: None,
        })
    }
    fn check(&self) -> Result<(), String> {
        regular(&self.path)?;
        let named = file_safety::open_native_identity(&self.path)
            .map_err(|_| "CLI staging identity unavailable")?;
        if file_safety::identity(&named)? != self.identity
            || file_safety::identity(&self.file)? != self.identity
            || file_safety::link_count(&self.file)? != 1
            || self
                .file
                .metadata()
                .map_err(|_| "CLI staging metadata unavailable")?
                .len()
                != self.bytes
        {
            return Err("CLI staging ownership or size changed".into());
        }
        if self.modified.is_some_and(|expected| {
            self.file.metadata().and_then(|m| m.modified()).ok() != Some(expected)
        }) {
            return Err("CLI sealed staging stamp changed".into());
        }
        Ok(())
    }
    pub(super) fn seal(
        &mut self,
        pool: &ResidentBudget,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        self.check()?;
        self.sealed = Some(owned_rename::Sealed::verify(
            &self.file,
            self.bytes,
            self.hash.clone().finish().as_ref().try_into().unwrap(),
            pool,
            check,
        )?);
        self.modified = Some(
            self.file
                .metadata()
                .and_then(|m| m.modified())
                .map_err(|_| "CLI staging stamp unavailable")?,
        );
        Ok(())
    }
    pub(super) fn replay(
        &mut self,
        output: &mut impl Write,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        if self.sealed.is_none() {
            return Err("CLI spool is not sealed".into());
        }
        self.check()?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|_| "CLI staging rewind failed")?;
        let mut buffer = [0; 64 * 1024];
        let mut left = self.bytes;
        let mut hash = Context::new(&SHA256);
        while left > 0 {
            check()?;
            let count = left.min(buffer.len() as u64) as usize;
            self.file
                .read_exact(&mut buffer[..count])
                .map_err(|_| "CLI staging read interrupted")?;
            hash.update(&buffer[..count]);
            output
                .write_all(&buffer[..count])
                .map_err(|_| "CLI output interrupted")?;
            left -= count as u64;
        }
        self.check()?;
        if hash.finish().as_ref() != self.hash.clone().finish().as_ref() {
            return Err("CLI staging replay changed".into());
        }
        Ok(())
    }
}
impl Write for Spool {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.sealed.is_some()
            || self
                .used
                .get()
                .checked_add(bytes.len() as u64)
                .is_none_or(|n| n > DISK)
        {
            return Err(std::io::Error::other(
                "CLI temporary disk admission exceeded",
            ));
        }
        let n = self.file.write(bytes)?;
        self.hash.update(&bytes[..n]);
        self.bytes += n as u64;
        self.used.set(self.used.get() + n as u64);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}
impl Drop for Spool {
    fn drop(&mut self) {
        if !self.published && self.check().is_ok() {
            let _ = fs::remove_file(&self.path);
        }
    }
}
