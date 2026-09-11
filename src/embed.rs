use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokenizers::Tokenizer;

const DOC_PREFIX: &str = "title: none | text: ";
const QUERY_PREFIX: &str = "task: search result | query: ";
const CODE_QUERY_PREFIX: &str = "task: code retrieval | query: ";

pub struct Embedder {
    tokenizer: Tokenizer,
    session: ort::session::Session,
    dim: usize,
}

impl Embedder {
    fn resolve_model_paths() -> Result<(PathBuf, PathBuf)> {
        // Delegate to global resolver (handles ./model, ~/.cache/reko, auto-download)
        crate::model::resolve_model_paths(None)
    }

    pub fn new() -> Result<Self> {
        Self::from_paths(None, None)
    }

    pub fn from_paths(model_path: Option<&Path>, tok_path: Option<&Path>) -> Result<Self> {
        let (mp, tp) = match (model_path, tok_path) {
            (Some(m), Some(t)) => (m.to_path_buf(), t.to_path_buf()),
            _ => Self::resolve_model_paths().or_else(|_| crate::model::resolve_model_paths(None))?,
        };

        let _ = ort::init().commit();

        let tokenizer = Tokenizer::from_file(&tp)
            .map_err(|e| anyhow::anyhow!("failed to load tokenizer {}: {}", tp.display(), e))?;

        let session = ort::session::Session::builder()
            .map_err(|e| anyhow::anyhow!("ort builder: {}", e))?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow::anyhow!("opt level: {}", e))?
            .with_intra_threads(4)
            .map_err(|e| anyhow::anyhow!("threads: {}", e))?
            .commit_from_file(&mp)
            .map_err(|e| anyhow::anyhow!("load ONNX {}: {}", mp.display(), e))?;

        Ok(Self {
            tokenizer,
            session,
            dim: 768,
        })
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    pub fn embed_batch(&mut self, texts: &[String], is_query: bool) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let prefix = if is_query { QUERY_PREFIX } else { DOC_PREFIX };
        let prefixed: Vec<String> = texts.iter().map(|t| format!("{prefix}{t}")).collect();
        self.embed_raw(&prefixed)
    }

    pub fn embed_code_chunks(&mut self, chunks: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed_batch(chunks, false)
    }

    pub fn embed_query(&mut self, query: &str) -> Result<Vec<f32>> {
        let q = format!("{CODE_QUERY_PREFIX}{query}");
        let v = self.embed_raw(&[q])?;
        Ok(v.into_iter().next().unwrap_or_else(|| vec![0.0; self.dim]))
    }

    fn embed_raw(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| anyhow::anyhow!("tokenize failed: {e}"))?;

        let batch = encodings.len();
        let max_len = encodings.iter().map(|e| e.len()).max().unwrap_or(0).min(2048);
        if max_len == 0 {
            return Ok(vec![vec![0.0; self.dim]; batch]);
        }

        let mut ids: Vec<i64> = Vec::with_capacity(batch * max_len);
        let mut mask: Vec<i64> = Vec::with_capacity(batch * max_len);
        for enc in &encodings {
            let id_slice = enc.get_ids();
            let attn = enc.get_attention_mask();
            let take = max_len.min(id_slice.len());
            for i in 0..max_len {
                if i < take {
                    ids.push(id_slice[i] as i64);
                    mask.push(attn[i] as i64);
                } else {
                    ids.push(0);
                    mask.push(0);
                }
            }
        }

        let input_names: Vec<String> = self.session.inputs().iter().map(|i| i.name().to_string()).collect();

        let ids_tensor = ort::value::Tensor::from_array(([batch, max_len], ids))
            .map_err(|e| anyhow::anyhow!("ids tensor: {}", e))?;
        let mask_tensor = ort::value::Tensor::from_array(([batch, max_len], mask))
            .map_err(|e| anyhow::anyhow!("mask tensor: {}", e))?;

        let mut inputs_vec: Vec<(std::borrow::Cow<str>, ort::session::SessionInputValue)> = Vec::new();
        for name in &input_names {
            match name.as_str() {
                "input_ids" => inputs_vec.push((name.clone().into(), ids_tensor.clone().into())),
                "attention_mask" => inputs_vec.push((name.clone().into(), mask_tensor.clone().into())),
                "token_type_ids" => {
                    let zeros = vec![0i64; batch * max_len];
                    let t = ort::value::Tensor::from_array(([batch, max_len], zeros))
                        .map_err(|e| anyhow::anyhow!("zeros tensor: {}", e))?;
                    inputs_vec.push((name.clone().into(), t.into()));
                }
                _ => {}
            }
        }

        let outputs = self
            .session
            .run(inputs_vec)
            .map_err(|e| anyhow::anyhow!("ort run: {}", e))?;

        // Try to find sentence_embedding output
        let mut emb_opt: Option<Vec<Vec<f32>>> = None;

        // helper inline extraction
        if let Some(val) = outputs.get("sentence_embedding") {
            if let Ok((shape, data)) = val.try_extract_tensor::<f32>() {
                if shape.len() == 2 && shape[0] as usize == batch && shape[1] as usize == 768 {
                    let flat = data.to_vec();
                    let mut batch_emb = Vec::with_capacity(batch);
                    for b in 0..batch {
                        let start = b * 768;
                        let mut v = flat[start..start + 768].to_vec();
                        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                        if norm > 0.0 {
                            for x in &mut v { *x /= norm; }
                        }
                        batch_emb.push(v);
                    }
                    emb_opt = Some(batch_emb);
                }
            }
        }
        if emb_opt.is_none() {
            for (_k, val) in outputs.iter() {
                if let Ok((shape, data)) = val.try_extract_tensor::<f32>() {
                    if shape.len() == 2 && shape[0] as usize == batch && shape[1] as usize == 768 {
                        let flat = data.to_vec();
                        let mut batch_emb = Vec::with_capacity(batch);
                        for b in 0..batch {
                            let start = b * 768;
                            let mut v = flat[start..start + 768].to_vec();
                            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                            if norm > 0.0 {
                                for x in &mut v { *x /= norm; }
                            }
                            batch_emb.push(v);
                        }
                        emb_opt = Some(batch_emb);
                        break;
                    }
                }
            }
        }
        if emb_opt.is_none() {
            for val in outputs.values() {
                if let Ok((shape, data)) = val.try_extract_tensor::<f32>() {
                    if shape.len() == 2 && shape[0] as usize == batch && shape[1] as usize == 768 {
                        let flat = data.to_vec();
                        let mut batch_emb = Vec::with_capacity(batch);
                        for b in 0..batch {
                            let start = b * 768;
                            let mut v = flat[start..start + 768].to_vec();
                            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                            if norm > 0.0 {
                                for x in &mut v { *x /= norm; }
                            }
                            batch_emb.push(v);
                        }
                        emb_opt = Some(batch_emb);
                        break;
                    }
                }
            }
        }

        emb_opt.ok_or_else(|| {
            anyhow::anyhow!(
                "failed to find sentence_embedding [batch,768] in outputs {:?} (inputs {:?})",
                outputs.keys().collect::<Vec<_>>(),
                input_names
            )
        })
    }
}
