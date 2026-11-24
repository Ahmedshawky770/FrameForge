//! hw-diagnostics: detect VAAPI/NVENC, report active decoder/encoder/interop/copy-back.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostics {
    pub decoder: String,
    pub encoder: String,
    pub interop: String,
    pub copy_back: bool,
    pub hw_rejected_reason: Option<String>,
}

impl Default for Diagnostics {
    fn default() -> Self {
        Self {
            decoder: "none".into(),
            encoder: "none".into(),
            interop: "none".into(),
            copy_back: false,
            hw_rejected_reason: Some("HW detection unavailable".into()),
        }
    }
}

pub fn probe_hw() -> Diagnostics {
    let mut reasons: Vec<String> = Vec::new();

    if let Some(diag) = try_vaapi() {
        return diag;
    } else {
        reasons.push("VAAPI: not available or no supported codecs".into());
    }

    if let Some(diag) = try_nvenc() {
        return diag;
    } else {
        reasons.push("NVENC/NVDEC: not available".into());
    }

    Diagnostics {
        decoder: "none".into(),
        encoder: "none".into(),
        interop: "none".into(),
        copy_back: false,
        hw_rejected_reason: Some(reasons.join("; ")),
    }
}

fn try_vaapi() -> Option<Diagnostics> {
    try_vaapi_vainfo()
}

fn try_nvenc() -> Option<Diagnostics> {
    try_nvenc_nvidia_smi()
}

fn try_vaapi_vainfo() -> Option<Diagnostics> {
    let output = Command::new("vainfo").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut decode_codecs = Vec::new();
    let mut encode_codecs = Vec::new();

    for line in stdout.lines() {
        let line = line.trim();
        if line.contains("VAProfileH264") && line.contains("VAEntrypointVLD") {
            decode_codecs.push("h264");
        }
        if line.contains("VAProfileHEVC") && line.contains("VAEntrypointVLD") {
            decode_codecs.push("hevc");
        }
        if line.contains("VAProfileVP9") && line.contains("VAEntrypointVLD") {
            decode_codecs.push("vp9");
        }
        if line.contains("VAProfileAV1") && line.contains("VAEntrypointVLD") {
            decode_codecs.push("av1");
        }
        if line.contains("VAProfileH264") && line.contains("VAEntrypointEncSlice") {
            encode_codecs.push("h264");
        }
        if line.contains("VAProfileHEVC") && line.contains("VAEntrypointEncSlice") {
            encode_codecs.push("hevc");
        }
        if line.contains("VAProfileVP9") && line.contains("VAEntrypointEncSlice") {
            encode_codecs.push("vp9");
        }
        if line.contains("VAProfileAV1") && line.contains("VAEntrypointEncSlice") {
            encode_codecs.push("av1");
        }
    }

    if decode_codecs.is_empty() && encode_codecs.is_empty() {
        return None;
    }

    let render_nodes = find_render_nodes();
    let interop = if render_nodes.iter().any(|p| p.exists()) {
        "drm"
    } else {
        "x11"
    };

    let decoder = if decode_codecs.is_empty() {
        "none".into()
    } else {
        format!("vaapi:{}", decode_codecs.join(","))
    };
    let encoder = if encode_codecs.is_empty() {
        "none".into()
    } else {
        format!("vaapi:{}", encode_codecs.join(","))
    };

    Some(Diagnostics {
        decoder,
        encoder,
        interop: interop.into(),
        copy_back: true,
        hw_rejected_reason: None,
    })
}

fn try_nvenc_nvidia_smi() -> Option<Diagnostics> {
    let output = Command::new("nvidia-smi")
        .arg("--query-gpu=name")
        .arg("--format=csv,noheader")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.trim().is_empty() {
        return None;
    }
    Some(Diagnostics {
        decoder: "nvdec".into(),
        encoder: "nvenc".into(),
        interop: "cuda".into(),
        copy_back: true,
        hw_rejected_reason: None,
    })
}

fn find_render_nodes() -> Vec<PathBuf> {
    let mut nodes = Vec::new();
    let dri_path = Path::new("/dev/dri");
    if let Ok(entries) = std::fs::read_dir(dri_path) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("renderD") {
                nodes.push(entry.path());
            }
        }
    }
    nodes.sort();
    nodes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_default_none() {
        let d = Diagnostics::default();
        assert_eq!(d.decoder, "none");
        assert!(d.hw_rejected_reason.is_some());
    }

    #[test]
    fn find_render_nodes_returns_sorted() {
        let nodes = find_render_nodes();
        let mut sorted = nodes.clone();
        sorted.sort();
        assert_eq!(nodes, sorted);
    }
}
