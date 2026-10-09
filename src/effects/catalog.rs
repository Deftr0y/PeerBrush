//! Shared effect discovery for the engine, native picker and protocol clients.
use serde_json::{json, Value};

pub const CATEGORIES: &[&str] = &["Tone", "Color", "Blur & light", "Distortion", "Mask"];

pub struct Entry {
    pub kind: &'static str,
    pub name: &'static str,
    pub category: &'static str,
    pub color: bool,
    pub mask_name: Option<&'static str>,
    pub keywords: &'static str,
}
impl Entry {
    pub fn name(&self, mask: bool) -> &'static str {
        if mask {
            self.mask_name.unwrap_or(self.name)
        } else {
            self.name
        }
    }
    pub fn supports(&self, mask: bool) -> bool {
        if mask {
            self.mask_name.is_some()
        } else {
            self.color
        }
    }
    pub fn matches(&self, mask: bool, query: &str) -> bool {
        if !self.supports(mask) {
            return false;
        }
        let haystack = format!(
            "{} {} {} {}",
            self.name(mask),
            self.kind,
            self.category,
            self.keywords
        )
        .to_lowercase();
        query
            .split_whitespace()
            .all(|word| haystack.contains(&word.to_lowercase()))
    }
}
pub const ENTRIES: &[Entry] = &[
    Entry {
        kind: "levels",
        name: "Levels",
        category: "Tone",
        color: true,
        mask_name: Some("Levels"),
        keywords: "gamma black white",
    },
    Entry {
        kind: "curves",
        name: "Curves",
        category: "Tone",
        color: true,
        mask_name: Some("Curves"),
        keywords: "contrast tonal",
    },
    Entry {
        kind: "adjust",
        name: "Color adjustment",
        category: "Tone",
        color: true,
        mask_name: Some("Color adjustment"),
        keywords: "brightness contrast saturation",
    },
    Entry {
        kind: "invert",
        name: "Invert",
        category: "Tone",
        color: true,
        mask_name: Some("Invert"),
        keywords: "negative",
    },
    Entry {
        kind: "color_balance",
        name: "Color balance",
        category: "Color",
        color: true,
        mask_name: None,
        keywords: "shadows midtones highlights",
    },
    Entry {
        kind: "hsl",
        name: "Hue / saturation",
        category: "Color",
        color: true,
        mask_name: None,
        keywords: "lightness hue shift",
    },
    Entry {
        kind: "grayscale",
        name: "Grayscale",
        category: "Color",
        color: true,
        mask_name: None,
        keywords: "black white desaturate",
    },
    Entry {
        kind: "blur",
        name: "Gaussian blur",
        category: "Blur & light",
        color: true,
        mask_name: Some("Feather"),
        keywords: "soften radius",
    },
    Entry {
        kind: "gaussian",
        name: "Gaussian blur",
        category: "Blur & light",
        color: false,
        mask_name: Some("Gaussian blur"),
        keywords: "soften radius",
    },
    Entry {
        kind: "bloom",
        name: "Bloom",
        category: "Blur & light",
        color: true,
        mask_name: None,
        keywords: "glow spread highlights",
    },
    Entry {
        kind: "liquify",
        name: "Liquify",
        category: "Distortion",
        color: true,
        mask_name: None,
        keywords: "warp push pinch bloat",
    },
    Entry {
        kind: "paint",
        name: "Paint",
        category: "Mask",
        color: false,
        mask_name: Some("Paint"),
        keywords: "brush grayscale",
    },
    Entry {
        kind: "fill",
        name: "Fill",
        category: "Mask",
        color: false,
        mask_name: Some("Fill"),
        keywords: "coverage value",
    },
];
pub fn get(kind: &str) -> Option<&'static Entry> {
    ENTRIES.iter().find(|entry| entry.kind == kind)
}
pub fn discovery() -> Value {
    json!({"version":1,"categories":CATEGORIES,"entries":ENTRIES.iter().map(|entry| {
        let mut stacks = vec![];
        if entry.color {stacks.push("color");}
        if entry.mask_name.is_some() {stacks.push("mask");}
        json!({"kind":entry.kind,"name":entry.name,"mask_name":entry.mask_name,
            "category":entry.category,"stacks":stacks,"keywords":entry.keywords})
    }).collect::<Vec<_>>()})
}
