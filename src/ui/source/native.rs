//! Owned native editor metadata. Mutable draft bytes and command copies are
//! admitted before allocation; command consumers retain the Admitted owner.
use crate::ui::native_dispatch::SharedCommand;
use crate::{
    engine::{
        native::{DocumentKey, NativeDocument},
        scene::{Content36, Source36},
    },
    fonts::{InstalledFont, TextFont},
    large_document::{Reservation, ResidentBudget},
    psd::memory36::Admitted,
    source::{Content, Source},
};
use serde_json::Value;

const DRAFT_BYTES: usize = 4 * 1024 * 1024;
const COMMAND_WORK_BYTES: usize = 4 * 1024 * 1024;

pub(super) struct Native {
    pub key: DocumentKey,
    pub(super) error: Option<String>,
    pub(super) widgets: Option<super::widgets::Widgets>,
    original: Option<Source36>,
    draft_taken: bool,
    command: Option<([u8; 32], SharedCommand)>,
    pub(super) pool: ResidentBudget,
    // Editor declares its actual Source payload before this Native field.
    pub(super) draft: Reservation,
}
impl Native {
    pub fn new(doc: &NativeDocument, original: Option<&Source36>) -> Result<Self, String> {
        let pool = doc.budget().clone();
        let draft = pool
            .reserve_bytes(DRAFT_BYTES, DRAFT_BYTES)
            .map_err(|e| e.to_string())?;
        let original = original
            .map(|source| -> Result<Source36, String> {
                if !source.content.uses_budget(&pool) {
                    return Err("Source editor requires the actual admitted metadata owner".into());
                }
                Ok(Source36 {
                    width: source.width,
                    height: source.height,
                    matrix: source.matrix,
                    content: source.content.shared()?,
                })
            })
            .transpose()?;
        Ok(Self {
            key: doc.key(),
            error: None,
            widgets: None,
            original,
            draft_taken: false,
            command: None,
            pool,
            draft,
        })
    }
    pub fn pool(&self) -> &ResidentBudget {
        &self.pool
    }
    pub fn original_draft(&mut self) -> Result<Option<Source>, String> {
        if self.draft_taken {
            return Err("Native source draft admission was already consumed".into());
        }
        self.draft_taken = true;
        let Some(source) = &self.original else {
            return Ok(None);
        };
        let mut allocation = Copy { used: 4096 };
        let content = match source.definition() {
            Content36::Bezier {
                nodes,
                closed,
                fill,
                stroke,
                stroke_width,
            } => Content::Bezier {
                nodes: allocation.vector(nodes, 128)?,
                closed: *closed,
                fill: *fill,
                stroke: *stroke,
                stroke_width: *stroke_width,
            },
            Content36::Shape {
                shape,
                bounds,
                points,
                closed,
                fill,
                stroke,
                stroke_width,
            } => Content::Shape {
                shape: allocation.text(shape, 32)?,
                bounds: *bounds,
                points: allocation.vector(points, 128)?,
                closed: *closed,
                fill: *fill,
                stroke: *stroke,
                stroke_width: *stroke_width,
            },
            Content36::Svg { svg } => Content::Svg {
                svg: allocation.text(svg, crate::svg::MARKUP_LIMIT)?,
            },
            Content36::Text {
                text,
                layout,
                font,
                size,
                line_height,
                letter_spacing,
                align,
                color,
            } => Content::Text {
                text: allocation.text(text, 16384)?,
                layout: *layout,
                font: allocation.font(font)?,
                size: *size,
                line_height: *line_height,
                letter_spacing: *letter_spacing,
                align: allocation.text(align, 16)?,
                color: *color,
            },
        };
        Ok(Some(Source {
            width: source.width,
            height: source.height,
            matrix: source.matrix,
            content,
        }))
    }
    /// The builder is the bounded source command serializer, never arbitrary
    /// input. Its temporary Value drops before the working reservation.
    pub fn command(
        &mut self,
        doc: &NativeDocument,
        signature: [u8; 32],
        build: impl FnOnce() -> Value,
    ) -> Result<Admitted<Value>, String> {
        if self.key != doc.key() || !self.pool.same_pool(doc.budget()) {
            return Err("Source editor command belongs to an older native frame".into());
        }
        if let Some((key, command)) = &self.command {
            if *key == signature {
                return Ok(command.value().clone());
            }
        }
        let working = self
            .pool
            .reserve_bytes(COMMAND_WORK_BYTES, COMMAND_WORK_BYTES)
            .map_err(|e| e.to_string())?;
        let value = build();
        let command = doc.copy_value_metadata(&value, &|| Ok(()))?;
        drop(value);
        drop(working);
        self.command = Some((signature, SharedCommand::new(command.clone())?));
        Ok(command)
    }
    pub fn owned_command(&self, signature: [u8; 32]) -> Option<SharedCommand> {
        self.command
            .as_ref()
            .filter(|(key, _)| *key == signature)
            .map(|(_, value)| value.clone())
    }
}
pub(super) fn prepare_buffers(source: &mut Source, search: &mut String) -> Result<(), String> {
    fn capacity(text: &mut String, limit: usize) -> Result<(), String> {
        if text.len() > limit {
            return Err("Source text exceeds its editable native bound".into());
        }
        if text.capacity() < limit {
            text.try_reserve_exact(limit - text.len())
                .map_err(|_| "Source text buffer allocation refused")?;
        }
        if text.capacity() > limit {
            return Err("Source text buffer exceeds its admitted capacity".into());
        }
        Ok(())
    }
    capacity(search, 512)?;
    match &mut source.content {
        Content::Svg { svg } => capacity(svg, crate::svg::MARKUP_LIMIT)?,
        Content::Text { text, align, .. } => {
            capacity(text, 16384)?;
            capacity(align, 16)?;
        }
        Content::Shape { points, .. } => {
            if points.len() > 128 {
                return Err("Source points exceed their editable bound".into());
            }
            if points.capacity() < 128 {
                points
                    .try_reserve_exact(128 - points.len())
                    .map_err(|_| "Source point allocation refused")?;
            }
            if points.capacity() > 128 {
                return Err("Source points exceed their admitted capacity".into());
            }
        }
        Content::Bezier { .. } => {}
    }
    Ok(())
}

/// All supported definition bodies have bounded native metadata:128 anchors or
/// points,16384 text bytes,512KiB SVG. Reserve the full mutable capacity before
/// allocation so TextBuffer and point edits need no implicit capacity growth.
struct Copy {
    used: usize,
}
impl Copy {
    fn admit(&mut self, bytes: usize) -> Result<(), String> {
        self.used = self
            .used
            .checked_add(bytes)
            .filter(|n| *n <= DRAFT_BYTES)
            .ok_or("Native source draft exceeds its admitted capacity")?;
        Ok(())
    }
    fn text(&mut self, value: &str, capacity: usize) -> Result<String, String> {
        if value.len() > capacity {
            return Err("Native source string exceeds its edit-buffer bound".into());
        }
        self.admit(capacity)?;
        let mut result = String::new();
        result
            .try_reserve_exact(capacity)
            .map_err(|_| "Native source string allocation refused")?;
        if result.capacity() > capacity {
            return Err("Native source string capacity exceeded admission".into());
        }
        result.push_str(value);
        Ok(result)
    }
    fn vector<T: Clone>(&mut self, value: &[T], capacity: usize) -> Result<Vec<T>, String> {
        if value.len() > capacity {
            return Err("Native source geometry exceeds its editable bound".into());
        }
        self.admit(
            capacity
                .checked_mul(std::mem::size_of::<T>())
                .ok_or("Native source geometry capacity overflow")?,
        )?;
        let mut result = Vec::new();
        result
            .try_reserve_exact(capacity)
            .map_err(|_| "Native source geometry allocation refused")?;
        if result.capacity() > capacity {
            return Err("Native source geometry capacity exceeded admission".into());
        }
        result.extend_from_slice(value);
        Ok(result)
    }
    fn font(&mut self, value: &TextFont) -> Result<TextFont, String> {
        value.validate().map_err(|e| e.to_string())?;
        Ok(match value {
            TextFont::Bundled(name) => TextFont::Bundled(self.text(name, 16)?),
            TextFont::Installed(font) => TextFont::Installed(InstalledFont {
                family: self.text(&font.family, 512)?,
                postscript: self.text(&font.postscript, 512)?,
                style: self.text(&font.style, 16)?,
                weight: font.weight,
                stretch: font.stretch,
                index: font.index,
                sha256: self.text(&font.sha256, 64)?,
            }),
        })
    }
}
