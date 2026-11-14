//! Scalar presentation metadata. No edit command or editable candidate escapes.
use super::*;
use crate::engine::native::{DocumentKey, NativeDocument};

/// Derived display identity only; this grants no raster or publication authority.
pub(crate) fn display_binding() -> DocumentKey {
    crate::engine::native::key()
}

struct Pose {
    layer: String,
    origin: [i32; 2],
    opacity: f32,
    color: [u8; 4],
}
pub(crate) struct Presentation {
    base: DocumentKey,
    poses: Admitted<Vec<Pose>>,
}
impl Presentation {
    pub(crate) fn capture(
        before: &NativeDocument,
        after: &NativeDocument,
        t: f32,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        check()?;
        let a = before.metadata();
        let b = after.metadata();
        if !t.is_finite()
            || !(0. ..=1.).contains(&t)
            || a.id() != b.id()
            || a.revision() >= b.revision()
            || a.depth() != b.depth()
            || !before.budget().same_pool(after.budget())
            || !before.disk_budget().same_pool(after.disk_budget())
        {
            return Err("Animation differs from its native source transition".into());
        }
        let count = b.layers().len();
        if count > 100 {
            return Err("Animation supports at most 100 layers".into());
        }
        let bytes = b.layers().try_fold(
            count * (std::mem::size_of::<Pose>() + 32) * 2 + 1024,
            |sum, layer| {
                sum.checked_add(layer.id().len())
                    .ok_or("Animation pose metadata overflow")
            },
        )?;
        if bytes > 64 * 1024 * 1024 {
            return Err("Animation pose metadata exceeds admission".into());
        }
        let charge = MetadataCharge::reserve(after.budget(), bytes, check)?;
        let mut poses = Vec::new();
        poses
            .try_reserve_exact(count)
            .map_err(|_| "Animation pose allocation refused")?;
        let lerp = |a: f32, b: f32| a + (b - a) * t;
        for layer in b.layers() {
            check()?;
            let Some(old) = a.layers().find(|old| old.id() == layer.id()) else {
                continue;
            };
            if old.origin() == layer.origin()
                && old.opacity() == layer.opacity()
                && old.color() == layer.color()
            {
                continue;
            }
            poses.push(Pose {
                layer: layer.id().into(),
                origin: std::array::from_fn(|i| {
                    lerp(old.origin()[i] as f32, layer.origin()[i] as f32).round() as i32
                }),
                opacity: lerp(old.opacity(), layer.opacity()),
                color: std::array::from_fn(|i| {
                    lerp(old.color()[i] as f32, layer.color()[i] as f32).round() as u8
                }),
            });
        }
        Ok(Self {
            base: after.key(),
            poses: Admitted::from_parts(Arc::new(poses), charge),
        })
    }
    pub(crate) fn signature(&self) -> [u8; 32] {
        let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
        hash.update(b"PeerBrush presentation poses v1\0");
        for pose in self.poses.iter() {
            hash.update(&(pose.layer.len() as u64).to_be_bytes());
            hash.update(pose.layer.as_bytes());
            for coordinate in pose.origin {
                hash.update(&coordinate.to_be_bytes());
            }
            hash.update(&pose.opacity.to_bits().to_be_bytes());
            hash.update(&pose.color);
        }
        hash.finish().as_ref().try_into().unwrap()
    }
    pub(crate) fn matches(&self, doc: &NativeDocument) -> bool {
        self.base == doc.key() && self.poses.owner().uses_budget(doc.budget())
    }
    pub(crate) fn scene(
        &self,
        source: &SceneSnapshot,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<SceneSnapshot> {
        let base = source.native_document()?;
        if !self.matches(base)
            || !source.runtime_views.structure.is_empty()
            || !source.runtime_views.patterns.is_empty()
        {
            return Err("Animation source changed before presentation".into());
        }
        let metadata = base
            .copy_render_metadata(check)?
            .map_unique(|mut metadata, _| {
                for pose in self.poses.iter() {
                    check()?;
                    let layer = metadata
                        .layers
                        .iter_mut()
                        .find(|layer| layer.id == pose.layer)
                        .ok_or("Animation layer disappeared")?;
                    [layer.x, layer.y] = pose.origin;
                    layer.opacity = pose.opacity;
                    layer.color = pose.color;
                }
                Ok(metadata)
            })?;
        let charge = native_scene_header(source.identity(), base.budget())?;
        check()?;
        Ok(SceneSnapshot {
            identity: source.identity.clone(),
            leases: source.leases.clone(),
            file_version: source.file_version,
            _file_ownership: source._file_ownership.clone(),
            runtime_views: RuntimeViews::default(),
            document: SnapshotDocument::Presentation {
                base: base.clone(),
                metadata,
            },
            binding: crate::engine::native::key(),
            _lease_charge: source._lease_charge.clone(),
            _header_charge: Some(charge),
        })
    }
}
impl SceneSnapshot {
    /// The only derived scene accepted by a memory-only read permit preserves
    /// the exact source roster and changes presentation scalars only.
    pub(crate) fn read_source_key(&self) -> DocumentKey {
        match &self.document {
            SnapshotDocument::Presentation { base, .. } => base.key(),
            _ => self.key(),
        }
    }
}
