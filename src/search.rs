use crate::embed::Embedder;
use crate::index::open_db;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub struct SearchResult {
    pub file: String,
    pub qualified_name: String,
    pub name: String,
    pub language: String,
    pub start_line: i64,
    pub end_line: i64,
    pub distance: f64,
}

pub fn search(
    root: &Path,
    query: &str,
    top_n: usize,
    model: Option<&Path>,
) -> Result<Vec<SearchResult>> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let db_path = root.join(".reko").join("reko.db");
    if !db_path.exists() {
        anyhow::bail!(
            "no index at {} — run `reko index {}` first",
            db_path.display(),
            root.display()
        );
    }

    let mut embedder = if let Some(mp) = model {
        let mf = mp.join("onnx").join("model_q4.onnx");
        let mf2 = mp.join("model_q4.onnx");
        let mf = if mf.exists() { mf } else { mf2 };
        let tf = mp.join("tokenizer.json");
        let tf2 = mp.join("onnx").join("tokenizer.json");
        let tf = if tf.exists() { tf } else { tf2 };
        Embedder::from_paths(Some(&mf), Some(&tf)).context("load embedder")?
    } else {
        Embedder::new().context("load embedder (check ./model or ~/.cache/reko/model)")?
    };

    let qvec = embedder.embed_query(query)?;
    let q_json = serde_json::to_string(&qvec)?;

    let conn = open_db(&db_path)?;

    // sqlite-vec KNN: requires `k = ?` (or LIMIT literal). Use k for parameterized top_n.
    let mut stmt = conn
        .prepare(
            "SELECT v.id, v.file, v.qualified_name, f.name, f.language, f.start_line, f.end_line, v.distance \
             FROM vec_index v JOIN functions f ON v.id = f.id \
             WHERE v.embedding MATCH vec_f32(?1) AND k = ?2 ORDER BY distance",
        )
        .or_else(|_| {
            conn.prepare(
                "SELECT v.id, v.file, v.qualified_name, f.name, f.language, f.start_line, f.end_line, v.distance \
                 FROM vec_index v JOIN functions f ON v.id = f.id \
                 WHERE v.embedding MATCH ?1 AND k = ?2 ORDER BY distance",
            )
        })?;

    let rows = stmt.query_map(
        rusqlite::params![q_json, top_n as i64],
        |row| {
            Ok(SearchResult {
                file: row.get(1)?,
                qualified_name: row.get(2)?,
                name: row.get(3)?,
                language: row.get(4)?,
                start_line: row.get(5)?,
                end_line: row.get(6)?,
                distance: row.get(7)?,
            })
        },
    )?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub fn search_and_print(root: &Path, query: &str, top_n: usize, model: Option<&Path>) -> Result<()> {
    let results = search(root, query, top_n, model)?;

    if results.is_empty() {
        println!("No results for \"{}\" (index may be empty, run `reko index`)", query);
        return Ok(());
    }

    // Header
    println!();
    println!("\x1b[1m\x1b[96m  ▸ REKO FIND  \x1b[0m\x1b[2m\"{}\"  — top {} in {}\x1b[0m", query, top_n, root.display());
    println!("\x1b[2m{}\x1b[0m", "─".repeat(72));

    for (i, r) in results.iter().enumerate() {
        let rank = i + 1;
        let score = 1.0 - r.distance; // cosine distance 0=identical, convert to similarity-ish
        let bar = {
            let pct = ((1.0 - r.distance.clamp(0.0, 2.0) / 2.0) * 100.0) as usize;
            let w = 18;
            let fill = pct * w / 100;
            format!("\x1b[92m{}\x1b[90m{}\x1b[0m", "█".repeat(fill), "░".repeat(w - fill))
        };
        // rank color
        let rank_c = match rank {
            1 => "\x1b[93m\x1b[1m",
            2 => "\x1b[97m",
            3 => "\x1b[90m",
            _ => "\x1b[2m",
        };
        println!(
            "  {}{:>2}.\x1b[0m \x1b[1m{}\x1b[0m \x1b[2m({})\x1b[0m  {}  \x1b[2m{:.3}\x1b[0m",
            rank_c,
            rank,
            r.qualified_name,
            r.language,
            bar,
            score
        );
        println!(
            "     \x1b[96m{}\x1b[0m  \x1b[33m{} \x1b[0m\x1b[2m{}:{}–{}\x1b[0m  \x1b[2mdistance {:.4}\x1b[0m",
            r.file, r.name, r.file, r.start_line, r.end_line, r.distance
        );
        if i + 1 < results.len() {
            println!("\x1b[2m{}\x1b[0m", "─".repeat(72));
        }
    }
    println!("\x1b[2m{}\x1b[0m", "─".repeat(72));
    println!(
        "\x1b[2m  tip: reko find \"<text>\" --top {}   reko find \"sort array\" --path /repo\x1b[0m\n",
        top_n
    );
    Ok(())
}
