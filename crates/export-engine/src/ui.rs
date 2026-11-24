//! UI-facing export metadata: path label, reason, lossless badge, estimate.
//!
//! The HTML layer is intentionally not modified here; these helpers give the
//! Tauri/CLI layer the data it needs to render an accurate export summary.

use frameforge_edit_engine::ExportPlan;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportPath {
    /// All cuts are keyframe-aligned and the inputs are compatible.
    CopyLossless,
    /// Compatible inputs, but some cuts are mid-GOP: copy GOPs + boundary
    /// re-encode.
    SmartCopy,
    /// Full re-encode (incompatible inputs, or no smart path available).
    FullReencode,
}

impl ExportPath {
    pub fn label(self) -> &'static str {
        match self {
            ExportPath::CopyLossless => "copy (lossless stream copy)",
            ExportPath::SmartCopy => "smart copy (copy GOPs + boundary re-encode)",
            ExportPath::FullReencode => "full re-encode",
        }
    }
}

pub fn export_reason(plan: &ExportPlan, path: ExportPath) -> String {
    match path {
        ExportPath::CopyLossless => "all cuts are keyframe-aligned".to_string(),
        ExportPath::SmartCopy => {
            "cuts are not keyframe-aligned; boundary GOPs are re-encoded".to_string()
        }
        ExportPath::FullReencode => {
            if !plan.merge_plan.copy_compatible {
                "sources are not compatible for stream copy".to_string()
            } else {
                "lossless copy is not possible for these cuts".to_string()
            }
        }
    }
}

/// The "Lossless OK" badge must only be shown when the whole plan is lossless.
/// Mid-GOP cuts make `merge_plan.lossless` false even when the inputs are
// compatible.
pub fn lossless_badge_visible(plan: &ExportPlan) -> bool {
    plan.merge_plan.lossless
}

/// Rough wall-clock estimate in seconds.  The caller may replace this with a
/// measurement from previous exports.
pub fn estimate_export_seconds(plan: &ExportPlan, path: ExportPath, hardware: bool) -> u64 {
    let duration: f64 = plan
        .segments
        .iter()
        .map(|s| (s.range.end.to_seconds() - s.range.start.to_seconds()).max(0.0))
        .sum();
    let factor = match path {
        ExportPath::CopyLossless => 0.05,
        ExportPath::SmartCopy => 0.5,
        ExportPath::FullReencode => {
            if hardware {
                0.3
            } else {
                1.2
            }
        }
    };
    (duration * factor + 2.0).ceil() as u64
}
