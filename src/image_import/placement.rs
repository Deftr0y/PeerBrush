use serde_json::Value;
use std::{borrow::Cow, path::Path};

pub(crate) struct Placement<'a> {
    pub parent: Option<&'a str>,
    pub origin: [i32; 2],
    pub name: Cow<'a, str>,
}
impl<'a> Placement<'a> {
    pub(crate) fn new(command: &'a Value) -> Result<Self, String> {
        let parent = command
            .get("parent")
            .filter(|p| !p.is_null())
            .map(|v| v.as_str().ok_or("Image parent must be a folder ID"))
            .transpose()?;
        let coordinate = |key: &str| {
            command.get(key).map_or(Ok(0), |v| {
                v.as_i64()
                    .filter(|n| (-100000..=100000).contains(n))
                    .map(|n| n as i32)
                    .ok_or_else(|| format!("Invalid image {key}"))
            })
        };
        let origin = [coordinate("x")?, coordinate("y")?];
        let path = Path::new(command["path"].as_str().ok_or("Missing image path")?);
        let name = command["name"]
            .as_str()
            .map(Cow::Borrowed)
            .unwrap_or_else(|| path.file_stem().unwrap_or_default().to_string_lossy());
        Ok(Self {
            parent,
            origin,
            name,
        })
    }
}
