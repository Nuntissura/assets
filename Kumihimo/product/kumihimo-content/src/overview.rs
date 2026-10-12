//! Sparse host semantics, never document loading or text/scene generation.
use crate::navigation::{increasing, nonnegative};
use crate::*;
use kumihimo_core::Scalar;
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverviewMarker {
    pub id: String,
    pub start: Scalar,
    pub end: Scalar,
    pub jump_target: Option<String>,
    pub label: String,
    pub style: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overview {
    pub revision: u64,
    pub model_revision: u64,
    pub unit: String,
    pub markers: Vec<OverviewMarker>,
}
impl Overview {
    pub fn opaque_bytes(&self) -> Result<usize> {
        self.markers.iter().try_fold(self.unit.len(), |total, m| {
            [
                m.id.len(),
                m.jump_target.as_ref().map_or(0, String::len),
                m.label.len(),
                m.style.len(),
            ]
            .into_iter()
            .try_fold(total, |sum, n| {
                sum.checked_add(n).ok_or(Error::Exhausted("overview bytes"))
            })
        })
    }
    fn longest_field(&self) -> usize {
        self.markers.iter().fold(self.unit.len(), |n, m| {
            n.max(m.id.len())
                .max(m.jump_target.as_ref().map_or(0, String::len))
                .max(m.label.len())
                .max(m.style.len())
        })
    }
    /// Lowered caps retain old obligations and accept only non-growing replacement dimensions.
    pub fn validate(
        &self,
        model: &DocumentModel,
        limits: &ContentLimits,
        old: Option<&Overview>,
    ) -> Result<()> {
        increasing(self.revision, 0, false)?;
        if self.model_revision != model.revision || self.unit != model.unit {
            return Err(Error::Stale("overview model/unit"));
        }
        if let Some(limit) = limits.overview_markers {
            if self.markers.len() > limit && self.markers.len() > old.map_or(0, |o| o.markers.len())
            {
                return Err(Error::Admission {
                    dimension: "overview markers",
                    limit,
                    attempted: self.markers.len(),
                });
            }
        }
        let bytes = self.opaque_bytes()?;
        let max_field = self.longest_field();
        let old_bytes = old.map(Self::opaque_bytes).transpose()?.unwrap_or(0);
        for (dimension, limit, new, previous) in [
            (
                "overview markers",
                limits.overview_markers,
                self.markers.len(),
                old.map_or(0, |o| o.markers.len()),
            ),
            ("overview bytes", limits.overview_bytes, bytes, old_bytes),
            (
                "marker bytes",
                limits.marker_bytes,
                max_field,
                old.map_or(0, Self::longest_field),
            ),
            (
                "payload_bytes",
                limits.payload_bytes,
                max_field,
                old.map_or(0, Self::longest_field),
            ),
        ] {
            if let Some(limit) = limit {
                if new > limit && new > previous {
                    return Err(Error::Admission {
                        dimension,
                        limit,
                        attempted: new,
                    });
                }
            }
        }
        let mut ids = HashSet::with_capacity(self.markers.len());
        for marker in &self.markers {
            if marker.id.is_empty()
                || !ids.insert(marker.id.as_str())
                || marker.jump_target.as_ref().is_some_and(String::is_empty)
            {
                return Err(Error::Invalid("overview marker identity"));
            }
            nonnegative(marker.start)?;
            nonnegative(marker.end)?;
            if marker.end.get() < marker.start.get() || marker.end.get() > model.extent.get() {
                return Err(Error::Invalid("overview marker range"));
            }
        }
        Ok(())
    }
}
impl Controller {
    pub fn update_overview(&mut self, scope: &Scope, overview: Overview) -> Result<()> {
        self.transaction(|core, s| {
            s.navigation_scope(core, scope)?;
            if !s.providers[&scope.provider]
                .capabilities
                .navigation
                .overview
            {
                return Err(Error::Unsupported("overview capability"));
            }
            let n = s
                .navigation
                .get(&scope.mount)
                .ok_or(Error::Stale("document model missing"))?;
            let model = n
                .model
                .as_ref()
                .ok_or(Error::Stale("document model missing"))?;
            if !increasing(
                overview.revision,
                n.last_overview_revision,
                n.overview.as_ref() == Some(&overview),
            )? {
                return Ok(());
            }
            overview.validate(model, &s.limits, n.overview.as_ref())?;
            let before = n.effective(&s.providers[&scope.provider].capabilities).0;
            let n = s.navigation.get_mut(&scope.mount).unwrap();
            n.last_overview_revision = overview.revision;
            n.overview = Some(overview);
            let after = n.effective(&s.providers[&scope.provider].capabilities).0;
            if before != after {
                s.navigation_invalidate(scope.mount);
            }
            Ok(())
        })
    }
}
