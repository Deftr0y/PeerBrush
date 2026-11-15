use super::{Content, Source, TextLayout};
use ab_glyph::{point, Font, Glyph, PxScale, ScaleFont};
use std::ops::{Range, RangeInclusive};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy)]
pub(super) struct LegacyStyle<'a> {
    pub width: u32,
    pub size: f32,
    pub line_height: f32,
    pub letter_spacing: f32,
    pub align: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bidi_boundaries_keep_both_carets_and_split_visually_disjoint_selections() {
        let text = "abאבcd";
        let mut map = TextCarets::new(text, TextLayout::Shaped).unwrap();
        let mut clusters = [
            Cluster { bytes: 0..1, advance: [0., 10.], rtl: false },
            Cluster { bytes: 1..2, advance: [10., 20.], rtl: false },
            Cluster { bytes: 4..6, advance: [20., 30.], rtl: true },
            Cluster { bytes: 2..4, advance: [30., 40.], rtl: true },
            Cluster { bytes: 6..7, advance: [40., 50.], rtl: false },
            Cluster { bytes: 7..8, advance: [50., 60.], rtl: false },
        ];
        map.shaped_row(text, 0., [0., 12.], &mut clusters, false);
        assert_eq!(map.at(Caret { index: 2, trailing: true }).unwrap().0, 20.);
        assert_eq!(map.at(Caret { index: 2, trailing: false }).unwrap().0, 40.);
        assert_eq!(map.highlights(1..3).collect::<Vec<_>>(), [
            [10., 0., 20., 12.], [30., 0., 40., 12.],
        ]);
    }

    #[test]
    fn line_navigation_keeps_both_sides_of_a_soft_wrap() {
        let mut map = TextCarets::new("abcd", TextLayout::Paragraph).unwrap();
        for (index, text) in ["ab", "cd"].into_iter().enumerate() {
            let mut clusters = [
                Cluster { bytes: 0..1, advance: [0., 10.], rtl: false },
                Cluster { bytes: 1..2, advance: [10., 20.], rtl: false },
            ];
            let top = index as f32 * 12.;
            map.shaped_row(text, 0., [top, top + 12.], &mut clusters, index == 0);
        }
        let end = map.step(Caret { index: 1, trailing: false }, CaretMove::LineEnd, &mut None);
        let start = map.step(Caret { index: 3, trailing: false }, CaretMove::LineStart, &mut None);
        assert_eq!(end.index, start.index);
        assert_eq!(end.index, 2);
        assert_eq!(map.at(end), Some((20., [0., 12.])));
        assert_eq!(map.at(start), Some((0., [12., 24.])));
    }
}

pub(super) struct LinePosition {
    pub start: f32,
    pub vertical: [f32; 2],
}

impl LegacyStyle<'_> {
    pub(super) fn line<'a>(
        self,
        font: &'a impl Font,
        row: usize,
        line: &'a str,
    ) -> (LinePosition, impl Iterator<Item = (char, Glyph, f32)> + 'a) {
        let Self { width, size, line_height, letter_spacing, align } = self;
        let scaled = font.as_scaled(PxScale::from(size));
        let mut advance = 0.;
        let mut previous = None;
        for ch in line.chars() {
            let glyph = scaled.glyph_id(ch);
            if let Some(previous) = previous {
                advance += scaled.kern(previous, glyph);
                advance += letter_spacing;
            }
            advance += scaled.h_advance(glyph);
            previous = Some(glyph);
        }
        let start = match align {
            "center" => (width as f32 - advance) * 0.5,
            "right" => width as f32 - advance,
            _ => 0.,
        };
        let ascent = scaled.ascent();
        let baseline = ascent + row as f32 * scaled.height() * line_height;
        let vertical = [baseline - ascent, baseline - scaled.descent()];
        let glyphs = line.chars().scan((start, None), move |(x, previous), ch| {
            let id = scaled.glyph_id(ch);
            if let Some(previous) = *previous {
                *x += scaled.kern(previous, id);
                *x += letter_spacing;
            }
            let glyph = id.with_scale_and_position(size, point(*x, baseline));
            *x += scaled.h_advance(id);
            *previous = Some(id);
            Some((ch, glyph, *x))
        });
        (LinePosition { start, vertical }, glyphs)
    }
}

struct CaretRow {
    indices: RangeInclusive<usize>,
    vertical: [f32; 2],
    offset: usize,
    start: f32,
    soft_start: bool,
    soft_end: bool,
}

#[derive(Clone, Copy)]
struct Cell {
    edges: [f32; 2],
    grapheme_start: bool,
}

pub(super) struct Cluster {
    pub(super) bytes: Range<usize>,
    pub(super) advance: [f64; 2],
    pub(super) rtl: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct Caret {
    pub(crate) index: usize,
    pub(crate) trailing: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum CaretMove {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    LineEnd,
}

pub(crate) struct TextCarets {
    rows: Vec<CaretRow>,
    cells: Vec<Cell>,
}

pub(crate) struct NativeCarets {
    pub(crate) map: TextCarets,
    _admission: crate::large_document::Reservation,
}

impl NativeCarets {
    pub(super) fn new(text: &str, layout: TextLayout, pool: &crate::large_document::ResidentBudget) -> Result<Self, String> {
        let bytes = TextCarets::ADMISSION_BYTES;
        let admission = pool.reserve_bytes(bytes, bytes).map_err(|error| error.to_string())?;
        Ok(Self { map: TextCarets::new(text, layout)?, _admission: admission })
    }
}

impl TextCarets {
    pub(crate) const ADMISSION_BYTES: usize = 64 * 1024;

    // This path runs after a successful resident preview, on the preview worker.
    pub(crate) fn resident(source: &Source) -> Result<Self, String> {
        let Content::Text { layout, font, text, size, line_height, letter_spacing, align, .. } =
            &source.content else { return Err("Text layout requires a text source".into()) };
        let font = crate::fonts::snapshot(font).map_err(|error| error.to_string())?;
        if *layout == TextLayout::Legacy {
            let style = LegacyStyle {
                width: source.width, size: *size, line_height: *line_height,
                letter_spacing: *letter_spacing, align,
            };
            return Self::legacy(text, style, &font);
        }
        let mut carets = Self::new(text, *layout)?;
        super::shaping::layout_with_carets(source, &font, Some(&mut carets))?;
        Ok(carets)
    }

    // The canvas can measure these static faces without loading installed font data.
    pub(crate) fn bundled_legacy(source: &Source) -> Result<Option<Self>, String> {
        let Content::Text {
            layout: TextLayout::Legacy,
            font: crate::fonts::TextFont::Bundled(name),
            text, size, line_height, letter_spacing, align, ..
        } = &source.content else { return Ok(None) };
        let font = crate::fonts::bundled(name).map_err(|error| error.to_string())?;
        let style = LegacyStyle {
            width: source.width, size: *size, line_height: *line_height,
            letter_spacing: *letter_spacing, align,
        };
        Ok(Some(Self::legacy(text, style, &font)?))
    }

    pub(super) fn new(text: &str, layout: TextLayout) -> Result<Self, String> {
        let count = text.chars().count();
        let lines = text.split('\n').count();
        if count > 4096 || lines > 96 {
            return Err("Canvas text exceeds the editable character or line limit".into());
        }
        let mut rows = Vec::new();
        let mut cells = Vec::new();
        let row_capacity = if layout == TextLayout::Paragraph { super::shaping::MAX_ROWS } else { lines };
        rows.try_reserve_exact(row_capacity).map_err(|_| "Text row allocation failed")?;
        cells.try_reserve_exact(count - (lines - 1)).map_err(|_| "Text caret allocation failed")?;
        if rows.capacity() > row_capacity || cells.capacity() > count - (lines - 1)
            || rows.capacity() * std::mem::size_of::<CaretRow>()
                + cells.capacity() * std::mem::size_of::<Cell>()
                + 1024 > Self::ADMISSION_BYTES
        {
            return Err("Text caret allocation exceeded its admission".into());
        }
        Ok(Self { rows, cells })
    }

    pub(super) fn legacy(text: &str, style: LegacyStyle<'_>, font: &impl Font) -> Result<Self, String> {
        let mut map = Self::new(text, TextLayout::Legacy)?;
        map.legacy_rows(text, style, font);
        Ok(map)
    }

    pub(super) fn legacy_rows(&mut self, text: &str, style: LegacyStyle<'_>, font: &impl Font) {
        for (row, line) in text.split('\n').enumerate() {
            let offset = self.cells.len();
            let (metrics, glyphs) = style.line(font, row, line);
            let mut graphemes = line.grapheme_indices(true).map(|(offset, _)| offset).peekable();
            for ((byte, _), (_, glyph, next)) in line.char_indices().zip(glyphs) {
                let x = glyph.position.x;
                if self.cells.len() > offset {
                    self.cells.last_mut().unwrap().edges[1] = x;
                }
                let grapheme_start = graphemes.next_if_eq(&byte).is_some();
                self.cells.push(Cell { edges: [x, next], grapheme_start });
            }
            self.finish_row(offset, metrics.start, metrics.vertical, false);
        }
    }

    pub(super) fn shaped_row(
        &mut self,
        line: &str,
        start: f64,
        vertical: [f32; 2],
        clusters: &mut [Cluster],
        soft_end: bool,
    ) {
        // The shaper visits clusters in visual order. The text buffer and all
        // selection ranges retain their logical character indices.
        clusters.sort_unstable_by_key(|cluster| cluster.bytes.start);
        let offset = self.cells.len();
        let mut end = 0;
        let mut x = start as f32;
        for cluster in clusters {
            let [left, right] = cluster.advance.map(|value| (start + value) as f32);
            let [before, after] = if cluster.rtl { [right, left] } else { [left, right] };
            self.cluster_cells(&line[end..cluster.bytes.start], [before; 2]);
            self.cluster_cells(&line[cluster.bytes.clone()], [before, after]);
            end = cluster.bytes.end;
            x = after;
        }
        self.cluster_cells(&line[end..], [x; 2]);
        let mut graphemes = line.grapheme_indices(true).map(|(offset, _)| offset).peekable();
        for (cell, (byte, _)) in self.cells[offset..].iter_mut().zip(line.char_indices()) {
            cell.grapheme_start = graphemes.next_if_eq(&byte).is_some();
        }
        self.finish_row(offset, start as f32, vertical, soft_end);
    }

    fn cluster_cells(&mut self, text: &str, [before, after]: [f32; 2]) {
        let graphemes = text.graphemes(true).count() as f32;
        for (i, group) in text.graphemes(true).enumerate() {
            let at = |part: usize| before + (after - before) * (part as f32 / graphemes);
            let start = at(i);
            let end = at(i + 1);
            for (j, _) in group.chars().enumerate() {
                self.cells.push(Cell {
                    edges: [if j == 0 { start } else { end }, end],
                    grapheme_start: j == 0,
                });
            }
        }
    }

    fn finish_row(&mut self, offset: usize, start: f32, vertical: [f32; 2], soft_end: bool) {
        let soft_start = self.rows.last().is_some_and(|row| row.soft_end);
        let first = self.rows.last().map_or(0, |row| row.indices.end() + usize::from(!soft_start));
        let end = first + self.cells.len() - offset;
        self.rows.push(CaretRow { indices: first..=end, vertical, offset, start, soft_start, soft_end });
    }

    pub(crate) fn nearest(&self, [x, y]: [f32; 2]) -> Caret {
        let row = self.rows.iter().min_by(|a, b| {
            let distance = |row: &CaretRow| (y - (row.vertical[0] + row.vertical[1]) * 0.5).abs();
            distance(a).total_cmp(&distance(b))
        }).expect("Text layout always has a row");
        self.closest(row, x)
    }

    fn closest(&self, row: &CaretRow, x: f32) -> Caret {
        self.stops(row).min_by(|&a, &b| {
            let [ax, aa] = self.edge(row, a);
            let [bx, ba] = self.edge(row, b);
            let inside = |a: f32, b: f32| (a.min(b)..=a.max(b)).contains(&x);
            (x - ax).abs().total_cmp(&(x - bx).abs())
                .then_with(|| inside(bx, ba).cmp(&inside(ax, aa)))
        }).expect("Text row always has a caret")
    }

    fn stops<'a>(&'a self, row: &'a CaretRow) -> impl Iterator<Item = Caret> + 'a {
        row.indices.clone()
            .filter(|&index| index == *row.indices.end() || self.cell(row, index).grapheme_start)
            .flat_map(|index| [Caret { index, trailing: false }, Caret { index, trailing: true }])
            .filter(|caret| {
                (!row.soft_start || caret.index != *row.indices.start() || !caret.trailing)
                    && (!row.soft_end || caret.index != *row.indices.end() || caret.trailing)
            })
    }

    fn row_index(&self, caret: Caret) -> Option<usize> {
        self.rows.iter().position(|row| {
            row.indices.contains(&caret.index)
                && (caret.trailing || !row.soft_end || caret.index != *row.indices.end())
        })
    }

    pub(crate) fn step(&self, from: Caret, motion: CaretMove, column: &mut Option<f32>) -> Caret {
        let vertical = matches!(motion, CaretMove::Up | CaretMove::Down);
        if !vertical {
            *column = None;
        }
        let Some(row_index) = self.row_index(from) else { return from };
        let row = &self.rows[row_index];
        let direction = match motion {
            CaretMove::Left | CaretMove::Up => -1,
            CaretMove::Right | CaretMove::Down => 1,
            CaretMove::LineStart => {
                return Caret { index: *row.indices.start(), trailing: false };
            }
            CaretMove::LineEnd => {
                return Caret {
                    index: *row.indices.end(),
                    trailing: row.indices.start() != row.indices.end(),
                };
            }
        };
        let x = self.x(row, from);
        let adjacent = row_index.checked_add_signed(direction).and_then(|index| self.rows.get(index));
        if vertical {
            let x = *column.get_or_insert(x);
            return adjacent.map_or(from, |row| self.closest(row, x));
        }
        let signed_distance = |candidate| (self.x(row, candidate) - x) * direction as f32;
        self.stops(row).filter(|&caret| signed_distance(caret) > 0.001)
            .min_by(|&a, &b| {
                signed_distance(a).total_cmp(&signed_distance(b)).then_with(|| {
                    let into = |caret| {
                        let [start, other] = self.edge(row, caret);
                        (other - start) * direction as f32 > 0.
                    };
                    into(b).cmp(&into(a))
                })
            })
            .or_else(|| adjacent.map(|row| {
                self.stops(row).min_by(|&a, &b| {
                    (self.x(row, a) * direction as f32).total_cmp(&(self.x(row, b) * direction as f32))
                }).expect("Text row always has a caret")
            }))
            .unwrap_or(from)
    }

    pub(crate) fn at(&self, caret: Caret) -> Option<(f32, [f32; 2])> {
        let row = &self.rows[self.row_index(caret)?];
        Some((self.x(row, caret), row.vertical))
    }

    pub(crate) fn visual_line(&self, caret: Caret) -> Option<Range<usize>> {
        let row = &self.rows[self.row_index(caret)?];
        Some(*row.indices.start()..*row.indices.end())
    }

    pub(crate) fn highlights(&self, selection: Range<usize>) -> impl Iterator<Item = [f32; 4]> + '_ {
        self.rows.iter().flat_map(move |row| {
            let first = selection.start.max(*row.indices.start());
            let last = selection.end.min(*row.indices.end());
            let newline = (!row.soft_end && first <= last && selection.end > *row.indices.end()).then(|| {
                let index = *row.indices.end();
                let end = self.x(row, Caret { index, trailing: true });
                let rtl = index > *row.indices.start() && {
                    let [a, b] = self.cell(row, index - 1).edges;
                    b < a
                };
                let width = (row.vertical[1] - row.vertical[0]) * 0.2;
                [end, if rtl { end - width } else { end + width }]
            });
            let mut segments = (first..last).map(move |index| self.cell(row, index).edges)
                .chain(newline)
                .map(|[a, b]| [a.min(b), a.max(b)])
                .filter(|[a, b]| a < b)
                .peekable();
            std::iter::from_fn(move || {
                let [mut left, mut right] = segments.next()?;
                while let Some(&[a, b]) = segments.peek() {
                    if a > right + 0.01 || b < left - 0.01 {
                        break;
                    }
                    left = left.min(a);
                    right = right.max(b);
                    segments.next();
                }
                Some([left, row.vertical[0], right, row.vertical[1]])
            })
        })
    }

    fn x(&self, row: &CaretRow, caret: Caret) -> f32 {
        self.edge(row, caret)[0]
    }

    fn edge(&self, row: &CaretRow, caret: Caret) -> [f32; 2] {
        let first = *row.indices.start();
        let last = *row.indices.end();
        if first == last {
            [row.start; 2]
        } else if caret.index == last || (caret.trailing && caret.index != first) {
            let [a, b] = self.cell(row, caret.index - 1).edges;
            [b, a]
        } else {
            self.cell(row, caret.index).edges
        }
    }

    fn cell(&self, row: &CaretRow, index: usize) -> Cell {
        self.cells[row.offset + index - row.indices.start()]
    }
}
