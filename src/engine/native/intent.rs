use super::*;
use crate::{
    engine::{Lease, Scope},
    large_document::Reservation,
};

#[derive(Clone, Copy)]
pub enum IntentKind {
    Visibility { visible: bool },
    Reorder,
    Pixels,
    Placement,
    Selection,
    History,
    Capture,
}
pub struct Intent {
    kind: IntentKind,
    scopes: Vec<Scope>,
    _charge: Reservation,
}
impl Intent {
    pub(super) fn from_scopes(
        kind: IntentKind,
        source: &[Scope],
        pool: &ResidentBudget,
    ) -> Result<Self> {
        let bytes = source.iter().try_fold(
            source
                .len()
                .checked_mul(std::mem::size_of::<Scope>())
                .and_then(|n| n.checked_add(128))
                .ok_or("Native scope footprint overflow")?,
            |n, scope| {
                n.checked_add(scope.target.as_ref().map_or(0, String::len))
                    .ok_or("Native scope footprint overflow")
            },
        )?;
        let charge = pool
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        let mut scopes = Vec::new();
        scopes
            .try_reserve_exact(source.len())
            .map_err(|_| "Native scope allocation refused")?;
        for scope in source {
            let target = scope
                .target
                .as_ref()
                .map(|text| {
                    let mut copy = String::new();
                    copy.try_reserve_exact(text.len())
                        .map_err(|_| "Native scope target allocation refused")?;
                    copy.push_str(text);
                    Ok::<_, String>(copy)
                })
                .transpose()?;
            scopes.push(Scope {
                target,
                rect: scope.rect,
            });
        }
        Ok(Self {
            kind,
            scopes,
            _charge: charge,
        })
    }
    pub fn kind(&self) -> &IntentKind {
        &self.kind
    }
    pub fn scopes(&self) -> &[Scope] {
        &self.scopes
    }
    pub(super) fn visibility(layer: &str, visible: bool, pool: &ResidentBudget) -> Result<Self> {
        let bytes = layer
            .len()
            .checked_add(12 + std::mem::size_of::<Scope>() + 128)
            .ok_or("Native intent footprint overflow")?;
        let charge = pool
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        let mut target = String::new();
        target
            .try_reserve_exact(layer.len() + 12)
            .map_err(|_| "Native intent allocation refused")?;
        target.push_str("@visibility:");
        target.push_str(layer);
        let mut scopes = Vec::new();
        scopes
            .try_reserve_exact(1)
            .map_err(|_| "Native scope allocation refused")?;
        scopes.push(Scope {
            target: Some(target),
            rect: None,
        });
        Ok(Self {
            kind: IntentKind::Visibility { visible },
            scopes,
            _charge: charge,
        })
    }
    pub(super) fn layers(kind: IntentKind, ids: &[String], pool: &ResidentBudget) -> Result<Self> {
        let bytes = ids.iter().try_fold(
            ids.len()
                .checked_mul(std::mem::size_of::<Scope>())
                .and_then(|n| n.checked_add(128))
                .ok_or("Native scope footprint overflow")?,
            |n, id| {
                n.checked_add(id.len())
                    .ok_or("Native scope footprint overflow")
            },
        )?;
        let charge = pool
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        let mut scopes = Vec::new();
        scopes
            .try_reserve_exact(ids.len())
            .map_err(|_| "Native scope allocation refused")?;
        for id in ids {
            let mut target = String::new();
            target
                .try_reserve_exact(id.len())
                .map_err(|_| "Native scope target allocation refused")?;
            target.push_str(id);
            scopes.push(Scope {
                target: Some(target),
                rect: None,
            });
        }
        Ok(Self {
            kind,
            scopes,
            _charge: charge,
        })
    }
    /// Recheck current leases at final publication; preparation-time admission
    /// cannot authorize a lease acquired while source I/O was running.
    pub(super) fn check(
        &self,
        document: &NativeDocument,
        actor: &str,
        leases: &[Lease],
        now: u64,
    ) -> Result<()> {
        for lease in leases {
            if lease.owner != actor
                && lease.expires > now
                && self.scopes.iter().any(|scope| {
                    lease
                        .scopes
                        .iter()
                        .any(|held| overlap(document, scope, held))
                })
            {
                return Err(format!(
                    "Reserved by {}: {}. Release the reservation or edit elsewhere.",
                    lease.owner, lease.description
                ));
            }
        }
        Ok(())
    }
}
fn overlap(document: &NativeDocument, a: &Scope, b: &Scope) -> bool {
    let visibility = |scope: &Scope| {
        scope
            .target
            .as_ref()
            .is_some_and(|target| target.starts_with("@visibility:"))
    };
    if visibility(a) || visibility(b) {
        return visibility(a) && visibility(b) && a.target == b.target;
    }
    if let (Some(x), Some(y)) = (&a.target, &b.target) {
        if x != y {
            let ancestor = |child: &str, parent: &str| {
                let mut current = Some(child);
                for _ in 0..=16 {
                    let Some(id) = current else { return false };
                    if id == parent {
                        return true;
                    }
                    current = document
                        .core
                        .metadata
                        .layers
                        .iter()
                        .find(|layer| layer.id == id)
                        .and_then(|layer| layer.parent.as_deref());
                }
                false
            };
            if !ancestor(x, y) && !ancestor(y, x) {
                return false;
            }
        }
    }
    match (a.rect, b.rect) {
        (Some(a), Some(b)) => a[0] < b[2] && a[2] > b[0] && a[1] < b[3] && a[3] > b[1],
        _ => true,
    }
}
