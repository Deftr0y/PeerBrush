//! Review tags retain only admitted identifiers and a pending-record proof.
use super::*;
use crate::collaboration::native::{self, Approval};

pub(super) struct Review {
    pub(super) id: String,
    pub(super) approval: Approval,
    pub(super) canvas_size: [u32; 2],
    _charge: Arc<MetadataCharge>,
}
impl Stamp {
    pub(super) fn check_proposal(&self, shared: &Shared) -> Result<()> {
        self.proposal
            .as_ref()
            .map_or(Ok(()), |review| review.approval.check(shared))
    }
}
impl Dispatch {
    fn review(&self, document: &NativeDocument, project: &str, id: &str) -> Option<&Review> {
        let (_, stamp, _) = self.expected.as_ref()?;
        let review = stamp.proposal.as_ref()?;
        (stamp.matches(project, document) && review.id == id).then_some(review)
    }
}
impl super::super::PeerBrush {
    pub(in crate::ui) fn capture_native_proposal(
        &self,
        document: &NativeDocument,
        project: &str,
        id: Option<&str>,
    ) -> Result<Option<Input>> {
        let mut size = [document.metadata().width(), document.metadata().height()];
        let approval = if let Some(id) = id {
            // The stamp's signal tracks the pending record. Rendering and
            // presentation still verify the record under the engine lock.
            let approval = match self.native_dispatch.review(document, project, id) {
                Some(review) => review.approval.clone(),
                None => native::review_guard(&self.shared, id)?,
            };
            size = approval.canvas_size(project, document.key())?;
            Some((id, approval))
        } else {
            None
        };
        let edge = size[0].max(size[1]).max(8192);
        let region = self.visible_native_region(size);
        let capture = CommandCapture::new(&[], None, None, false, None)?;
        let base = capture.signature(edge)?;
        let signature = match &approval {
            Some((id, _)) => fingerprint(&("proposal", base, id), edge, None, false, None)?,
            None => base,
        };
        let epoch = self.preview_epoch.load(Ordering::Relaxed);
        let unchanged = self.native_dispatch.matches_capture(
            document,
            project,
            region_signature(signature, region),
            None,
            epoch,
        );
        let same_proposal = self.native_dispatch.expected.as_ref().is_some_and(|(_, stamp, _)| {
            match (&stamp.proposal, &approval) {
                (Some(previous), Some((_, approval))) => previous.approval.same(approval),
                (None, None) => true,
                _ => false,
            }
        });
        if unchanged && same_proposal {
            return Ok(None);
        }
        let mut input = capture.admit(document, project, edge)?;
        if let Some((id, approval)) = approval {
            let charge = MetadataCharge::reserve(document.budget(), id.len() + 512, &|| Ok(()))?;
            let stamp = Arc::get_mut(&mut input.stamp).ok_or("Proposal request is already shared")?;
            stamp.signature = signature;
            stamp.proposal = Some(Review {
                id: id.into(),
                approval,
                canvas_size: size,
                _charge: charge,
            });
            input.cache = false;
        }
        input.with_region(document, region).map(Some)
    }

    pub(in crate::ui) fn native_proposal_size(
        &self,
        document: view::DocumentView<'_>,
    ) -> Option<[u32; 2]> {
        let id = self.proposal_review.as_deref()?;
        if self.proposal_original {
            return None;
        }
        let document = document.native()?;
        if let Some(review) = self.native_dispatch.review(document, &self.project_id, id) {
            return review.approval.canvas_size(&self.project_id, document.key()).ok();
        }
        native::review_guard(&self.shared, id)
            .ok()?
            .canvas_size(&self.project_id, document.key())
            .ok()
    }

    pub(in crate::ui) fn native_proposal_canvas_current(
        &self,
        document: view::DocumentView<'_>,
    ) -> bool {
        let Some(id) = self.proposal_review.as_deref() else {
            return true;
        };
        let Some(document) = document.native() else {
            return false;
        };
        let Some(receipt) = self.native_receipt.as_ref() else {
            return false;
        };
        let Some(canvas) = self.native_canvas.current() else {
            return false;
        };
        receipt.job == self.preview_epoch.load(Ordering::Relaxed)
            && self
                .native_dispatch
                .expected
                .as_ref()
                .is_some_and(|(job, stamp, pending)| {
                    *job == receipt.job && !pending && stamp.same_request(&receipt.stamp)
                })
            && receipt.stamp.matches(&self.project_id, document)
            && receipt.tag.same_allocation(canvas.tag())
            && receipt.stamp.check_proposal(&self.shared).is_ok()
            && if self.proposal_original {
                receipt.stamp.proposal.is_none()
            } else {
                receipt
                    .stamp
                    .proposal
                    .as_ref()
                    .is_some_and(|review| review.id == id)
            }
    }
}
