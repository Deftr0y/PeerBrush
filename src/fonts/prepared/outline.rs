//! Bound the actual pinned parser's emitted outline before ab_glyph allocates
//! its infallible curve vector. No outline or font payload escapes this pass.
pub(super) fn preparse(
    bytes: &[u8],
    index: u32,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<(), String> {
    check()?;
    let face = ttf_parser::Face::parse(bytes, index).map_err(|_| "Invalid immutable font face")?;
    let mut capacity = 0usize;
    let mut item = |size: usize| -> Result<(), String> {
        check()?;
        capacity = capacity
            .checked_add(size.checked_mul(4).ok_or("Font cache footprint overflow")?)
            .filter(|n| *n <= 768 * 1024)
            .ok_or("Font mapping cache exceeds snapshot admission")?;
        Ok(())
    };
    for table in face
        .tables()
        .cmap
        .iter()
        .flat_map(|t| t.subtables)
        .filter(|t| t.is_unicode())
    {
        let _ = table;
        item(std::mem::size_of::<ttf_parser::cmap::Subtable<'_>>())?;
    }
    for table in face
        .tables()
        .kern
        .iter()
        .flat_map(|t| t.subtables)
        .filter(|t| t.horizontal && !t.variable)
    {
        let _ = table;
        item(std::mem::size_of::<ttf_parser::kern::Subtable<'_>>())?;
    }
    Ok(())
}
pub(super) fn preflight(
    bytes: &[u8],
    index: u32,
    id: u16,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<(), String> {
    check()?;
    let face = ttf_parser::Face::parse(bytes, index).map_err(|_| "Invalid immutable font face")?;
    if id >= face.number_of_glyphs() {
        return Err("Glyph is outside the admitted font face".into());
    }
    let mut visitor = OutlineLimit {
        events: 0,
        check,
        error: None,
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        face.outline_glyph(ttf_parser::GlyphId(id), &mut visitor)
    }));
    match result {
        Ok(_) => (visitor.check)(),
        Err(_) => Err(visitor
            .error
            .unwrap_or_else(|| "Font outline parser refused bounded preparation".into())),
    }
}
struct OutlineLimit<'a> {
    events: usize,
    check: &'a mut dyn FnMut() -> Result<(), String>,
    error: Option<String>,
}
impl OutlineLimit<'_> {
    fn event(&mut self, values: &[f32]) {
        self.events += 1;
        let error = if self.events > 32768 {
            Some("Glyph outline exceeds the bounded curve event budget".into())
        } else if values.iter().any(|v| !v.is_finite()) {
            Some("Glyph outline contains nonfinite geometry".into())
        } else if self.events % 1024 == 1 {
            (self.check)().err()
        } else {
            None
        };
        if let Some(error) = error {
            self.error = Some(error);
            std::panic::resume_unwind(Box::new(()));
        }
    }
}
impl ttf_parser::OutlineBuilder for OutlineLimit<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.event(&[x, y]);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.event(&[x, y]);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.event(&[x1, y1, x, y]);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.event(&[x1, y1, x2, y2, x, y]);
    }
    fn close(&mut self) {
        self.event(&[]);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    const FONT: &[u8] = include_bytes!("../../../assets/fonts/UbuntuSans-Regular.ttf");
    fn record(bytes: &[u8], tag: &[u8; 4]) -> usize {
        (0..u16::from_be_bytes(bytes[4..6].try_into().unwrap()) as usize)
            .map(|i| 12 + i * 16)
            .find(|i| &bytes[*i..*i + 4] == tag)
            .unwrap()
    }
    fn install(bytes: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]) {
        while bytes.len() % 4 != 0 {
            bytes.push(0);
        }
        let at = record(bytes, tag);
        let offset = bytes.len() as u32;
        bytes[at + 8..at + 12].copy_from_slice(&offset.to_be_bytes());
        bytes[at + 12..at + 16].copy_from_slice(&(data.len() as u32).to_be_bytes());
        bytes.extend_from_slice(data);
    }
    fn simple(points: u16) -> Vec<u8> {
        let mut b = vec![0, 1, 0, 0, 0, 0, 0, 1, 0, 1];
        b.extend_from_slice(&(points - 1).to_be_bytes());
        b.extend_from_slice(&0u16.to_be_bytes());
        let mut n = usize::from(points);
        while n > 0 {
            let group = n.min(256);
            b.extend_from_slice(&[0x39, (group - 1) as u8]);
            n -= group;
        }
        b
    }
    fn fixture(composite: bool) -> Vec<u8> {
        let mut bytes = FONT.to_vec();
        let head = record(&bytes, b"head");
        let offset = u32::from_be_bytes(bytes[head + 8..head + 12].try_into().unwrap()) as usize;
        bytes[offset + 50..offset + 52].copy_from_slice(&1i16.to_be_bytes());
        let maxp = record(&bytes, b"maxp");
        let offset = u32::from_be_bytes(bytes[maxp + 8..maxp + 12].try_into().unwrap()) as usize;
        bytes[offset + 4..offset + 6]
            .copy_from_slice(&(if composite { 3u16 } else { 2u16 }).to_be_bytes());
        let mut glyph = simple(if composite { 4 } else { 40000 });
        let first = glyph.len() as u32;
        let mut offsets = vec![0, 0, first];
        if composite {
            glyph.extend_from_slice(&[0xff, 0xff, 0, 0, 0, 0, 0, 1, 0, 1]);
            for i in 0..8192 {
                glyph.extend_from_slice(&(if i == 8191 { 3u16 } else { 35u16 }).to_be_bytes());
                glyph.extend_from_slice(&[0, 1, 0, 0, 0, 0]);
            }
            offsets.push(glyph.len() as u32);
        }
        install(&mut bytes, b"glyf", &glyph);
        install(
            &mut bytes,
            b"loca",
            &offsets
                .into_iter()
                .flat_map(u32::to_be_bytes)
                .collect::<Vec<_>>(),
        );
        bytes
    }
    #[test]
    fn ordinary_composite_and_blank_outlines_are_admitted() {
        preparse(FONT, 0, &mut || Ok(())).unwrap();
        let face = ttf_parser::Face::parse(FONT, 0).unwrap();
        for ch in ['A', 'À', 'é', ' '] {
            let id = face.glyph_index(ch).unwrap().0;
            preflight(FONT, 0, id, &mut || Ok(())).unwrap();
        }
        assert!(preflight(FONT, 0, u16::MAX, &mut || Ok(())).is_err());
    }
    #[test]
    fn mapping_cache_counts_repeated_cmap_edges_before_owned_font_parse() {
        let mut bytes = FONT.to_vec();
        let count = u16::MAX;
        let offset = 4 + usize::from(count) * 8;
        let mut cmap = vec![0; offset + 262];
        cmap[2..4].copy_from_slice(&count.to_be_bytes());
        for i in 0..usize::from(count) {
            let at = 4 + i * 8;
            cmap[at + 4..at + 8].copy_from_slice(&(offset as u32).to_be_bytes());
        }
        cmap[offset + 2..offset + 4].copy_from_slice(&262u16.to_be_bytes());
        install(&mut bytes, b"cmap", &cmap);
        assert!(preparse(&bytes, 0, &mut || Ok(()))
            .unwrap_err()
            .contains("mapping cache exceeds snapshot admission"));
    }
    #[test]
    fn actual_simple_and_repeated_composite_graphs_stop_at_event_bound() {
        for composite in [false, true] {
            let bytes = fixture(composite);
            let error =
                preflight(&bytes, 0, if composite { 2 } else { 1 }, &mut || Ok(())).unwrap_err();
            assert!(error.contains("curve event budget"), "{error}");
        }
    }
    #[test]
    fn parser_callback_cancellation_and_nonfinite_geometry_fail_closed() {
        let face = ttf_parser::Face::parse(FONT, 0).unwrap();
        let id = face.glyph_index('A').unwrap().0;
        let mut calls = 0;
        let error = preflight(FONT, 0, id, &mut || {
            calls += 1;
            if calls == 2 {
                Err("outline canceled".into())
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(error, "outline canceled");
        assert_eq!(calls, 2);
        let mut check = || Ok(());
        let mut visitor = OutlineLimit {
            events: 0,
            check: &mut check,
            error: None,
        };
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || visitor.event(&[f32::NAN])
        ))
        .is_err());
        assert!(visitor.error.unwrap().contains("nonfinite"));
    }
}
