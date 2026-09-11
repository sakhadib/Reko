use crate::embed::Embedder;
use crate::scan::{self, FileRecord};
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};

/// Ensure .reko/reko.jsonl exists, else extract.
/// Returns path to .reko/reko.jsonl and records.
pub fn ensure_reko_jsonl(root: &Path) -> Result<(PathBuf, Vec<FileRecord>)> {
    let reko_dir = root.join(".reko");
    std::fs::create_dir_all(&reko_dir).ok();
    let jsonl = reko_dir.join("reko.jsonl");
    if jsonl.exists() {
        // Load existing
        let records = load_jsonl(&jsonl)?;
        if !records.is_empty() {
            return Ok((jsonl, records));
        }
        // fall through to re-extract if empty
    }
    // Auto-extract
    eprintln!(" .reko/reko.jsonl not found → running extract pipeline...");
    let (records, stats) = scan::scan_directory(root)?;
    scan::write_jsonl(&jsonl, &records)?;
    eprintln!(
        " Extracted {} files → {} functions → {}",
        stats.files_with_functions, stats.total_functions, jsonl.display()
    );
    Ok((jsonl, records))
}

fn load_jsonl(path: &Path) -> Result<Vec<FileRecord>> {
    use std::io::{BufRead, BufReader};
    let f = std::fs::File::open(path)?;
    let reader = BufReader::new(f);
    let mut out = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let rec: FileRecord = serde_json::from_str(&line)?;
        out.push(rec);
    }
    Ok(out)
}

/// Create or open sqlite db at .reko/reko.db with sqlite-vec.
pub fn open_db(db_path: &Path) -> Result<Connection> {
    // Register sqlite-vec extension
    unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    }
    let conn = Connection::open(db_path)
        .with_context(|| format!("open db {}", db_path.display()))?;

    // Enable extension loading if needed (for bundled)
    // Create tables
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS files (
            file TEXT PRIMARY KEY,
            language TEXT,
            count INTEGER,
            absolute TEXT
        );
        CREATE TABLE IF NOT EXISTS functions (
            id TEXT PRIMARY KEY,
            file TEXT,
            qualified_name TEXT,
            language TEXT,
            name TEXT,
            source_text TEXT,
            hash TEXT,
            start_line INTEGER,
            end_line INTEGER
        );
        "#,
    )?;

    // vec0 virtual table for embeddings: 768 dims, cosine
    // Use IF NOT EXISTS
    let _ = conn.execute(
        "CREATE VIRTUAL TABLE IF NOT EXISTS vec_index USING vec0(embedding float[768] distance_metric=cosine, id TEXT PRIMARY KEY, file TEXT, qualified_name TEXT)",
        [],
    );

    // Also ensure chunks table for debugging
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS chunks (
            id TEXT PRIMARY KEY,
            function_id TEXT,
            chunk_idx INTEGER,
            content TEXT,
            embedding BLOB
        );
        "#,
    )?;

    Ok(conn)
}

/// Index all functions in .reko/reko.jsonl into .reko/reko.db using EmbeddingGemma.
/// `model_path` is the model dir (contains tokenizer.json); if None, auto-resolves ./model.
pub fn index_directory_with_model(root: &Path, model_path: Option<&Path>) -> Result<PathBuf> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let reko_dir = root.join(".reko");
    std::fs::create_dir_all(&reko_dir)?;

    let (_jsonl_path, records) = ensure_reko_jsonl(&root)?;
    let db_path = reko_dir.join("reko.db");

    let mut conn = open_db(&db_path)?;

    // Prepare embedder – respect explicit model dir
    let mut _model_path_dbg = model_path.map(|p| p.display().to_string());
    let mut embedder = if let Some(mp) = model_path {
        let model_file = mp.join("onnx").join("model_q4.onnx");
        let model_file2 = mp.join("model_q4.onnx");
        let mf = if model_file.exists() { model_file } else { model_file2 };
        let tok = mp.join("tokenizer.json");
        let tok2 = mp.join("onnx").join("tokenizer.json");
        let tf = if tok.exists() { tok } else { tok2 };
        Embedder::from_paths(Some(&mf), Some(&tf)).context("failed to init embedder")?
    } else {
        Embedder::new().context("failed to init embedder (check ./model/)")?
    };

    // Progress bar for indexing
    use indicatif::{ProgressBar, ProgressStyle};
    let total_funcs: usize = records.iter().map(|r| r.count).sum();
    let pb = ProgressBar::new(total_funcs as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) {eta_precise} {msg}",
        )
        .unwrap()
        .progress_chars("█▉▊▋▌▍▎▏  "),
    );
    pb.set_message("Embedding functions...");

    // Clear previous data? For idempotent, delete and reinsert
    conn.execute("DELETE FROM functions", [])?;
    conn.execute("DELETE FROM vec_index", [])?;
    conn.execute("DELETE FROM files", [])?;
    conn.execute("DELETE FROM chunks", [])?;

    let tx = conn.transaction()?;

    // For each file/record, for each function, embed
    for rec in &records {
        // Insert file
        tx.execute(
            "INSERT OR REPLACE INTO files (file, language, count, absolute) VALUES (?1, ?2, ?3, ?4)",
            params![rec.file, rec.language, rec.count as i64, rec.absolute],
        )?;
        for func in &rec.functions {
            let base_id = func.identity.id.clone();
            // Make DB id unique per file to avoid collisions like sample::add across .js/.c
            let hash_suffix = func.source.hash.get(7..15).unwrap_or(&func.source.hash);
            let id = format!("{}::{}::{}", rec.file, base_id, hash_suffix);
            let qn = func.identity.qualified_name.clone();
            let name = func.identity.name.clone();
            let lang = func.identity.language.clone();
            let src = func.source.source_text.clone();
            let hash = func.source.hash.clone();
            let sl = func.source.location.start.line as i64;
            let el = func.source.location.end.line as i64;

            tx.execute(
                "INSERT OR REPLACE INTO functions (id, file, qualified_name, language, name, source_text, hash, start_line, end_line) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![id, rec.file, qn, lang, name, src, hash, sl, el],
            )?;
            // Keep original qualified mapping for debugging: also store base_id via extra? For now id is unique.

            // Chunk: for now one chunk per function (source_text)
            // Use normalized_source if available else source_text
            let chunk_content = if func.source.normalized_source.trim().is_empty() {
                src.clone()
            } else {
                func.source.normalized_source.clone()
            };
            // Truncate if too long (embedding model max 2048 tokens ~ 8000 chars)
            let chunk_trunc = if chunk_content.len() > 8000 {
                chunk_content.chars().take(8000).collect::<String>()
            } else {
                chunk_content
            };

            // Embed batch of 1 for now (could batch) – skip on failure to avoid aborting whole DB
            let vec = match embedder.embed_code_chunks(&[chunk_trunc.clone()]) {
                Ok(mut v) => v.into_iter().next().unwrap_or_else(|| vec![0.0; 768]),
                Err(e) => {
                    pb.println(format!("warn: embed failed for {} ({}): {}", id, rec.file, e));
                    vec![0.0; 768]
                }
            };

            // Serialize embedding as JSON for vec0? sqlite-vec expects float[768] as blob or JSON?
            // Use vec0's expected format: JSON array string or blob. We'll use blob via rusqlite's blob?
            // sqlite-vec expects embedding as float array via `vec_f32`? We can insert as binary blob.
            // Simplest: INSERT INTO vec_index(rowid, embedding, id, file, qualified_name) VALUES (?, vec_f32(?), ?, ?, ?)
            // But vec0 expects embedding float[768] type; we can use `vec_f32` function? Check.
            // Alternative: use `INSERT INTO vec_index(embedding, id, file, qualified_name) VALUES (?, ?, ?, ?)` where ? is JSON array.

            // Convert to JSON array string for sqlite-vec (it accepts JSON)
            let emb_json = serde_json::to_string(&vec)?;

            // For vec0, we need to insert embedding as JSON array string via `vec_f32`? Let's try direct.
            // Try using `embedding` column as blob: use `?` with JSON
            // We'll use `vec_f32` helper if available, else raw JSON
            // For now insert as JSON string; sqlite-vec will parse.
            tx.execute(
                "INSERT OR REPLACE INTO vec_index (embedding, id, file, qualified_name) VALUES (vec_f32(?1), ?2, ?3, ?4)",
                params![emb_json, id, rec.file, qn],
            ).or_else(|_| {
                // fallback without vec_f32
                tx.execute(
                    "INSERT OR REPLACE INTO vec_index (embedding, id, file, qualified_name) VALUES (?1, ?2, ?3, ?4)",
                    params![emb_json, id, rec.file, qn],
                )
            })?;

            // Also store chunk
            tx.execute(
                "INSERT OR REPLACE INTO chunks (id, function_id, chunk_idx, content, embedding) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![format!("{id}#0"), id, 0, chunk_trunc, emb_json],
            )?;

            pb.inc(1);
            pb.set_message(format!("{}::{}", rec.file, name));
        }
    }

    tx.commit()?;
    pb.finish_with_message(format!("Indexed {} functions → {}", total_funcs, db_path.display()));

    Ok(db_path)
}

pub fn index_directory(root: &Path) -> Result<PathBuf> {
    index_directory_with_model(root, None)
}
