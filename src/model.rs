use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Global model location (XDG cache). Falls back to ~/.cache/reko/model or ~/.reko/model
pub fn global_model_dir() -> PathBuf {
    if let Some(cache) = dirs::cache_dir() {
        return cache.join("reko").join("model");
    }
    if let Some(home) = dirs::home_dir() {
        return home.join(".cache").join("reko").join("model");
    }
    PathBuf::from("./model")
}

/// Resolve model files, auto-downloading to global cache if missing.
/// Search order:
/// 1. --model flag (explicit)
/// 2. $REKO_MODEL / $REKO_MODEL_PATH env
/// 3. ./model (project local, dev)
/// 4. global cache (~/.cache/reko/model)
/// If not found, auto-download via `hf` CLI or `hf-hub`.
pub fn resolve_model_paths(model_opt: Option<&Path>) -> Result<(PathBuf, PathBuf)> {
    // 1. explicit --model
    if let Some(p) = model_opt {
        if let Some(found) = find_model_in_dir(p) {
            return Ok(found);
        }
        // if explicit dir provided but not found, try to use it as base and download there
        if p.exists() || !p.as_os_str().is_empty() {
            // try download to that dir
            if let Ok(found) = ensure_downloaded(p) {
                return Ok(found);
            }
        }
    }

    // 2. env
    for env in ["REKO_MODEL", "REKO_MODEL_PATH"] {
        if let Ok(val) = std::env::var(env) {
            let pb = PathBuf::from(val);
            if let Some(found) = find_model_in_dir(&pb) {
                return Ok(found);
            }
            if pb.exists() {
                if let Ok(found) = ensure_downloaded(&pb) {
                    return Ok(found);
                }
            }
        }
    }

    // 3. local ./model
    for cand in [PathBuf::from("./model"), PathBuf::from("./model/onnx")] {
        let dir = if cand.ends_with("onnx") {
            cand.parent().unwrap().to_path_buf()
        } else {
            cand
        };
        if let Some(found) = find_model_in_dir(&dir) {
            return Ok(found);
        }
    }

    // 4. global cache
    let global = global_model_dir();
    if let Some(found) = find_model_in_dir(&global) {
        return Ok(found);
    }
    // also try global/onnx subfolder
    if let Some(found) = find_model_in_dir(&global.join("onnx")) {
        // need to return with correct tokenizer path
        // find_model_in_dir will handle, but we try global directly
        return Ok(found);
    }

    // 5. fallback ~/.reko/model
    if let Some(home) = dirs::home_dir() {
        let alt = home.join(".reko").join("model");
        if let Some(found) = find_model_in_dir(&alt) {
            return Ok(found);
        }
    }

    // Not found anywhere → auto-download to global cache
    eprintln!("Model not found locally, downloading embeddinggemma-300m-ONNX to {} ...", global.display());
    eprintln!("This is a one-time 200M download (q4, 768d) and will be cached globally.");
    ensure_downloaded(&global)
}

/// Find model_q4.onnx + tokenizer.json inside dir (or dir/onnx)
fn find_model_in_dir(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let candidates = [
        dir.join("onnx").join("model_q4.onnx"),
        dir.join("model_q4.onnx"),
        dir.join("onnx").join("model.onnx"),
        dir.join("model.onnx"),
    ];
    let tok_candidates = [
        dir.join("tokenizer.json"),
        dir.join("onnx").join("tokenizer.json"),
    ];
    let mp = candidates.iter().find(|p| p.exists())?.clone();
    let tp = tok_candidates.iter().find(|p| p.exists())?.clone();
    // also need data file
    let data = mp.with_extension("").with_extension("onnx_data");
    // for model_q4.onnx, data is model_q4.onnx_data
    let data_alt = PathBuf::from(format!("{}.onnx_data", mp.display()));
    // Check data exists (for q4, it's model_q4.onnx_data)
    let data_path = if mp.extension().and_then(|e| e.to_str()) == Some("onnx") {
        let candidate = mp.with_file_name(format!("{}.onnx_data", mp.file_stem().unwrap().to_string_lossy()));
        if candidate.exists() { Some(candidate) } else { None }
    } else {
        None
    };
    // If model is q4 but data missing, not valid
    if mp.file_name().and_then(|n| n.to_str()).unwrap_or("").contains("q4") {
        let data_file = dir.join("onnx").join("model_q4.onnx_data");
        let data_file2 = dir.join("model_q4.onnx_data");
        if !data_file.exists() && !data_file2.exists() && data_path.as_ref().map(|p| !p.exists()).unwrap_or(true) {
            return None;
        }
    }
    Some((mp, tp))
}

/// Download model to dir via `hf` CLI if available, else via `hf-hub` crate.
fn ensure_downloaded(dir: &Path) -> Result<(PathBuf, PathBuf)> {
    std::fs::create_dir_all(dir).ok();
    // Try hf CLI first (user has hf authenticated)
    if which_hf().is_some() {
        eprintln!("Downloading via `hf download` (this may take a minute)...");
        let status = std::process::Command::new("hf")
            .arg("download")
            .arg("onnx-community/embeddinggemma-300m-ONNX")
            .arg("--include")
            .arg("onnx/model_q4.onnx")
            .arg("--include")
            .arg("onnx/model_q4.onnx_data")
            .arg("--include")
            .arg("tokenizer.json")
            .arg("--include")
            .arg("tokenizer_config.json")
            .arg("--include")
            .arg("config.json")
            .arg("--local-dir")
            .arg(dir)
            .status()
            .context("failed to spawn hf download")?;
        if status.success() {
            if let Some(found) = find_model_in_dir(dir) {
                eprintln!("Downloaded to {}", dir.display());
                return Ok(found);
            }
        }
        eprintln!("hf download failed or incomplete, trying hf-hub fallback...");
    }

    // Fallback: use hf-hub crate (requires tokio, but we can use sync API without tokio)
    // Use hf_hub::api::sync::Api
    eprintln!("Downloading via hf-hub crate...");
    let api = hf_hub::api::sync::Api::new().context("hf-hub api")?;
    let repo = api.model("onnx-community/embeddinggemma-300m-ONNX".to_string());

    let files = [
        "onnx/model_q4.onnx",
        "onnx/model_q4.onnx_data",
        "tokenizer.json",
        "tokenizer_config.json",
        "config.json",
    ];
    for file in files {
        let dest = dir.join(file);
        if dest.exists() {
            continue;
        }
        eprintln!(" fetching {file} ...");
        let path = repo
            .get(file)
            .with_context(|| format!("failed to download {file}"))?;
        std::fs::create_dir_all(dest.parent().unwrap()).ok();
        std::fs::copy(&path, &dest).with_context(|| format!("copy to {}", dest.display()))?;
    }

    find_model_in_dir(dir).ok_or_else(|| anyhow::anyhow!("model still not found after download in {}", dir.display()))
}

fn which_hf() -> Option<PathBuf> {
    // check if `hf` in PATH
    if let Ok(out) = std::process::Command::new("which").arg("hf").output() {
        if out.status.success() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
        }
    }
    // also try `hf --help` directly
    if std::process::Command::new("hf").arg("--help").output().is_ok() {
        return Some(PathBuf::from("hf"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn global_dir_is_some() {
        let p = global_model_dir();
        assert!(!p.as_os_str().is_empty());
    }
}
