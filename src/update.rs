use crate::embed::Embedder;
use crate::index::open_db;
use crate::scan::{self, FileRecord};
use anyhow::{Context, Result};
use rusqlite::params;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct UpdateStats {
    pub deleted_files: usize,
    pub added_files: usize,
    pub deleted_funcs: usize,
    pub added_funcs: usize,
    pub modified_funcs: usize,
    pub total_funcs: usize,
}

pub fn update_directory(root: &Path, model_path: Option<&Path>) -> Result<UpdateStats> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let reko_dir = root.join(".reko");
    std::fs::create_dir_all(&reko_dir)?;
    let jsonl = reko_dir.join("reko.jsonl");
    let old_jsonl = reko_dir.join("reko-old.jsonl");
    let db_path = reko_dir.join("reko.db");

    if !jsonl.exists() {
        anyhow::bail!("no .reko/reko.jsonl at {} — run `reko extract` or `reko index` first", jsonl.display());
    }
    if !db_path.exists() {
        anyhow::bail!("no .reko/reko.db at {} — run `reko index` first", db_path.display());
    }

    // 1. Copy current to old
    std::fs::copy(&jsonl, &old_jsonl).with_context(|| format!("copy {} to {}", jsonl.display(), old_jsonl.display()))?;
    eprintln!("Saved backup → {}", old_jsonl.display());

    // 2. Make new reko.jsonl via scan
    eprintln!("Re-extracting {} ...", root.display());
    let (new_records, stats) = scan::scan_directory(&root)?;
    scan::write_jsonl(&jsonl, &new_records)?;
    eprintln!(
        "New extract: {} files → {} functions → {}",
        stats.files_with_functions, stats.total_functions, jsonl.display()
    );

    // 3. Load old and new for comparison
    let old_records = load_jsonl(&old_jsonl)?;
    let new_records = new_records; // already have

    // Build maps
    let old_by_file: HashMap<String, &FileRecord> = old_records.iter().map(|r| (r.file.clone(), r)).collect();
    let new_by_file: HashMap<String, &FileRecord> = new_records.iter().map(|r| (r.file.clone(), r)).collect();

    let old_files: HashSet<String> = old_by_file.keys().cloned().collect();
    let new_files: HashSet<String> = new_by_file.keys().cloned().collect();

    let deleted_files: Vec<String> = old_files.difference(&new_files).cloned().collect();
    let added_files: Vec<String> = new_files.difference(&old_files).cloned().collect();

    eprintln!(
        "Diff: {} deleted files, {} added files, {} common",
        deleted_files.len(),
        added_files.len(),
        old_files.intersection(&new_files).count()
    );

    // Open DB
    let mut conn = open_db(&db_path)?;

    // Progress for DB updates
    use indicatif::{ProgressBar, ProgressStyle};
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg} [{elapsed_precise}]")
            .unwrap()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
    );
    pb.enable_steady_tick(std::time::Duration::from_millis(80));

    // 4 & 5. Deleted files -> remove from DB
    let mut stats_out = UpdateStats::default();
    stats_out.deleted_files = deleted_files.len();
    stats_out.added_files = added_files.len();

    let tx = conn.transaction()?;
    for file in &deleted_files {
        let n1 = tx.execute("DELETE FROM functions WHERE file = ?1", params![file])?;
        let n2 = tx.execute("DELETE FROM vec_index WHERE file = ?1", params![file])?;
        tx.execute("DELETE FROM files WHERE file = ?1", params![file])?;
        tx.execute("DELETE FROM chunks WHERE file = ?1", params![file]).ok();
        tx.execute("DELETE FROM chunks WHERE function_id LIKE ?1", params![format!("{file}%")]).ok();
        stats_out.deleted_funcs += n1;
        pb.set_message(format!("Deleted file: {file}"));
        eprintln!("  - deleted file {} ({} funcs, {} vecs)", file, n1, n2);
    }

    // For added files, we will handle via added funcs loop below (all funcs are added)
    // For common files, detect added/deleted/modified funcs
    // Collect all added/modified funcs to re-embed
    struct Pending {
        file: String,
        func: crate::ir::IrFunction,
    }
    let mut to_embed: Vec<Pending> = Vec::new();
    let mut to_delete_ids: Vec<String> = Vec::new();

    for file in old_files.intersection(&new_files) {
        let old_rec = old_by_file.get(file).unwrap();
        let new_rec = new_by_file.get(file).unwrap();

        let old_map: HashMap<String, &crate::ir::IrFunction> = old_rec
            .functions
            .iter()
            .map(|f| (f.identity.qualified_name.clone(), f))
            .collect();
        let new_map: HashMap<String, &crate::ir::IrFunction> = new_rec
            .functions
            .iter()
            .map(|f| (f.identity.qualified_name.clone(), f))
            .collect();

        let old_qns: HashSet<String> = old_map.keys().cloned().collect();
        let new_qns: HashSet<String> = new_map.keys().cloned().collect();

        // Deleted funcs
        for qn in old_qns.difference(&new_qns) {
            if let Some(f) = old_map.get(qn) {
                let hash_suffix = f.source.hash.get(7..15).unwrap_or(&f.source.hash);
                let db_id = format!("{}::{}::{}", file, f.identity.id, hash_suffix);
                to_delete_ids.push(db_id);
                stats_out.deleted_funcs += 1;
            }
        }
        // Added funcs
        for qn in new_qns.difference(&old_qns) {
            if let Some(f) = new_map.get(qn) {
                to_embed.push(Pending {
                    file: file.clone(),
                    func: (*f).clone(),
                });
                stats_out.added_funcs += 1;
            }
        }
        // Modified funcs: same qn but hash/string/inputs differ
        for qn in old_qns.intersection(&new_qns) {
            let old_f = old_map.get(qn).unwrap();
            let new_f = new_map.get(qn).unwrap();
            if is_modified(old_f, new_f) {
                let hash_suffix = old_f.source.hash.get(7..15).unwrap_or(&old_f.source.hash);
                let old_db_id = format!("{}::{}::{}", file, old_f.identity.id, hash_suffix);
                to_delete_ids.push(old_db_id);
                to_embed.push(Pending {
                    file: file.clone(),
                    func: (*new_f).clone(),
                });
                stats_out.modified_funcs += 1;
            }
        }
    }

    // Added files: all funcs are added
    for file in &added_files {
        if let Some(rec) = new_by_file.get(file) {
            for func in &rec.functions {
                to_embed.push(Pending {
                    file: file.clone(),
                    func: func.clone(),
                });
                stats_out.added_funcs += 1;
            }
        }
    }

    // Delete old modified/deleted funcs
    for db_id in &to_delete_ids {
        tx.execute("DELETE FROM functions WHERE id = ?1", params![db_id])?;
        tx.execute("DELETE FROM vec_index WHERE id = ?1", params![db_id])?;
        tx.execute("DELETE FROM chunks WHERE function_id = ?1", params![db_id])?;
        tx.execute("DELETE FROM chunks WHERE id = ?1", params![format!("{db_id}#0")])?;
    }

    // Need to also insert/update files table for added/modified files
    // For added files, insert files entry; for modified, count already in new_records
    // We'll upsert files for all new files
    for file in added_files.iter().chain(
        old_files
            .intersection(&new_files)
            .filter(|f| {
                // only if file had modified/added funcs
                new_by_file.get(*f).unwrap().functions.iter().any(|func| {
                    to_embed.iter().any(|p| p.file == **f && p.func.identity.qualified_name == func.identity.qualified_name)
                })
            }),
    ) {
        if let Some(rec) = new_by_file.get(file) {
            tx.execute(
                "INSERT OR REPLACE INTO files (file, language, count, absolute) VALUES (?1, ?2, ?3, ?4)",
                params![rec.file, rec.language, rec.count as i64, rec.absolute],
            )?;
        }
    }
    // Ensure added files' files entry
    for rec in new_records.iter().filter(|r| added_files.contains(&r.file)) {
        tx.execute(
            "INSERT OR REPLACE INTO files (file, language, count, absolute) VALUES (?1, ?2, ?3, ?4)",
            params![rec.file, rec.language, rec.count as i64, rec.absolute],
        )?;
    }

    // Prepare embedder if needed
    let need_embed = !to_embed.is_empty();
    let mut embedder_opt = if need_embed {
        Some(crate::embed::Embedder::new().context("load embedder for update")?)
    } else {
        None
    };

    // Progress bar for embedding
    let pb2 = if need_embed {
        let pb = ProgressBar::new(to_embed.len() as u64);
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) {eta_precise} {msg}",
            )
            .unwrap()
            .progress_chars("█▉▊▋▌▍▎▏  "),
        );
        pb.set_message("Re-embedding modified/added...");
        Some(pb)
    } else {
        None
    };

    // Insert added/modified funcs
    for pending in to_embed {
        let func = &pending.func;
        let file = &pending.file;
        let base_id = &func.identity.id;
        let hash_suffix = func.source.hash.get(7..15).unwrap_or(&func.source.hash);
        let db_id = format!("{}::{}::{}", file, base_id, hash_suffix);
        let qn = func.identity.qualified_name.clone();
        let name = func.identity.name.clone();
        let lang = func.identity.language.clone();
        let src = func.source.source_text.clone();
        let hash = func.source.hash.clone();
        let sl = func.source.location.start.line as i64;
        let el = func.source.location.end.line as i64;

        tx.execute(
            "INSERT OR REPLACE INTO functions (id, file, qualified_name, language, name, source_text, hash, start_line, end_line) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![db_id, file, qn, lang, name, src, hash, sl, el],
        )?;

        // Embed
        let chunk_content = if func.source.normalized_source.trim().is_empty() {
            src.clone()
        } else {
            func.source.normalized_source.clone()
        };
        let chunk_trunc = if chunk_content.len() > 8000 {
            chunk_content.chars().take(8000).collect::<String>()
        } else {
            chunk_content
        };

        let vec = if let Some(emb) = embedder_opt.as_mut() {
            match emb.embed_code_chunks(&[chunk_trunc.clone()]) {
                Ok(mut v) => v.into_iter().next().unwrap_or_else(|| vec![0.0; 768]),
                Err(e) => {
                    eprintln!("warn: embed failed for {}: {}", db_id, e);
                    vec![0.0; 768]
                }
            }
        } else {
            vec![0.0; 768]
        };

        let emb_json = serde_json::to_string(&vec)?;
        tx.execute(
            "INSERT OR REPLACE INTO vec_index (embedding, id, file, qualified_name) VALUES (vec_f32(?1), ?2, ?3, ?4)",
            params![emb_json, db_id, file, qn],
        )
        .or_else(|_| {
            tx.execute(
                "INSERT OR REPLACE INTO vec_index (embedding, id, file, qualified_name) VALUES (?1, ?2, ?3, ?4)",
                params![emb_json, db_id, file, qn],
            )
        })?;
        tx.execute(
            "INSERT OR REPLACE INTO chunks (id, function_id, chunk_idx, content, embedding) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![format!("{db_id}#0"), db_id, 0, chunk_trunc, emb_json],
        )?;

        if let Some(pb) = &pb2 {
            pb.inc(1);
            pb.set_message(format!("{file}::{name}"));
        }
    }

    if let Some(pb) = pb2 {
        pb.finish_with_message(format!(
            "Updated {} modified, {} added, {} deleted funcs",
            stats_out.modified_funcs, stats_out.added_funcs, to_delete_ids.len()
        ));
    }

    tx.commit()?;
    pb.finish_with_message("Update complete");

    // Update total_funcs
    let total_funcs: i64 = {
        let conn2 = open_db(&db_path)?;
        conn2
            .query_row("SELECT COUNT(*) FROM functions", [], |r| r.get(0))
            .unwrap_or(0)
    };
    stats_out.total_funcs = total_funcs as usize;

    eprintln!(
        "Update done: {} deleted files, {} added files, {} deleted funcs, {} added funcs, {} modified funcs, total {} funcs",
        stats_out.deleted_files,
        stats_out.added_files,
        stats_out.deleted_funcs,
        stats_out.added_funcs,
        stats_out.modified_funcs,
        stats_out.total_funcs
    );

    Ok(stats_out)
}

fn is_modified(old: &crate::ir::IrFunction, new: &crate::ir::IrFunction) -> bool {
    if old.source.hash != new.source.hash {
        return true;
    }
    if old.source.source_text != new.source.source_text {
        return true;
    }
    if old.source.normalized_source != new.source.normalized_source {
        return true;
    }
    if old.signature.parameters != new.signature.parameters {
        return true;
    }
    if old.signature.return_type != new.signature.return_type {
        return true;
    }
    if old.signature.throws != new.signature.throws {
        return true;
    }
    // Also check body raw
    if old.body.raw != new.body.raw {
        return true;
    }
    false
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
