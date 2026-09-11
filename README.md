# reko

**Polyglot code extractor — one CLI, 15 languages, exact IR.**

`reko` walks any repository, respects `.gitignore`, skips package/build dirs, reads files byte-exact (tabs, `CRLF`, spaces preserved), and emits a single **hierarchical JSONL** `folders → files → methods` via a `reader → ExtractionFactory` pipeline. Install once, run anywhere: `reko extract .`.

> **What it does:** For every supported file under a directory, it extracts **every function/method** into the IR defined in `supportive/IR_function.json` (`identity / source {hash,location} / declaration / signature / context / body / ...` – 215-field spec) and writes one JSON line per file.

---

## Features

- **15 languages:** `Java`, `Python`, `C`, `C++`, `PHP`, `JavaScript`, `JSX` (React), `C#`, `TypeScript`, `TSX` (React), `Go`, `Rust`, `Swift`, `Ruby`, `Kotlin` (13 classic + 2 frontend).
- **Exact reader:** `src/reader.rs` preserves `\t`, spaces, `\n` vs `\r\n` vs `\r`, trailing-no-newline.
- **ExtractionFactory:** one extractor per language under `src/ExtractionFactory/*.rs` (brace/indent/`def→end` aware, string/comment aware, `sha256` + `line/col/byte` location, qualified names like `pkg::Class::func`).
- **Orchestrator:** `src/orchestrator.rs` + `src/scan.rs` → `reader → extractor` per file.
- **Repository walk:** `ignore` crate respects `.gitignore` + `.git/info/exclude` + global, hard-coded package ignores (`node_modules,target,dist,build,vendor,__pycache__,.venv,.next,coverage,...`), skips symlinks, parallel `rayon` (all cores).
- **Output:** single `reko.jsonl` in repo root (or `--output` file/folder) — each line `{file, language, count, functions: [IR...]}` sorted hierarchically, plus stats + unsupported breakdown.
- **Cross-platform:** Windows / Linux / macOS, `clap` derive, `cargo build --release` LTO+stripped.

---

## Install

### From source (recommended)
```bash
git clone https://github.com/sakhadib/Reko.git && cd Reko
cargo build --release          # binary → target/release/reko
cargo install --path .         # installs `reko` to ~/.cargo/bin
reko --help
```

### From crates.io (after publish)
```bash
cargo install reko
```

### From GitHub Releases (binaries)
Releases publish `reko-linux-x86_64`, `reko-windows-x86_64.exe`, `reko-macos-{x86_64,aarch64}` on tag `v*`. Download from **Releases** and add to `PATH`.

### Model (auto, global)
`reko index` needs `embeddinggemma-300m-ONNX` (q4, 768d, ~200M). No manual setup:
- First `reko index` auto-downloads to global cache `~/.cache/reko/model` (or `~/.reko/model`) via `hf` / `hf-hub` — one-time, reused for **any repo** you index.
- Override with `reko index --model /path/to/model` or `REKO_MODEL=/path`.
- `reko extract` needs no model.

### Cross-compilation
```bash
rustup target add x86_64-unknown-linux-gnu x86_64-pc-windows-msvc aarch64-apple-darwin
cargo build --release --target x86_64-unknown-linux-gnu
```

---

## Usage

### Extract a repository (hierarchical JSONL)
```bash
# In any repo, from its root:
reko extract .                          # → ./.reko/reko.jsonl (15 files, 61 funcs)

# From anywhere, pointing at a repo:
reko extract --path /path/to/repo       # → /path/to/repo/.reko/reko.jsonl

# Custom output (still respects .gitignore + package ignores):
reko extract . --output /tmp/out        # → /tmp/out/reko.jsonl
reko extract . --output /tmp/result.jsonl  # → file
```

**What you get (`.reko/reko.jsonl`):** JSON Lines, one line per file:
```json
{"file":"src/foo.py","language":"python","count":2,"absolute":"/abs/src/foo.py","functions":[{"ir_version":"1.0","identity":{"id":"foo::MyClass::bar","name":"bar","qualified_name":"foo::MyClass::bar","kind":"function","language":"python",...},"source":{"file":"src/foo.py","module":"foo","source_text":"def bar...","hash":"sha256:...","location":{"start":{"line":3,"column":5,"byte":42},"end":{"line":5,"column":12,"byte":110}}},...}]}
```
Folders are implicit via `file` path (sorted). Unsupported files are **skipped and reported**.

### Index with vectors (global model, one-time download)
```bash
reko index .                            # uses .reko/reko.jsonl else auto-extracts → .reko/reko.db (sqlite-vec 768d)
reko index --path /path/to/repo         # from anywhere, same global model ~/.cache/reko/model
reko index --model ./model --force      # override model, force re-index
# Model auto-downloaded on first index to ~/.cache/reko/model (embeddinggemma-300m q4, ~200M)
```

### Semantic find (top 5 formatted)
```bash
reko find "add two numbers"                     # → top 5 in CWD, shows file, qualified_name, lines, distance + bar
reko find "sort array" --top 10                 # top 10
reko find "reverse string" --top 3 --path /repo # from anywhere, uses repo's .reko/reko.db
reko find "factorial" --path ManualTest         # semantic over code chunks (task: code retrieval)
```
Output:
```
  ▸ REKO FIND  "add two numbers"  — top 3 in /repo
  1. com.example.test::TestFunctions::add (java)  █████ ... 0.540
     sample.kt  add  sample.kt:4–4  distance 0.4544
     ───────────────────────────────────────
  2. ns::Foo::add (cpp)  ...  sample.cpp:5–5
```

### Extract a single file
```bash
reko extract path/to/File.java               # → pretty JSON to stdout
reko extract path/to/File.java --output out.json
reko extract path/to/File.java --compact     # compact JSON
```

### Other commands
```bash
reko read path/to/file --help   # exact content with line numbers (tabs/CRLF preserved)
reko cat path/to/file           # alias for read
reko --help
reko --version
```

### Ignore behavior
- Respects every `.gitignore` (per-dir, parents, global, `.git/info/exclude`).
- Always skips package dirs even if not gitignored: `.git,.hg,.svn,node_modules,target,dist,build,vendor,__pycache__,.venv,venv,.idea,.vscode,.next,out,coverage,.pytest_cache,.mypy_cache,.gradle,.parcel-cache,.turbo` etc.
- Skips symlinks.
- Reports `ignored package entries`, `unsupported` (e.g. `md:10 json:2`), `errors`, `total functions`.

---

## Supported languages & extensions

| Language | Extensions | Example |
|----------|------------|---------|
| Java | `.java` | `public int foo(int a)` |
| Python | `.py` | `def foo(a: int) -> str` |
| C | `.c .h` | `int foo(int a)` |
| C++ | `.cpp .cc .cxx .hpp .hh` | `template <T> T foo(T)` |
| PHP | `.php` | `public function foo(): int` |
| JavaScript | `.js .mjs .cjs` | `function foo(a,b)` |
| JSX | `.jsx` | `function Comp(){return <div>}` (only JSX bodies, non-JSX ignored) |
| C# | `.cs` | `public int Foo<T>(int a)` |
| TypeScript | `.ts .mts .cts` | `function foo<T>(a: T): T` |
| TSX | `.tsx` | `const C: React.FC<Props> = ()=><div>` |
| Go | `.go` | `func (r Recv) Foo[T any](x T)` |
| Rust | `.rs` | `pub async fn foo<T>(x: T)` |
| Swift | `.swift` | `func foo<T>(x: T) async throws` |
| Ruby | `.rb` | `def foo(a,b=1,*args)` |
| Kotlin | `.kt .kts` | `suspend fun String.foo()` |

Add a new one: add `src/ExtractionFactory/myExtractor.rs` with `pub fn extract(content: &str, path: &Path) -> Result<Vec<IrFunction>>`, register in `src/ExtractionFactory/mod.rs`, dispatch in `src/orchestrator.rs`.

---

## Development

```bash
cargo test          # 83 tests (reader + 13 extractors + scan)
cargo build
cargo run -- extract ManualTest --output /tmp/out  # manual hierarchy test
cargo run -- read ManualTest/TestFunctions.java | head
```

Project layout:
```
src/main.rs                         # CLI (+ extract / index)
src/reader.rs                       # exact read
src/ir.rs                           # IR spec mirror
src/orchestrator.rs                 # reader → ExtractionFactory
src/scan.rs                         # walker (ignore+rayon) → .reko/reko.jsonl
src/model.rs                        # global model resolver + hf auto-download → ~/.cache/reko/model
src/embed.rs                        # EmbeddingGemma ONNX (q4, 768d, prefix + L2)
src/index.rs                        # .reko/reko.db sqlite-vec (vec0)
src/ExtractionFactory/{java,python,c,cpp,php,js,jsx,csharp,ts,tsx,go,rust,swift,ruby,kotlin}Extractor.rs
supportive/IR_function.json         # IR spec
model/                              # gitignored, dev cache; prod uses ~/.cache/reko/model
```

CI: `.github/workflows/ci.yml` builds `ubuntu/windows/macos` + release artifacts on `v*` tags.

---

## License
MIT — see [LICENSE](LICENSE)
