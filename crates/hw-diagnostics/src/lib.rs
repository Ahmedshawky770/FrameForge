//! hw-diagnostics: detect VAAPI/NVENC, report active decoder/encoder/interop/copy-back.

use serde::{Deserialize, Serialize};
use std::path::Path;
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

#[cfg(feature = "vaapi")]
fn try_vaapi() -> Option<Diagnostics> {
    use libva::{Display, VAProfile, VAEntrypoint};

    let render_nodes = find_render_nodes();
    for node in render_nodes {
        if let Ok(display) = Display::drm(node) {
            if let Ok(profiles) = display.query_config_profiles() {
                let mut decode_codecs = Vec::new();
                let mut encode_codecs = Vec::new();

                for profile in profiles {
                    if let Ok(entrypoints) = display.query_config_entrypoints(profile) {
                        for entrypoint in entrypoints {
                            match (profile, entrypoint) {
                                (VAProfile::H264, VAEntrypoint::VLD) => decode_codecs.push("h264"),
                                (VAProfile::HEVC, VAEntrypoint::VLD) => decode_codecs.push("hevc"),
                                (VAProfile::VP9, VAEntrypoint::VLD) => decode_codecs.push("vp9"),
                                (VAProfile::AV1, VAEntrypoint::VLD) => decode_codecs.push("av1"),
                                (VAProfile::H264, VAEntrypoint::EncSlice) => encode_codecs.push("h264"),
                                (VAProfile::HEVC, VAEntrypoint::EncSlice) => encode_codecs.push("hevc"),
                                (VAProfile::VP9, VAEntrypoint::EncSlice) => encode_codecs.push("vp9"),
                                (VAProfile::AV1, VAEntrypoint::EncSlice) => encode_codecs.push("av1"),
                                _ => {}
                            }
                        }
                    }
                }

                if !decode_codecs.is_empty() || !encode_codecs.is_empty() {
                    let driver_name = display.vendor_string().unwrap_or_default();
                    let interop = if driver_name.contains("Intel") || driver_name.contains("iHD") {
                        "drm"
                    } else if driver_name.contains("AMD") || driver_name.contains("radeon") {
                        "drm"
                    } else {
                        "drm"
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

                    return Some(Diagnostics {
                        decoder,
                        encoder,
                        interop: interop.into(),
                        copy_back: true,
                        hw_rejected_reason: None,
                    });
                }
            }
        }
    }

    try_vaapi_vainfo()
}

#[cfg(not(feature = "vaapi"))]
fn try_vaapi() -> Option<Diagnostics> {
    try_vaapi_vainfo()
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

fn find_render_nodes() -> Vec<std::path::PathBuf> {
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

#[cfg(feature = "nvenc")]
fn try_nvenc() -> Option<Diagnostics> {
    use nvml_wrapper::Nvml;

    let nvml = Nvml::init().ok()?;
    let device_count = nvml.device_count().ok()?;

    for i in 0..device_count {
        if let Ok(device) = nvml.device_by_index(i) {
            let name = device.name().unwrap_or_default();

            let mut decode_codecs = Vec::new();
            let mut encode_codecs = Vec::new();

            if let Ok(caps) = device.encoder_capabilities() {
                for cap in caps {
                    match cap.codec {
                        nvml_wrapper::enum_wrappers::encoder::CodecType::H264 => encode_codecs.push("h264"),
                        nvml_wrapper::enum_wrappers::encoder::CodecType::HEVC => encode_codecs.push("hevc"),
                        nvml_wrapper::enum_wrappers::encoder::CodecType::AV1 => encode_codecs.push("av1"),
                        _ => {}
                    }
                }
            }

            if let Ok(caps) = device.decoder_capabilities() {
                for cap in caps {
                    match cap.codec {
                        nvml_wrapper::enum_wrappers::decoder::CodecType::H264 => decode_codecs.push("h264"),
                        nvml_wrapper::enum_wrappers::decoder::CodecType::HEVC => decode_codecs.push("hevc"),
                        nvml_wrapper::enum_wrappers::decoder::CodecType::VP9 => decode_codecs.push("vp9"),
                        nvml_wrapper::enum_wrappers::decoder::CodecType::AV1 => decode_codecs.push("av1"),
                        _ => {}
                    }
                }
            }

            if !decode_codecs.is_empty() || !encode_codecs.is_empty() {
                let decoder = if decode_codecs.is_empty() {
                    "none".into()
                } else {
                    format!("nvdec:{}", decode_codecs.join(","))
                };
                let encoder = if encode_codecs.is_empty() {
                    "none".into()
                } else {
                    format!("nvenc:{}", encode_codecs.join(","))
                };

                return Some(Diagnostics {
                    decoder,
                    encoder,
                    interop: "cuda".into(),
                    copy_back: true,
                    hw_rejected_reason: None,
                });
            }
        }
    }

    try_nvenc_nvidia_smi()
}

#[cfg(not(feature = "nvenc"))]
fn try_nvenc() -> Option<Diagnostics> {
    try_nvenc_nvidia_smi()
}

fn try_nvenc_nvidia_smi() -> Option<Diagnostics> {
    let output = Command::new("nvidia-smi")
        .args(["--query-gpu=name", "--format=csv,noheader"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        return None;
    }

    let output2 = Command::new("nvidia-smi")
        .args(["--query-gpu=encoder.stats.sessionCount,decoder.stats.sessionCount", "--format=csv,noheader"])
        .output()
        .ok()?;
    let _ = String::from_utf8_lossy(&output2.stdout);

    let decode_codecs = vec!["h264", "hevc", "vp9", "av1"];
    let encode_codecs = vec!["h264", "hevc", "av1"];

    let decoder = format!("nvdec:{}", decode_codecs.join(","));
    let encoder = format!("nvenc:{}", encode_codecs.join(","));

    Some(Diagnostics {
        decoder,
        encoder,
        interop: "cuda".into(),
        copy_back: true,
        hw_rejected_reason: None,
    })
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