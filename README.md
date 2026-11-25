# `rusty_scanner` & `rusty_triage` — Rusty Intelli Scanner Suite

High-performance, crash-recoverable video classification, interactive TUI manual triage, multi-anchor consolidation, and live HTTP drill-down audit suite built in Rust to orchestrate **`rusty_dedup`** (wrapped via **`scripts/rusty_dedup_metrics_harness.py`**) and **`rusty_clean`**.

---

## Architecture & End-to-End Pipeline

```text
  Miscellaneous Directories / Video Files
                    │
                    ▼
   ┌───────────────────────────────────────┐
   │  rusty_scanner (Two-Channel + NLP)    │──(--dry-run)──► anchor_matches.csv + un.txt
   └───────────────────┬───────────────────┘                                     │
                       │                                                         ▼
                       │                                         ┌───────────────────────────────┐
                       │                                         │  rusty_triage (Ratatui TUI)   │
                       │                                         │  • Smart Suggestions [1]-[5]  │
                       │                                         │  • Assign File / Whole Dir(D) │
                       │                                         │  • Live Alias Auto-Sweep (a)  │
                       │                                         │  • Create New Anchor (n)      │
                       │                                         └───────────────┬───────────────┘
                       │◄────────────(--from-csv manual_matches.csv / [X])───────┘
                       ▼
   ┌───────────────────────────────────────────────────────────────────────────┐
   │  Live Consolidation Orchestrator (Per Rank #1 Anchor Batch)               │
   │  1. Invoke `rusty_dedup` via `rusty_dedup_metrics_harness.py`             │
   │     on `<anchor_root> <file1> <file2> ...` (3-layer Prometheus metrics)   │
   │  2A. 0 Duplicates  ──► Direct atomic/streamed move to `<destination>`     │
   │  2B. Has Duplicates ─► Crash-safe Temp-Hold park unrelated videos ──►     │
   │                        `rusty_clean -t <destination>` ──► Restore parked  │
   │  3. Create deduplicated softlinks in Rank #2–#4 secondary anchors         │
   │  4. Archive `dedup/*.json` + `clean/*.json` + update HTML report live     │
   └───────────────────────────────────┬───────────────────────────────────────┘
                                       ▼
   ┌───────────────────────────────────────────────────────────────────────────┐
   │  Persistent Audit Store (`./reports/run_<timestamp>_<pid>/`)              │
   │  + Built-in HTTP Report Server (`0.0.0.0:8787`, Auto-Refresh every 5s)    │
   └───────────────────────────────────────────────────────────────────────────┘
```

---

## Feature Highlights

### 1. Non-Short-Circuiting Two-Channel Matcher & NLP Noun Weighting
- **Zero-Short-Circuiting Two-Channel Evidence Fusion**: Every candidate video is always evaluated in full across *both* channels:
  - **Directory Channel (`S_dir`)**: Evaluates enclosing directory names from the innermost parent outward **up to and including** the passed miscellaneous directory's own basename (`dir_weight: 0.97`).
  - **Title Channel (`S_title`)**: Evaluates filename clauses, contiguous whole-word token phrases, camelCase/PascalCase splits (`StudioAlpha` / `studioAlpha`), spaceless compact affixes, and weighted Jaro-Winkler + Levenshtein fuzzy clauses (`title_weight: 0.95`).
  - **Corroboration Boost (`DIR_AND_TITLE`)**: When both directory and filename corroborate the same anchor, `corroboration_boost: 0.08` elevates confidence above single-channel matches:
    $$\text{score} = \min\Big(1.0,\ \max(w_{\text{dir}} S_{\text{dir}},\ w_{\text{title}} S_{\text{title}}) + b_{\text{corr}} \min(S_{\text{dir}}, S_{\text{title}})\Big)$$
- **NLP Part-of-Speech (POS) Noun Weighting (`nlp:` in `scanner_config.yaml`)**:
  - Classifies tokens into 6 POS categories so Proper Nouns and Nouns carry far more weight than incidental descriptive words:
    - `ProperNoun` (`1.00`) • `Noun` (`0.85`) • `Verb` (`0.50`) • `Adjective` (`0.45`) • `Honorific` (`0.35`) • `FunctionWord` (`0.15`)
  - Prevents false positives on generic adjectives/verbs in titles (e.g., `"Live Concert"` will not false-match an anchor named `live_h265`) and prevents shared honorifics (`"Doctor Grant"` vs `"Doctor Green"`) from inflating fuzzy similarity while still catching genuine typos (`"Professor Allexander Stone"`).
- **Built-In Precision Guards**:
  - **Short-Acronym Boundary Guard**: Short aliases like `SX` match `[SX] Episode_01.mp4`, never substrings inside words like `sxsw_panel.mp4`.
  - **Letter-Spaced Acronym Guard**: Stylized anchors like `S T A R_h265` match `S.T.A.R.`, never collapsing to match the ordinary English word `"Star"`.
  - **Sequel / Episode Number Guard**: Blocks fuzzy cross-matching between numbered sequels (`Episode 2` never fuzzy-matches `Episode 1`).
  - **Technical Tag Normalization**: Strips encoding/resolution noise tokens (`h265`, `x265`, `hevc`, `1080p`, `2160p`, `4k`, `60fps`, `aac`, etc.) while still honoring raw alias matches.

---

### 2. Multi-Anchor Collaborations, Custom Destinations & Rank #2–#4 Symlinks
- **Map-Style `anchors.yaml`**: Each anchor absolute path supports optional `aliases` and a custom `destination` (relative subfolder such as `incoming/2026` or any external absolute directory path).
- **Deterministic 4-Way Ranking**:
  1. Highest fused `confidence_score` (DESC)
  2. Earliest `clause_position` in title/path (ASC)
  3. Longest matched alias alphanumeric length (DESC)
  4. Declaration order in `anchors.yaml` (ASC)
- **Automated Co-Star Linking**:
  - **Rank #1 (`PRIMARY_MOVE`)**: Deduplicates against `<anchor_root>` and consolidates the surviving winner into `<destination>`.
  - **Ranks #2 .. #(1 + `max_secondary_symlinks`) (`SYMLINK`)**: Creates collision-safe softlinks in secondary anchors' `<destination>` pointing directly to Rank #1's surviving higher-quality winner file (skipping automatically if an equivalent inode link already exists).

---

### 3. Crash-Safe Consolidation, Temp-Hold Parking & Metrics Harness
- **Full Prometheus / Grafana Metrics Harness Wrapping**:
  - Every `rusty_dedup` invocation in both `rusty_scanner` and `rusty_triage` automatically runs through `scripts/rusty_dedup_metrics_harness.py` (`tools.dedup_metrics_harness: auto`, configurable `harness_linger_secs`), capturing all 3 telemetry layers:
    1. Rust 500ms lock-free domain snapshot
    2. OS-kernel thread & `ffmpeg` child process inspection
    3. Host CPU, RAM, and GPU metrics
- **Targeted Per-File Deduplication (`ARG_MAX`-Safe)**:
  - Invokes `rusty_dedup <anchor_root> <matched_file_1> <matched_file_2> ...` directly on matched files (never scanning unrelated files in `misc_dir`), automatically splitting massive batches below OS `ARG_MAX`.
- **Move-Never-Copy Fast Path**:
  - When `rusty_dedup` finds `0` duplicates, files move directly to `<destination>` via instant same-filesystem `rename()` or cross-volume 4 MiB streaming + `fsync()` + source unlink (rolling back partial destination writes if unlink fails so duplicates are never left behind).
- **Crash-Recoverable Temp-Hold Parking**:
  - When duplicates are detected, unrelated non-duplicate videos in shared parent folders (or `<anchor_root>` when using a custom `destination`) are temporarily parked in a same-volume sibling `.rusty_scanner_hold_*` directory backed by an `fsync`ed `hold_manifest.json` before `rusty_clean -t <destination>` runs, then restored immediately.
  - If a run is interrupted (`Ctrl-C`, SSH drop, power loss), `recover_stranded_temp_holds` automatically restores any parked files from dead PIDs at the start of the next run.

---

### 4. `rusty_triage` — Standalone Interactive TUI for Unmatched Videos
A full-screen `ratatui` + `crossterm` terminal UI purpose-built to triage `un.txt` / `misc_files.txt` (or scan directories directly):
- **Dual Browse Modes (`t`)**: Switch anytime between a collapsible **Parent Directory Tree** (automatically stripping shared root prefixes in list rows while showing full paths in the footer) and a **Flat File List**, with `/` live substring search and `Space` multi-select.
- **Smart Suggestions (`[1]`–`[5]`)**: Instant one-keystroke assignment powered by `src/suggestions.rs`, which strips numeric/date prefixes (`001_`, `2026-01-15_`), matches multi-word initials (`DBZ` $\rightarrow$ `Dragon Ball Z_h265`, `SXO` $\rightarrow$ `Studio X Official_h265`), and normalizes honorific/possessive variants (`Dr Grants` $\rightarrow$ `Doctor Grant_h265`).
- **Whole Parent Directory Assignment (`D`)**: Assigns every video inside the current folder to an anchor in one keystroke—guarded by a confirmation modal if you press `D` on a shared top-level root folder (like `/downloads`) so you never accidentally sweep unrelated batches.
- **Live Alias Learning + Rayon Auto-Sweep (`a` / `Ctrl-a`)**: Atomically appends a new alias to `anchors.yaml` (preserving comments and formatting), reloads the matcher, and **automatically sweeps all remaining unassigned files in parallel via Rayon**.
- **On-the-Fly New Anchor Creation (`n` / `Ctrl-n`)**: Infers root volumes (e.g., `/Volumes/DriveA/Media`, `/Volumes/DriveB/Archive`) from `anchors.yaml`, pre-fills `<Creator>_h265`, creates the folder on disk, updates `anchors.yaml`, and assigns + auto-sweeps matching files.
- **Unlimited Undo (`u`)**, session resume from existing `manual_matches.csv`, CSV export (`w`), and direct live execution (`X`).

---

### 5. Persistent Job Report Store & 3-Level Drill-Down HTML Dashboard
- **Zero Deleted Job Reports**: Every execution permanently archives all raw job outputs inside `./reports/run_<YYYYMMDD_HHMMSS>_<pid>/`:
  - `dedup/<seq>_p<pass>_c<chunk>_<anchor>_dups.json` — Pristine `rusty_dedup` output JSON (saved even when 0 duplicates are found; `rusty_clean` runs against an ephemeral scratch copy so `dedup/*_dups.json` is never mutated).
  - `clean/<seq>_p<pass>_c<chunk>_<anchor>_clean.json` — Full `rusty_clean -o` structured decision report JSON.
  - `dedup/<seq>_..._pairs.json` — Opt-in pair-level dHash frame match % and sliding-window offsets when `reports.capture_pair_categories_log: true`.
  - `run_manifest.json` & `execution_report.html` — Flushed atomically after **every single anchor batch**, so even an interrupted run preserves complete audit history up to that batch.
- **Interactive Self-Contained HTML5 + CSS + JS Dashboard**:
  - **Level 0 — Clickable KPI Cards**: Filter instantly by *Anchor Batches*, *New Videos Moved (+GB)*, *Duplicate Groups*, *Anchor Upgrades ★*, *Disk Space Reclaimed*, or *Rank #2–#4 Symlinks*.
  - **Level 1 — Instant Search, Filter & Sort Toolbar**: Search across anchors, filenames, matched aliases, tiers, and codecs (`AV1`, `H265`, `H264`, `WMV`); sort by reclaimed disk space, upgrades, dup count, moved count, execution order, or name; toggle Dark/Light theme.
  - **Level 2 — Per-Anchor Collapsible Accordions**: Tabbed drill-down into **Duplicate Groups**, **New Videos Moved** (both direct moves and `rusty_clean` Phase-2 moves), and **Rank #2–#4 Symlinks**, plus 1-click links to raw `dups.json` and `clean.json`.
  - **Level 3 — Side-by-Side 8-Attribute Score Comparison**: Visual score bars (`X.XXXX / 9.0`) comparing `★ WINNER` vs `✖ DELETED` copies across V-Codec, Resolution, V-Bitrate (`2×` weight), Audio Codec & Bitrate, Runtime, and File Size, with automatic outcome badges:
    - `★ ANCHOR UPGRADED` (incoming misc copy beat existing anchor copy)
    - `✓ EXISTING ANCHOR KEPT` (anchor copy beat incoming misc duplicate)
    - `⇄ MISC BATCH DEDUPED` (duplicate incoming misc files consolidated)
    - `⚙ INTRA-ANCHOR CLEANUP` (pre-existing duplicates inside anchor resolved)
- **Offline HTML Rebuild (`--build-report <RUN_DIR>`)**: Regenerates `execution_report.html` anytime from `run_manifest.json` or directly from raw `dedup/*.json` + `clean/*.json` files.

---

### 6. Built-In Lightweight HTTP Report Server & Auto-Refresh
- Configured under `report_server:` in `scanner_config.yaml` (`listen_addr: 0.0.0.0:8787`, `live_refresh_secs: 5`):
  - **`GET /`** — **Execution Reports Hub**: Lists all live and historical runs with roll-up KPIs and automatic periodic refresh.
  - **`GET /latest`** — Redirects directly to the newest run's `execution_report.html`.
  - **`GET /<run_id>/...`** — Serves dashboards and raw JSONs with `Cache-Control: no-store` and strict `403 Forbidden` path-traversal protection.
- **Seamless Background & 24/7 Modes**:
  - Automatically runs in a background thread during live executions (`serve_during_run: true`), skipping cleanly if a persistent server is already bound to `listen_addr`.
  - Run continuously across SSH disconnects via `./scripts/report_server.sh start` or `rusty_scanner --serve-reports` (also writes `reports/index.html` so even `python3 -m http.server` works out of the box).
- **Automatic Periodic Browser Refresh**: While a run is `IN_PROGRESS`, the browser automatically refreshes KPIs and anchor cards every `live_refresh_secs` seconds without collapsing open accordions or clearing active search/filters (with a topbar **`⟳ Auto-Refresh: ON (5s)`** pause/resume toggle and automatic `location.reload()` fallback).

---

## Build & Cross-Compilation

```bash
cd /Users/maildev/coding/rusty_scanner

# Native optimized release build
cargo build --release

# Cross-platform release packaging (macOS, Linux, Windows + SHA256SUMS.txt)
./scripts/build_binaries.sh macos arm64
./scripts/build_binaries.sh all
```

Compiled binaries:
- `target/release/rusty_scanner`
- `target/release/rusty_triage`

---

## Typical End-to-End Workflows

```bash
cd /Users/maildev/coding/rusty_scanner

# 1. Start the persistent HTTP report server (viewable from any browser on LAN/VPN at http://<host>:8787/)
./scripts/report_server.sh start

# 2. Optional dry-run preview -> writes anchor_matches.csv + un.txt without touching video files
./target/release/rusty_scanner -a anchors.yaml -c scanner_config.yaml \
  --dry-run -o anchor_matches.csv -u un.txt \
  "/Volumes/DriveA/Incoming/Downloads/"

# 3. Live automated consolidation -> runs rusty_dedup (via metrics harness) + rusty_clean,
#    saves all dedup/clean JSONs + live auto-refreshing HTML report under ./reports/, and writes un.txt
./target/release/rusty_scanner -a anchors.yaml -c scanner_config.yaml -u un.txt \
  "/Volumes/DriveA/Incoming/Downloads/"

# 4. Interactive TUI triage for remaining unmatched files in un.txt
#    (Press `X` inside the TUI to execute immediately, or `w` to save manual_matches.csv)
./target/release/rusty_triage -a anchors.yaml -c scanner_config.yaml \
  -i un.txt -o manual_matches.csv -u un_remaining.txt

# 5. Execute saved manual_matches.csv later (if you saved with `w` instead of pressing `X`)
./target/release/rusty_scanner -a anchors.yaml -c scanner_config.yaml \
  --from-csv manual_matches.csv

# 6. Rebuild HTML report offline from any saved run directory
./target/release/rusty_scanner --build-report ./reports/run_<YYYYMMDD_HHMMSS>_<pid>
```

---

## CLI Reference

### `rusty_scanner`

| Option / Argument | Description |
| :--- | :--- |
| `[INPUTS]...` | Miscellaneous video directories (scanned recursively) and/or individual video files |
| `-a, --anchors <PATH>` | Map-style anchors YAML file (default: `anchor_dirs.yaml`) |
| `-c, --config <PATH>` | Scanner configuration YAML (defaults to `./scanner_config.yaml` or `<exe_dir>/scanner_config.yaml`) |
| `--dry-run` | Preview matches into CSV (`-o`) and unmatched list (`-u`) without moving or deleting files |
| `--from-csv <CSV_PATH>` | Execute curated match assignments from a CSV (e.g., `manual_matches.csv` from `rusty_triage`) |
| `-o, --csv-out <PATH>` | Output CSV path (`anchor_matches.csv` default in `--dry-run`; pass `-` for stdout; optional audit CSV in live mode) |
| `-u, --unmatched-out <PATH>` | Output file listing remaining unmatched video paths (default: `misc_files.txt`) |
| `--report-dir <DIR>` | Override `reports.output_dir` for persistent `dedup/` & `clean/` JSONs and HTML reports |
| `--build-report <RUN_DIR>` | Rebuild `execution_report.html` from a saved `reports/run_*` directory and exit |
| `--serve-reports` | Run the standalone HTTP report server (`report_server` in `scanner_config.yaml`) |
| `--listen <HOST:PORT>` | Override `report_server.listen_addr` (e.g., `0.0.0.0:9090`) |
| `-v, --verbose` | Print detailed discovery, matching, Temp-Hold parking, and subprocess command logs |

### `rusty_triage`

| Option / Argument | Description |
| :--- | :--- |
| `[INPUTS]...` | Optional miscellaneous directories/files to scan when `-i` is omitted |
| `-a, --anchors <PATH>` | Map-style anchors YAML (auto-detects `anchors.yaml` if present, else `anchor_dirs.yaml`) |
| `-c, --config <PATH>` | Optional path to `scanner_config.yaml` |
| `-i, --input-list <TXT>` | Unmatched file list (`un.txt` / `misc_files.txt`; auto-detected in CWD if omitted) |
| `-o, --output-csv <CSV>` | Output CSV for staged assignments (default: `manual_matches.csv`) |
| `-u, --unmatched-out <TXT>` | Optional path to write remaining unassigned files when saving/executing |
| `--report-dir <DIR>` | Override `reports.output_dir` when executing immediately with `X` |
| `-v, --verbose` | Enable verbose diagnostics during live execution (`X`) |

### `rusty_triage` Keybindings

| Key | Action |
| :--- | :--- |
| `↑`/`↓` or `j`/`k`, `PgUp`/`PgDn`, Mouse Wheel | Navigate unmatched tree (Left) or anchor search list (Right) |
| `←`/`→` or `h`/`l` | Collapse / expand parent directory group |
| `t` | Toggle between **Parent Directory Tree** and **Flat File** views |
| `Tab` / `f` / Mouse Click | Switch focus between Unmatched Pane and Anchor Search Pane |
| `/` | Filter unmatched folders/files by substring (`Esc` clears) |
| Type in Right Pane | Live fuzzy search across all anchor names, paths, and aliases (`Esc` clears) |
| `1`–`5` | Quick-assign current file, directory, or selection to Smart Suggestion `[1]`–`[5]` |
| `Space` | Toggle multi-selection on a file or entire directory |
| `Enter` | Assign highlighted/selected item(s) to the highlighted Anchor |
| `D` | Assign the file's **entire parent directory** to the highlighted Anchor (with shared-root guard) |
| `a` / `Ctrl-a` | Add new alias to highlighted Anchor in `anchors.yaml` & auto-sweep remaining unmatched files |
| `n` / `Ctrl-n` | Create a brand-new anchor folder on a chosen root volume, update `anchors.yaml`, and assign + sweep |
| `H` | Toggle hiding already-assigned items |
| `u` / `Backspace` | Undo last action / clear assignment on highlighted item |
| `w` / `Ctrl-s` | Save staged assignments to `manual_matches.csv` |
| `X` / `Ctrl-x` | Save & immediately execute staged assignments via `rusty_dedup_metrics_harness.py` + `rusty_dedup` + `rusty_clean` |
| `q` / `Ctrl-q` | Quit (prompts to save if unsaved assignments exist) |

---

## Configuration Summary (`scanner_config.yaml`)

- **`matching:`** — Tier base weights (`exact_clause_score`, `whole_token_score`, `compact_affix_score`, `fuzzy_*`), channel fusion weights (`dir_weight`, `title_weight`, `corroboration_boost`), `max_secondary_symlinks` (`3`), and `noise_tokens`.
- **`nlp:`** — POS tag weights (`proper_noun_weight`, `noun_weight`, `verb_weight`, `adjective_weight`, `honorific_weight`, `function_word_weight`), `nlp_influence`, and `min_effective_score`.
- **`tools:`** — `python_bin`, `dedup_metrics_harness` (`auto`), `harness_linger_secs`, `rusty_dedup_bin` (`auto`), `rusty_clean_bin` (`auto`), `dedup_config` (`auto`), `decision_config` (`auto`), and `states_dir` (`.states`).
- **`reports:`** — `enabled` (`true`), `output_dir` (`./reports`), and `capture_pair_categories_log` (`false`).
- **`report_server:`** — `enabled` (`true`), `listen_addr` (`0.0.0.0:8787`), `serve_during_run` (`true`), `linger_after_run_secs` (`0`), and `live_refresh_secs` (`5`).
