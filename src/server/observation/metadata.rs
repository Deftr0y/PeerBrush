//! Borrowed observation DTOs preserve editable definitions without Raster copies.
use crate::engine::{
    native::{
        CoverageMetadata, EffectMetadata, LayerMetadata, MaskMetadata, MaskStepMetadata,
        RasterMetadata,
    },
    scene::{Content36, SceneSnapshot, Source36},
};
use serde::{
    ser::{SerializeMap, SerializeSeq},
    Serialize, Serializer,
};
use serde_json::Value;

macro_rules! fields {($map:ident,$($key:literal=>$value:expr),*$(,)?)=>{$($map.serialize_entry($key,&$value)?;)*};}
pub(crate) struct Document<'a> {
    pub scene: &'a SceneSnapshot,
    pub profile: &'a Value,
}
impl Serialize for Document<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let d = self.scene.metadata();
        let mut map = s.serialize_map(None)?;
        let polygon = d
            .selection_polygon()
            .filter(|p| crate::selection::bounds(p).ok() == d.selection());
        fields!(map,"id"=>d.id(),"name"=>d.name(),"width"=>d.width(),"height"=>d.height(),"revision"=>d.revision(),"bit_depth"=>d.depth(),"color_profile"=>self.profile,"read_only"=>d.read_only(),"warnings"=>d.warnings(),"source_structure"=>self.scene.source_structure(),"source_patterns"=>self.scene.source_patterns(),"filters"=>Filters(self.scene),"selection"=>d.selection(),"selection_polygon"=>polygon);
        map.end()
    }
}
pub(crate) struct Layers<'a>(pub &'a SceneSnapshot);
impl Serialize for Layers<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let layers = self.0.metadata().layers();
        let mut seq = s.serialize_seq(Some(layers.len()))?;
        for layer in layers {
            seq.serialize_element(&Layer(layer))?;
        }
        seq.end()
    }
}
struct Layer<'a>(LayerMetadata<'a>);
impl Serialize for Layer<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let l = self.0;
        let p = l.pixels();
        let [x, y] = l.origin();
        let mut map = s.serialize_map(None)?;
        fields!(map,"id"=>l.id(),"name"=>l.name(),"kind"=>l.kind(),"parent"=>l.parent(),"clip_to"=>l.clip_to(),"visible"=>l.visible(),"locked"=>l.locked(),"opacity"=>l.opacity(),"blend"=>l.blend(),"bounds"=>[x,y,x+p.width() as i32,y+p.height() as i32],"effects"=>Effects(l),"source"=>l.source().map(Source),"path_document_nodes"=>Path(l),"transform_source"=>Retained(p),"mask"=>l.mask().map(Mask));
        map.end()
    }
}
struct Filters<'a>(&'a SceneSnapshot);
impl Serialize for Filters<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let items = self.0.metadata().filters();
        let mut seq = s.serialize_seq(Some(items.len()))?;
        for item in items {
            seq.serialize_element(&Effect(item))?;
        }
        seq.end()
    }
}
struct Effects<'a>(LayerMetadata<'a>);
impl Serialize for Effects<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let items = self.0.effects();
        let mut seq = s.serialize_seq(Some(items.len()))?;
        for item in items {
            seq.serialize_element(&Effect(item))?;
        }
        seq.end()
    }
}
struct Effect<'a>(EffectMetadata<'a>);
impl Serialize for Effect<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let e = self.0;
        let mut map = s.serialize_map(None)?;
        fields!(map,"id"=>e.id(),"kind"=>e.kind(),"enabled"=>e.enabled(),"weight"=>e.weight(),"settings"=>e.settings());
        if e.clips().len() > 0 {
            map.serialize_entry("native_coverage_clips", &Clips(e))?;
        }
        map.end()
    }
}
struct Clips<'a>(EffectMetadata<'a>);
impl Serialize for Clips<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let clips = self.0.clips();
        let mut seq = s.serialize_seq(Some(clips.len()))?;
        for (stroke, coverage) in clips {
            #[derive(Serialize)]
            struct Clip<'a> {
                stroke: u32,
                coverage: Coverage<'a>,
            }
            seq.serialize_element(&Clip {
                stroke,
                coverage: Coverage(coverage),
            })?;
        }
        seq.end()
    }
}
struct Coverage<'a>(CoverageMetadata<'a>);
impl Serialize for Coverage<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let c = self.0;
        let p = c.mask();
        let mut map = s.serialize_map(None)?;
        fields!(map,"bounds"=>c.bounds(),"origin"=>c.origin(),"contours"=>c.contours(),"storage_kind"=>"source-backed","source_slot"=>p.ordinal(),"width"=>p.width(),"height"=>p.height(),"bit_depth"=>p.depth(),"transform_source"=>Retained(p));
        map.end()
    }
}
struct Mask<'a>(MaskMetadata<'a>);
impl Serialize for Mask<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(None)?;
        fields!(map,"enabled"=>self.0.enabled(),"linked"=>self.0.linked(),"steps"=>Steps(self.0));
        map.end()
    }
}
struct Steps<'a>(MaskMetadata<'a>);
impl Serialize for Steps<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let items = self.0.steps();
        let mut seq = s.serialize_seq(Some(items.len()))?;
        for step in items {
            seq.serialize_element(&Step(step))?;
        }
        seq.end()
    }
}
struct Step<'a>(MaskStepMetadata<'a>);
impl Serialize for Step<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let v = self.0;
        let mut map = s.serialize_map(None)?;
        fields!(map,"id"=>v.id(),"kind"=>v.kind(),"enabled"=>v.enabled(),"weight"=>v.weight(),"value"=>v.value(),"settings"=>v.settings(),"transform_source"=>Retained(v.pixels()));
        map.end()
    }
}
struct Retained<'a>(RasterMetadata<'a>);
impl Serialize for Retained<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let Some(r) = self.0.retained() else {
            return s.serialize_none();
        };
        let p = r.pixels();
        let logical = u64::from(p.width()) * u64::from(p.height()) * 4 * u64::from(p.depth() / 8);
        let mut map = s.serialize_map(None)?;
        fields!(map,"width"=>p.width(),"height"=>p.height(),"bit_depth"=>p.depth(),"matrix"=>r.matrix(),"bytes"=>Option::<u64>::None,"storage_kind"=>"source-backed","native_logical_bytes"=>logical);
        if let Some(matrix) = r.projective() {
            map.serialize_entry("projective", &matrix)?;
        }
        if !r.warps().is_empty() {
            map.serialize_entry("warp_stages", r.warps())?;
        }
        map.end()
    }
}
struct Source<'a>(&'a Source36);
impl Serialize for Source<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(None)?;
        fields!(map,"width"=>self.0.width,"height"=>self.0.height,"matrix"=>self.0.matrix,"content"=>Content(self.0.definition()));
        map.end()
    }
}
struct Content<'a>(&'a Content36);
impl Serialize for Content<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let Content36::Text {
            text,
            layout,
            font,
            size,
            line_height,
            letter_spacing,
            align,
            color,
        } = self.0
        else {
            return self.0.serialize(s);
        };
        let mut map = s.serialize_map(None)?;
        fields!(map,"kind"=>"text","text"=>text,"font"=>font,"size"=>size,"line_height"=>line_height,"align"=>align,"color"=>color);
        if *layout != crate::source::TextLayout::Legacy {
            map.serialize_entry("layout", layout)?;
        }
        if *letter_spacing != 0. {
            map.serialize_entry("letter_spacing", letter_spacing)?;
        }
        map.end()
    }
}
struct Path<'a>(LayerMetadata<'a>);
impl Serialize for Path<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let Some(source) = self.0.source() else {
            return s.serialize_none();
        };
        let Content36::Bezier { nodes, .. } = source.definition() else {
            return s.serialize_none();
        };
        let [x, y] = self.0.origin();
        let m = source.matrix;
        let point = |p: [f32; 2]| {
            [
                m[0] * p[0] + m[2] * p[1] + m[4] + x as f32,
                m[1] * p[0] + m[3] * p[1] + m[5] + y as f32,
            ]
        };
        let mut seq = s.serialize_seq(Some(nodes.len()))?;
        for (index, node) in nodes.iter().enumerate() {
            #[derive(Serialize)]
            struct Node {
                index: usize,
                point: [f32; 2],
                in_handle: Option<[f32; 2]>,
                out_handle: Option<[f32; 2]>,
                smooth: bool,
            }
            seq.serialize_element(&Node {
                index,
                point: point(node.point),
                in_handle: node.in_handle.map(point),
                out_handle: node.out_handle.map(point),
                smooth: node.smooth,
            })?;
        }
        seq.end()
    }
}
