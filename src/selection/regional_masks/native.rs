//! Raw native mask evaluation bound to one immutable document generation.
use super::*;
use crate::{
    engine::native::{LayerMetadata, NativeDocument, RasterSlot},
    psd::memory36::{Admitted, MetadataCharge},
};
use std::sync::Arc;

pub(crate) struct Native<'a> {
    document: &'a NativeDocument,
    layer: LayerMetadata<'a>,
    key: RasterSlot,
    identity: Admitted<SceneIdentity>,
}
impl<'a> Native<'a> {
    pub(crate) fn identity_owner(&self) -> Admitted<SceneIdentity> {
        self.identity.clone()
    }
    pub(crate) fn new(
        document: &'a NativeDocument,
        layer: LayerMetadata<'a>,
        check: &dyn Fn() -> std::result::Result<(), String>,
    ) -> std::result::Result<Self, String> {
        let key = document.layer_slot(layer.id())?;
        if key != layer.pixels().slot() {
            return Err("Mask layer differs from its captured document".into());
        }
        let charge = MetadataCharge::reserve(
            document.budget(),
            document.metadata().id().len() + 1024,
            check,
        )?;
        let identity = SceneIdentity {
            // Worker capture grants document authority; it carries no project grant.
            project_id: String::new(),
            document_id: document.metadata().id().into(),
            revision: document.metadata().revision(),
        };
        Ok(Self {
            document,
            layer,
            key,
            identity: Admitted::from_parts(Arc::new(identity), charge),
        })
    }
}
pub(crate) struct PreparedNative {
    identity: Admitted<SceneIdentity>,
    key: RasterSlot,
    frame: Frame,
    region: Region,
    layer_id: Admitted<String>,
    mask: Option<crate::mask::region::RegionalMask>,
    resident: ResidentBudget,
}
impl Source for Native<'_> {
    type Key = RasterSlot;
    type Prepared = PreparedNative;
    fn identity(&self) -> &SceneIdentity {
        &self.identity
    }
    fn layer_id(&self) -> &str {
        self.layer.id()
    }
    fn source_key(&self) -> &Self::Key {
        &self.key
    }
    fn frame(&self) -> Frame {
        Frame {
            width: self.layer.pixels().width(),
            height: self.layer.pixels().height(),
            depth: self.layer.pixels().depth(),
            origin: self.layer.origin(),
        }
    }
    fn has_mask(&self) -> bool {
        self.layer.mask().is_some()
    }
    fn uses_budgets(&self, resident: &ResidentBudget, disk: &DiskBudget) -> bool {
        self.document.budget().same_pool(resident) && self.document.disk_budget().same_pool(disk)
    }
    fn prepare_region(
        &mut self,
        region: Region,
        check: &dyn Fn() -> Result<()>,
    ) -> std::result::Result<Self::Prepared, String> {
        check().map_err(|e| e.to_string())?;
        let b = region.bounds();
        let frame = self.frame();
        if b[0] < 0 || b[1] < 0 || b[2] > frame.width as i32 || b[3] > frame.height as i32 {
            return Err("Mask footprint exceeds its native layer frame".into());
        }
        let checked = || check().map_err(|e| e.to_string());
        let owner = MetadataCharge::reserve(
            self.document.budget(),
            self.layer.id().len() + 512,
            &checked,
        )?;
        let layer_id = Admitted::from_parts(Arc::new(self.layer.id().to_owned()), owner);
        let mask = if self.has_mask() {
            let mut bounds = b;
            for (i, v) in bounds.iter_mut().enumerate() {
                *v = v
                    .checked_add(frame.origin[i % 2])
                    .ok_or("Mask footprint coordinate overflow")?;
            }
            Some(crate::mask::region::prepare_native(
                self.document,
                self.layer,
                bounds,
                &mut || checked(),
            )?)
        } else {
            None
        };
        Ok(PreparedNative {
            identity: self.identity.clone(),
            key: self.key,
            frame,
            region,
            layer_id,
            mask,
            resident: self.document.budget().clone(),
        })
    }
}
impl Prepared for PreparedNative {
    type Key = RasterSlot;
    fn identity(&self) -> &SceneIdentity {
        &self.identity
    }
    fn layer_id(&self) -> &str {
        &self.layer_id
    }
    fn source_key(&self) -> &Self::Key {
        &self.key
    }
    fn frame(&self) -> Frame {
        self.frame
    }
    fn region(&self) -> Region {
        self.region
    }
    fn uses_budget(&self, budget: &ResidentBudget) -> bool {
        self.resident.same_pool(budget)
            && self
                .mask
                .as_ref()
                .is_none_or(|mask| mask.uses_budget(budget))
    }
    fn value8(&self, x: i32, y: i32) -> std::result::Result<f32, String> {
        self.check_point(x, y)?;
        self.mask.as_ref().map_or(Ok(1.), |mask| mask.value8(x, y))
    }
    fn value16(&self, x: i32, y: i32) -> std::result::Result<f64, String> {
        self.check_point(x, y)?;
        self.mask.as_ref().map_or(Ok(1.), |mask| mask.value16(x, y))
    }
}
impl PreparedNative {
    fn check_point(&self, x: i32, y: i32) -> std::result::Result<(), String> {
        let [l, t, r, b] = self.region.bounds();
        if x < l || x >= r || y < t || y >= b {
            return Err("Mask sample exceeds its prepared native footprint".into());
        }
        Ok(())
    }
}
