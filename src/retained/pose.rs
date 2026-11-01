//! Charged retained placement metadata. Pixel and publication owners are separate.
use crate::{
    engine::native::RetainedMetadata,
    large_document::{Reservation, ResidentBudget},
    projective::Map,
    warp::{Grid, Placement, Stage, MAX_STAGES},
};

pub(crate) struct Pose {
    pub frame: [u32; 2],
    pub depth: u16,
    pub matrix: [f64; 6],
    pub projective: Option<[f64; 9]>,
    warps: Vec<Stage>,
    budget: ResidentBudget,
    _charge: Reservation,
}
impl Pose {
    pub(crate) fn new(
        frame: [u32; 2],
        depth: u16,
        original: Option<RetainedMetadata<'_>>,
        budget: &ResidentBudget,
    ) -> Result<Self, String> {
        let source = original.map_or(&[][..], |o| o.warps());
        if source.len() > MAX_STAGES {
            return Err("Retained Warp stage limit exceeded".into());
        }
        let charge = budget
            .reserve_bytes(16 * 1024, 16 * 1024)
            .map_err(|e| e.to_string())?;
        let mut warps = Vec::new();
        warps
            .try_reserve_exact(MAX_STAGES)
            .map_err(|_| "Native retained placement allocation refused")?;
        if warps
            .capacity()
            .checked_mul(std::mem::size_of::<Stage>())
            .is_none_or(|n| n > charge.bytes())
        {
            return Err("Native retained placement exceeds metadata admission".into());
        }
        for stage in source {
            warps.push(Stage {
                before: stage.before,
                grid: Grid {
                    rect: stage.grid.rect,
                    points: stage.grid.points,
                },
            });
        }
        let pose = Self {
            frame,
            depth,
            matrix: original.map_or(super::IDENTITY, |o| o.matrix()),
            projective: original.and_then(|o| o.projective()),
            warps,
            budget: budget.clone(),
            _charge: charge,
        };
        pose.validate()?;
        Ok(pose)
    }
    pub(crate) fn warps(&self) -> &[Stage] {
        &self.warps
    }
    pub(crate) fn validate(&self) -> Result<(), String> {
        let _prepared = self
            .budget
            .reserve_bytes(64 * 1024, 64 * 1024)
            .map_err(|e| e.to_string())?;
        super::geometry::validate(
            self.frame[0],
            self.frame[1],
            self.depth,
            self.depth,
            self.matrix,
            self.projective,
            &self.warps,
        )
    }
    pub(crate) fn placement(&self) -> Result<Map, String> {
        super::geometry::placement(self.matrix, self.projective)
    }
    pub(crate) fn prepend(&mut self, delta: Map) -> Result<(), String> {
        let map = delta.compose(self.placement()?)?;
        if let Some(matrix) = map.as_affine() {
            self.matrix = matrix;
            self.projective = None;
        } else {
            self.matrix = super::IDENTITY;
            self.projective = Some(map.matrix());
        }
        self.validate()
    }
    pub(crate) fn append_warp(
        &mut self,
        grid: &Grid,
        frame: Map,
        after: Map,
    ) -> Result<(), String> {
        if self.warps.len() >= MAX_STAGES {
            return Err("Retained Warp stage limit reached; undo an earlier Warp or cancel".into());
        }
        let before = frame.compose(self.placement()?)?.matrix();
        self.warps.push(Stage {
            before,
            grid: grid.clone(),
        });
        self.matrix = super::IDENTITY;
        self.projective = None;
        self.prepend(after)
    }
    pub(crate) fn bounds(&self, source: [i32; 4]) -> Result<[f64; 4], String> {
        self.validate()?;
        let _prepared = self
            .budget
            .reserve_bytes(64 * 1024, 64 * 1024)
            .map_err(|e| e.to_string())?;
        super::geometry::bounds(source, self.matrix, self.projective, &self.warps)
    }
    pub(crate) fn support_bounds_in(&self, frame: Map) -> Result<[f64; 4], String> {
        self.validate()?;
        let _prepared = self
            .budget
            .reserve_bytes(64 * 1024, 64 * 1024)
            .map_err(|e| e.to_string())?;
        Placement::new(&self.warps, frame.compose(self.placement()?)?)?.bounds([
            -1.,
            -1.,
            self.frame[0] as f64 + 1.,
            self.frame[1] as f64 + 1.,
        ])
    }
}
