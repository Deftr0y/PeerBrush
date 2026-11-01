//! Actual native file sources. Byte validation and staging stay off Engine.
use super::*;
use crate::{
    engine::{native::NativeDocument, scene::SceneSnapshot},
    large_document::{FreshFragment, ScratchBytes, Store},
    psd::{
        self,
        bulk36::BulkLimits,
        canonical36::Limits as MetadataLimits,
        import36,
        memory36::{Admitted, MetadataCharge, WorkingSet},
        row_stream::Source,
        structural,
    },
    workspace::source_runtime::SourceSession,
};
use std::io::{Read, Seek, SeekFrom};
const FILE_BYTES: u64 = 256 * 1024 * 1024;
const METADATA_BYTES: usize = 64 * 1024 * 1024;
fn metadata_limits() -> MetadataLimits {
    MetadataLimits {
        bytes: METADATA_BYTES,
        nodes: 8_388_608,
        depth: 128,
    }
}
fn bulk_limits() -> BulkLimits {
    BulkLimits {
        rasters: 65536,
        tiles: 1_048_576,
        references: 1_048_576,
        chunks: 16384,
        encoded_bytes: FILE_BYTES,
    }
}
fn save_limits() -> psd::save36::SaveLimits {
    psd::save36::SaveLimits {
        metadata: metadata_limits(),
        bulk: bulk_limits(),
        file_bytes: FILE_BYTES,
    }
}
fn import_limits() -> import36::ImportLimits {
    import36::ImportLimits {
        metadata: metadata_limits(),
        bulk: bulk_limits(),
        working: WorkingSet {
            raw_bytes: METADATA_BYTES,
            canonical_core_bytes: METADATA_BYTES,
            canonical_manifest_bytes: METADATA_BYTES,
            typed_metadata_bytes: 128 * 1024 * 1024,
            tile_scratch_bytes: psd::bulk36::MAX_TILE_BYTES,
            index_scratch_bytes: 64 * 1024 * 1024,
        },
    }
}

/// This is only transport dispatch. All non36, ambiguous or malformed ordinary
/// sources retain their existing semantic/read-only decoder; no probe result is
/// editable or an identity proof. Native36 admission repeats the entire source.
fn source36_marker(
    file: &mut fs::File,
    bytes: u64,
    session: &SourceSession,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<bool, String> {
    check()?;
    session.check()?;
    file.seek(SeekFrom::Start(0))
        .map_err(|_| "Cannot inspect Photoshop header")?;
    let mut header = [0u8; 26];
    file.read_exact(&mut header)
        .map_err(|_| "Truncated Photoshop header")?;
    if &header[..4] != b"8BPS" {
        return Ok(false);
    }
    fn u32_at(file: &mut fs::File, at: u64, end: u64) -> Result<u64, String> {
        if at.checked_add(4).is_none_or(|n| n > end) {
            return Err("Photoshop resource span is truncated".into());
        }
        file.seek(SeekFrom::Start(at))
            .map_err(|_| "Cannot seek Photoshop resource")?;
        let mut word = [0; 4];
        file.read_exact(&mut word)
            .map_err(|_| "Cannot read Photoshop resource")?;
        Ok(u64::from(u32::from_be_bytes(word)))
    }
    let color = u32_at(file, 26, bytes)?;
    let resource_at = 30u64
        .checked_add(color)
        .ok_or("Photoshop resource offset overflow")?;
    let resource_bytes = u32_at(file, resource_at, bytes)?;
    let start = resource_at + 4;
    let end = start
        .checked_add(resource_bytes)
        .filter(|n| *n <= bytes)
        .ok_or("Photoshop resource section exceeds file")?;
    let mut at = start;
    let mut native = false;
    while at < end {
        check()?;
        session.check()?;
        if at.checked_add(7).is_none_or(|n| n > end) {
            return Err("Truncated Photoshop resource record".into());
        }
        file.seek(SeekFrom::Start(at))
            .map_err(|_| "Cannot seek Photoshop resource")?;
        let mut record = [0; 7];
        file.read_exact(&mut record)
            .map_err(|_| "Cannot read Photoshop resource")?;
        if &record[..4] != b"8BIM" {
            return Ok(false);
        }
        let name = (u64::from(record[6]) + 2) & !1;
        let length_at = at + 6 + name;
        let length = u32_at(file, length_at, end)?;
        let data = length_at + 4;
        let next = data
            .checked_add(length)
            .and_then(|n| n.checked_add(length % 2))
            .filter(|n| *n <= end)
            .ok_or("Photoshop resource exceeds section")?;
        if u16::from_be_bytes([record[4], record[5]]) == 4000 && length >= 4 {
            let mut marker = [0; 4];
            file.read_exact(&mut marker)
                .map_err(|_| "Cannot read private source marker")?;
            if marker == *b"PBR1" {
                let _decoder = MetadataCharge::reserve(&session.resident(), 65536, check)?;
                let mut decoded =
                    ScratchBytes::new(&session.resident(), METADATA_BYTES, METADATA_BYTES)
                        .map_err(|e| e.to_string())?;
                let mut stream = flate2::read::ZlibDecoder::new((&mut *file).take(length - 4));
                let mut used = 0;
                let complete = (|| -> Result<bool, String> {
                    while used < decoded.len() {
                        check()?;
                        session.check()?;
                        let end = (used + 65536).min(decoded.len());
                        let n = match stream.read(&mut decoded[used..end]) {
                            Ok(n) => n,
                            Err(_) => return Ok(false),
                        };
                        if n == 0 {
                            return Ok(true);
                        }
                        used += n;
                        // Existing compact sources put their format first. A
                        // legacy dispatch hint can stop here; the unchanged
                        // semantic decoder still validates the full source.
                        if let Some(prefix) = decoded[..used].strip_prefix(b"{\"format\":") {
                            let digits = prefix.iter().take_while(|b| b.is_ascii_digit()).count();
                            if prefix.get(digits) == Some(&b',')
                                && std::str::from_utf8(&prefix[..digits])
                                    .ok()
                                    .and_then(|v| v.parse::<u32>().ok())
                                    .is_some_and(|v| (1..=34).contains(&v))
                            {
                                return Ok(false);
                            }
                        }
                    }
                    check()?;
                    Ok(matches!(stream.read(&mut [0u8; 1]), Ok(0)))
                })()?;
                if complete {
                    #[derive(serde::Deserialize)]
                    struct Header {
                        format: u32,
                    }
                    native |= serde_json::from_slice::<Header>(&decoded[..used])
                        .is_ok_and(|h| h.format == 36);
                }
            }
        }
        at = next;
    }
    check()?;
    session.check()?;
    Ok(native)
}

struct ImportOwner<'a> {
    session: &'a SourceSession,
    fragment: &'a FreshFragment,
}
impl import36::ImportSession for ImportOwner<'_> {
    fn store(&self) -> &Store {
        self.fragment.store()
    }
    fn check(&self) -> Result<(), String> {
        self.session.check()
    }
}
