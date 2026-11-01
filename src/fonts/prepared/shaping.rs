//! Count reference edges, including repeated offsets, before harfrust constructs
//! per-lookup and AAT caches. File length alone cannot bound their amplification.
use ttf_parser::{Face, Tag};
fn word(bytes: &[u8], at: usize) -> Result<usize, String> {
    bytes
        .get(at..at.checked_add(2).ok_or("Font offset overflow")?)
        .map(|v| u16::from_be_bytes(v.try_into().unwrap()) as usize)
        .ok_or_else(|| "Truncated shaping table".into())
}
fn long(bytes: &[u8], at: usize) -> Result<usize, String> {
    bytes
        .get(at..at.checked_add(4).ok_or("Font offset overflow")?)
        .map(|v| u32::from_be_bytes(v.try_into().unwrap()) as usize)
        .ok_or_else(|| "Truncated shaping table".into())
}
fn total(sum: &mut usize, n: usize, max: usize) -> Result<(), String> {
    *sum = sum
        .checked_add(n)
        .filter(|n| *n <= max)
        .ok_or("Font shaping graph exceeds bounded cache admission")?;
    Ok(())
}
fn ot(
    bytes: &[u8],
    lookups: &mut usize,
    subtables: &mut usize,
    features: &mut usize,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let list = word(bytes, 8)?;
    let count = word(bytes, list)?;
    total(lookups, count, 1024)?;
    for i in 0..count {
        check()?;
        let at = list
            .checked_add(word(bytes, list + 2 + i * 2)?)
            .ok_or("Font offset overflow")?;
        let n = word(bytes, at + 4)?;
        total(subtables, n, 8192)?;
        for j in 0..n {
            let offset = word(bytes, at + 6 + j * 2)?;
            if offset == 0 || at.checked_add(offset).is_none_or(|v| v >= bytes.len()) {
                return Err("Invalid shaping subtable reference".into());
            }
        }
    }
    let list = word(bytes, 6)?;
    let count = word(bytes, list)?;
    total(features, count, 4096)?;
    for i in 0..count {
        check()?;
        let at = list
            .checked_add(word(bytes, list + 2 + i * 6 + 4)?)
            .ok_or("Font feature offset overflow")?;
        total(features, word(bytes, at + 2)?, 65536)?;
    }
    Ok(())
}
fn aat(
    bytes: &[u8],
    kind: &[u8; 4],
    sum: &mut usize,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<(), String> {
    if kind == b"morx" {
        let chains = long(bytes, 4)?;
        total(sum, chains, 128)?;
        let mut at = 8usize;
        for _ in 0..chains {
            check()?;
            let length = long(bytes, at + 4)?;
            let count = long(bytes, at + 12)?;
            total(sum, count, 128)?;
            let feature_bytes = long(bytes, at + 8)?
                .checked_mul(12)
                .ok_or("Font feature footprint overflow")?;
            let mut sub = at
                .checked_add(16)
                .and_then(|n| n.checked_add(feature_bytes))
                .ok_or("Font offset overflow")?;
            let end = at
                .checked_add(length)
                .filter(|n| *n <= bytes.len())
                .ok_or("Truncated AAT chain")?;
            for _ in 0..count {
                let n = long(bytes, sub)?;
                if n < 12 {
                    return Err("Invalid AAT subtable length".into());
                }
                sub = sub
                    .checked_add(n)
                    .filter(|n| *n <= end)
                    .ok_or("Truncated AAT subtable")?;
            }
            if length < 16 || sub > end {
                return Err("Invalid AAT chain length".into());
            }
            at = end;
        }
    } else {
        let apple = kind == b"kerx" || word(bytes, 0)? != 0;
        let (count, mut at) = if apple {
            (long(bytes, 4)?, 8usize)
        } else {
            (word(bytes, 2)?, 4usize)
        };
        total(sum, count, 128)?;
        for _ in 0..count {
            check()?;
            let length = if apple {
                long(bytes, at)?
            } else {
                word(bytes, at + 2)?
            };
            if length < (if apple { 8 } else { 6 }) {
                return Err("Invalid kerning subtable length".into());
            }
            at = at
                .checked_add(length)
                .filter(|n| *n <= bytes.len())
                .ok_or("Truncated kerning subtable")?;
        }
    }
    Ok(())
}
pub(super) fn preflight(
    face: &Face<'_>,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<(), String> {
    check()?;
    // harfrust0.12 limits working glyph buffers to256×input (min65536).
    // Its fixed cache payload types and graph caps below are accounted by the
    // separate192MiB operation ticket, not by the source font-byte reservation.
    if std::mem::size_of::<harfrust::GlyphInfo>() > 20
        || std::mem::size_of::<harfrust::GlyphPosition>() > 20
    {
        return Err("Shaping buffer layout requires a new memory audit".into());
    }
    let (mut lookups, mut subtables, mut features) = (0, 0, 0);
    for tag in [b"GSUB", b"GPOS"] {
        if let Some(bytes) = face.raw_face().table(Tag::from_bytes(tag)) {
            ot(bytes, &mut lookups, &mut subtables, &mut features, check)?;
        }
    }
    let mut caches = 0;
    for tag in [b"morx", b"kern", b"kerx"] {
        if let Some(bytes) = face.raw_face().table(Tag::from_bytes(tag)) {
            aat(bytes, tag, &mut caches, check)?;
        }
    }
    check()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn graph(count: u16, subtables: u16) -> Vec<u8> {
        let list = 10usize;
        let lookup = list + 2 + usize::from(count) * 2;
        let feature = lookup + 6 + usize::from(subtables) * 2 + 2;
        let mut bytes = vec![0; feature + 2];
        bytes[..2].copy_from_slice(&1u16.to_be_bytes());
        bytes[6..8].copy_from_slice(&(feature as u16).to_be_bytes());
        bytes[8..10].copy_from_slice(&(list as u16).to_be_bytes());
        bytes[list..list + 2].copy_from_slice(&count.to_be_bytes());
        for i in 0..usize::from(count) {
            bytes[list + 2 + i * 2..list + 4 + i * 2]
                .copy_from_slice(&((lookup - list) as u16).to_be_bytes());
        }
        bytes[lookup..lookup + 2].copy_from_slice(&1u16.to_be_bytes());
        bytes[lookup + 4..lookup + 6].copy_from_slice(&subtables.to_be_bytes());
        for i in 0..usize::from(subtables) {
            bytes[lookup + 6 + i * 2..lookup + 8 + i * 2]
                .copy_from_slice(&(6 + subtables * 2).to_be_bytes());
        }
        bytes
    }
    #[test]
    fn repeated_lookup_edges_are_counted_before_cache_allocation() {
        let mut lookup = 0;
        let mut sub = 0;
        let mut feature = 0;
        ot(
            &graph(512, 16),
            &mut lookup,
            &mut sub,
            &mut feature,
            &mut || Ok(()),
        )
        .unwrap();
        assert_eq!((lookup, sub), (512, 8192));
        let error = ot(&graph(513, 16), &mut 0, &mut 0, &mut 0, &mut || Ok(())).unwrap_err();
        assert!(error.contains("bounded cache admission"));
    }
    #[test]
    fn hostile_cache_counts_and_truncated_references_fail_closed() {
        assert!(ot(&graph(1025, 1), &mut 0, &mut 0, &mut 0, &mut || Ok(())).is_err());
        let mut aat_bytes = vec![0; 8];
        aat_bytes[4..8].copy_from_slice(&129u32.to_be_bytes());
        assert!(aat(&aat_bytes, b"morx", &mut 0, &mut || Ok(()))
            .unwrap_err()
            .contains("bounded cache admission"));
        let mut bytes = graph(1, 1);
        bytes[12..14].copy_from_slice(&u16::MAX.to_be_bytes());
        assert!(ot(&bytes, &mut 0, &mut 0, &mut 0, &mut || Ok(())).is_err());
        assert_eq!(
            ot(&graph(1, 1), &mut 0, &mut 0, &mut 0, &mut || Err(
                "shaping canceled".into()
            ))
            .unwrap_err(),
            "shaping canceled"
        );
    }
}
