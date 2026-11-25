Rusty Intelli Scanner Suite

### An app to automatically organize your video library

Automated video library intake, intelligent classification, interactive terminal triage, visual duplicate consolidation, and live browser-based audit reporting—built in Rust.

---

## Table of Contents
1. [What It Is](#1-what-it-is)
2. [Why Would Anyone Use It?](#2-why-would-anyone-use-it)
3. [Killer Features Under the Hood](#3-killer-features-under-the-hood)
4. [Tour of Binary Components & Core Concepts](#4-tour-of-binary-components--core-concepts)
5. [Architecture & End-to-End Pipeline](#5-architecture--end-to-end-pipeline)
6. [Technical Feature Highlights](#6-technical-feature-highlights)
7. [Build & Cross-Compilation](#7-build--cross-compilation)
8. [Quick-Start & End-to-End Workflows](#8-quick-start--end-to-end-workflows)
9. [CLI & Keybinding Reference](#9-cli--keybinding-reference)

---

## 1. What It Is

Imagine you maintain a large, organized video collection spread across one or more hard drives or NAS volumes—neatly sorted into dedicated folders by creator, studio, channel, show, or series—and you regularly have a messy "incoming" pile of newly downloaded or unsorted video files.

Those incoming files rarely have clean names. Some use abbreviations (`[SX] Ep01_1080p_HEVC.mkv`), some mash words together (`StudioAlpha_LiveConcert_4K.mp4`), some have typos, some feature multiple creators at once, and some are actually **re-encoded duplicates** of videos you already archived months ago (either worse quality you don't want, or higher-resolution upgrades you *do* want).

The **Rusty Intelli Scanner Suite** takes that messy pile and turns library organization into a safe, repeatable, mostly hands-free pipeline:

1. **Classifies automatically (`rusty_scanner`)**: Reads both folder structures and filenames using linguistic rules and fuzzy matching to figure out which destination folder(s) in your library each incoming video belongs to.
2. **Deduplicates visually & upgrades quality**: Coordinates companion visual-deduplication and quality-scoring tools (`rusty_dedup` and `rusty_clean`) so incoming videos are compared **frame-by-frame** against what you already own—keeping whichever copy has the better video/audio quality and discarding redundant copies.
3. **Links multi-folder videos without wasting space**: If a video belongs in two or three folders at once (such as a collaboration or crossover), it stores the real file in the primary folder and places lightweight shortcuts (symlinks) in the others.
4. **Makes leftover mystery files effortless (`rusty_triage`)**: Any file too ambiguous to auto-sort is handed to an interactive, keyboard-driven terminal app where you can assign files with a single keypress, teach the scanner new nicknames on the fly, and watch it automatically sweep similar files in real time.
5. **Records a visual receipt for everything**: Hosts a live-updating web dashboard you can open in any browser to inspect every moved file, reclaimed gigabyte, and side-by-side quality comparison.

---

## 2. Why Would Anyone Use It?

If you have ever tried to manage terabytes of video files with basic shell scripts, file-renamers, or manual drag-and-drop, you quickly run into five painful problems:

- **Naive filename scripts misfire constantly**: Simple keyword matching gets fooled by codec tags (`x265`, `1080p`), common English words (a clip titled `"Live Concert"` accidentally matching a folder named `Live`), shared titles (`"Doctor Grant"` fuzzy-matching `"Doctor Green"`), or short acronyms (`SX` matching inside `sxsw_panel.mp4`).
- **Filename checks cannot spot visual duplicates—or quality upgrades**: An incoming file named `concert_final_v2.mp4` (`2160p AV1`) might be the exact same footage as `2021_live_show.avi` (`720p H264`) already sitting in your archive. Skipping by name misses the duplicate; blindly overwriting risks replacing a crisp `4K` master with a blurry `720p` re-encode.
- **Collaborations eat disk space**: Copying a `5 GB` collaboration video into three participating studios' folders wastes `10 GB` of storage; maintaining symlinks manually across drives is tedious and error-prone.
- **Long batch runs across external drives are fragile and opaque**: Terminal output scrolls past and vanishes, leaving you wondering *why* a file was deleted—and an accidental `Ctrl-C`, SSH disconnect, or power blip halfway through a multi-drive batch can leave files stranded in limbo.
- **The "last 5%" bottleneck**: Even when automation handles 95% of your queue, manually opening Finder/Explorer windows to sort the remaining 200 cryptically named files takes hours.

**Rusty Intelli Scanner** solves all five: high-precision linguistic matching in front, frame-accurate visual deduplication and quality upgrading in the middle, a 1-keystroke learning TUI for edge cases, crash-recoverable file handling on disk, and a permanent interactive HTML audit trail at the end.

---

## 3. Killer Features Under the Hood

Here is what happens behind the scenes to keep your library clean without babysitting:

- **Linguistic "Common Sense" (Two-Channel + NLP Noun Weighting)**
  Instead of stopping at the first regex hit, the matcher evaluates *both* the parent folder hierarchy and the filename, then weighs words by English Part-of-Speech. Proper nouns and concrete nouns carry heavy weight (`StudioAlpha`, `Dragon`), while generic verbs, adjectives, and role titles (`Live`, `New`, `Doctor`, `Professor`) are discounted—eliminating maddening false positives while still catching genuine spelling mistakes.
- **Automatic Archive Quality Upgrades**
  When an incoming video matches an existing video's visual content, every copy is scored across **8 media attributes** (video codec, resolution, video bitrate, audio codec, audio bitrate, runtime, and file size). If the new file is better, your library gets **automatically upgraded (`★ ANCHOR UPGRADED`)**; if your existing copy is already better, the inferior incoming duplicate is cleanly discarded (`✓ EXISTING ANCHOR KEPT`).
- **Instant Moves & Crash-Proof "Temp-Hold" Parking**
  When no visual duplicates exist, `rusty_scanner` skips heavy cleanup passes entirely—moving files instantaneously via atomic filesystem `rename()` on the same drive (or verified `fsync` streaming across drives) so bytes aren't needlessly copied twice. When duplicates *do* require cleanup inside a shared folder, unrelated bystander videos are temporarily tucked into a journaled `.rusty_scanner_hold_*` safety folder and **automatically restored on the next launch** even if your machine loses power mid-run.
- **Teach Once, Auto-Sweep Everywhere (`rusty_triage`)**
  Inside the interactive triage app, when you teach an anchor a new nickname or acronym (press `a`), it writes the alias cleanly into your `anchors.yaml` (preserving all your comments) and **immediately re-scans every remaining unmatched file across all CPU cores**, sweeping away dozens of related files in milliseconds.
- **Zero-Waste Multi-Anchor Symlinking**
  Videos matching multiple creators or series are ranked deterministically. Rank #1 receives the surviving master file; Ranks #2–#4 automatically receive collision-safe symbolic links pointing straight to Rank #1's winning file.
- **Live Browser Dashboard + Permanent Paper Trail**
  Open `http://<host>:8787` from your desktop, laptop, or phone while a batch is running. The dashboard auto-refreshes every 5 seconds without losing your scroll position or open tabs, showing clickable KPI cards and visual score bars for every decision—backed by raw JSON files that are archived forever.

---

## 4. Tour of Binary Components & Core Concepts

If you are brand new to this repository, start here—this section explains every program involved and every term used throughout the documentation.

### Part A: Binary & Script Components

The suite consists of **two primary Rust binaries built from this repository**, which orchestrate **three companion tools** during live consolidation:

| Component | Where It Lives | Purpose in Plain English |
| :--- | :--- | :--- |
| **`rusty_scanner`** | Built here (`target/release/rusty_scanner`) | **The Automated Engine & Web Server.** Scans unsorted video directories, scores and ranks matches against your library, outputs dry-run CSV previews, orchestrates live deduplication/moves/symlinks, rebuilds offline HTML dashboards, and serves reports over HTTP. |
| **`rusty_triage`** | Built here (`target/release/rusty_triage`) | **The Interactive Terminal UI (TUI).** Full-screen keyboard/mouse app for sorting leftover unmatched videos. Offers `[1]`–`[5]` smart suggestions, whole-folder assignment (`D`), live alias learning + auto-sweep (`a`), on-disk new folder creation (`n`), undo (`u`), and 1-key live execution (`X`). |
| **`rusty_dedup`** | Companion binary (auto-discovered via `scanner_config.yaml`) | **The Visual Duplicate Detector.** Decodes video frames and compares perceptual visual fingerprints (`dHash`) with sliding-window time alignment. Finds duplicate videos even if renamed, re-encoded (`H264` vs `HEVC` vs `AV1`), resized, or slightly trimmed, writing `*_dups.json`. |
| **`rusty_clean`** | Companion binary (auto-discovered via `scanner_config.yaml`) | **The Quality Judge & Consolidator.** Reads `rusty_dedup`'s duplicate groups, scores each copy across 8 video/audio quality metrics, keeps the highest-scoring winner, removes inferior duplicates, moves winners into the target folder, and writes `*_clean.json`. |
| **`rusty_dedup_metrics_harness.py`** | Companion Python wrapper (`auto` in `scanner_config.yaml`) | **The Live Telemetry Wrapper.** Launches `rusty_dedup` while exposing a Prometheus `/metrics` endpoint that captures internal Rust progress, OS threads / `ffmpeg` child processes, and host CPU/RAM/GPU usage. |
| **`scripts/report_server.sh`** & **`scripts/build_binaries.sh`** | Included helper scripts (`./scripts/`) | `report_server.sh` starts/stops `rusty_scanner --serve-reports` in the background (surviving SSH disconnects); `build_binaries.sh` compiles and packages release tarballs/zips with SHA-256 checksums for macOS, Linux, and Windows. |

> **Note:** You can use `--dry-run` in `rusty_scanner` and assign/organize/export CSVs in `rusty_triage` **without** needing `rusty_dedup` or `rusty_clean` installed. Those companion binaries are only invoked when you run live file consolidation.

---

### Part B: Core Concepts (Plain-English Glossary)

- **Anchor (Anchor Directory)**
  A permanent, organized destination folder in your library—for example, `/Volumes/DriveA/Anime/DBZ_h265` or `/Volumes/DriveB/StudioX`. It is called an *anchor* for two reasons: (1) incoming unsorted clips "gravitate" toward it when matched, and (2) the existing videos already inside it act as the reference baseline when checking for duplicates.
- **`anchors.yaml` (Aliases & Custom Destinations)**
  The YAML map listing all your anchor directories. Under each anchor path, you can optionally list:
  - **`aliases`**: Alternate names, acronyms, or spellings that also belong to this anchor (e.g., `["DragonBall Z", "DBZ"]`). Note: the folder's own name—both raw and with technical tags like `_h265` stripped—is always included automatically.
  - **`destination`**: Optional sub-folder (such as `incoming/2026`) or external path where newly consolidated files should be placed, while still deduplicating against the entire anchor root! See [`anchor_example.yaml`](anchor_example.yaml) for a clean template.
- **Miscellaneous ("Misc") Input & Unmatched List (`un.txt` / `misc_files.txt`)**
  "Misc" simply means the unsorted folders or video files you point `rusty_scanner` at. Any video that does not clear the minimum match confidence score is left untouched and written to an **unmatched list** (`un.txt` or `misc_files.txt`), which you can feed straight into `rusty_triage`.
- **Multi-Anchor Ranking (`PRIMARY_MOVE` vs. `SYMLINK`)**
  A single video can match more than one anchor (for instance, `"StudioAlpha & StudioBeta - Special.mp4"`). Rather than guessing randomly or copying the file twice, `rusty_scanner` ranks all matching anchors from #1 downward:
  - **Rank #1 (`PRIMARY_MOVE`)**: Deduplicates against Rank #1's anchor folder and stores the physical winning video file there.
  - **Ranks #2–#4 (`SYMLINK`)**: Creates lightweight filesystem symbolic links inside the secondary anchors pointing to Rank #1's physical file.
- **Configuration (`scanner_config.yaml`)**
  Controls how strict or forgiving matching is, which technical tags (`1080p`, `h265`, `webrip`, `60fps`) get ignored as noise, Part-of-Speech word weights, where companion binaries live (`auto` finds them automatically), where audit reports are stored, and what port the web dashboard listens on. See [`scanner_config.yaml`](scanner_config.yaml).
- **Temp-Hold Parking (`.rusty_scanner_hold_*`)**
  Because `rusty_clean` operates at the directory level when resolving duplicates, unrelated videos sitting in the same incoming folder could otherwise be swept along. Before `rusty_clean` runs, `rusty_scanner` instantaneously parks unrelated files in a hidden same-drive `.rusty_scanner_hold_*` folder with an `fsync`ed manifest (`hold_manifest.json`), and moves them right back afterward. If a crash interrupts the run, the next startup reads the manifest and restores them automatically.
- **Telemetry (3-Layer Prometheus / Grafana Metrics)**
  When running large batches, `rusty_scanner` wraps `rusty_dedup` in `rusty_dedup_metrics_harness.py` so your monitoring dashboard sees three synchronized layers of live health metrics:
  1. **Application Layer**: Lock-free 500ms snapshots of `rusty_dedup`'s internal indexing, hashing, and comparison counters.
  2. **Process/Kernel Layer**: OS thread states and spawned `ffmpeg` video decoder subprocess counts.
  3. **Hardware Layer**: Host CPU utilization, RAM footprint, and GPU usage.
  A configurable `harness_linger_secs` setting briefly holds the `/metrics` endpoint open after fast anchor batches finish so Prometheus never misses the final totals between scrape intervals.
- **Audit Store & Run Manifest (`./reports/run_<timestamp>_<pid>/`)**
  Every live execution creates a timestamped run folder containing every raw `dedup/*.json` and `clean/*.json` file, a master `run_manifest.json`, and a self-contained `execution_report.html` dashboard—updated atomically after every single anchor batch.

---

## 5. Architecture & End-to-End Pipeline

### How a Video Travels Through the System

1. **Discovery & Classification**: Point `rusty_scanner` at one or more unsorted directories. Use `--dry-run` first if you want a zero-risk preview (`anchor_matches.csv` + `un.txt`), or run live mode directly.
2. **Optional Interactive Triage**: Open leftover unmatched files (`un.txt`) in `rusty_triage`. Assign files via `[1]`–`[5]` suggestions, whole-directory shortcuts (`D`), or new aliases (`a`), then either press `X` to execute immediately or `w` to save `manual_matches.csv` for `rusty_scanner --from-csv`.
3. **Per-Anchor Batch Consolidation**: Matches are grouped by their **Rank #1** anchor and processed one anchor batch at a time:
   - `rusty_dedup` (via the metrics harness) compares the matched incoming files against existing videos inside `<anchor_root>`.
   - **Fast Path (0 Duplicates)**: Incoming files move straight into `<destination>` without touching unrelated files.
   - **Dedup Path (Duplicates Found)**: Unrelated bystander files are parked in Temp-Hold $\rightarrow$ `rusty_clean` scores every duplicate copy, keeps the highest-quality winner in `<destination>`, and deletes inferior copies $\rightarrow$ parked files are immediately restored.
   - **Secondary Symlinks**: Collision-safe symlinks to the surviving file are created in any **Rank #2–#4** co-anchors.
4. **Live Archiving & Web Reporting**: After *every* anchor batch finishes, raw JSON reports are archived and `execution_report.html` refreshes live via the built-in HTTP server (`0.0.0.0:8787`).

```text
  Unsorted ("Misc") Video Directories / Files
                     │
                     ▼
    ┌────────────────────────────────────────┐
    │  rusty_scanner (Two-Channel + NLP)     │──(--dry-run)──► anchor_matches.csv + un.txt
    └───────────────────┬────────────────────┘                                    │
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
    │  Live Consolidation Orchestrator (Grouped Per Rank #1 Anchor Batch)       │
    │  1. Invoke `rusty_dedup` via `rusty_dedup_metrics_harness.py`             │
    │     on `<anchor_root> <file1> <file2> ...` (3-layer Prometheus telemetry) │
    │  2A. 0 Duplicates  ──► Instant atomic rename / fsync stream to `<dest>`   │
    │  2B. Has Duplicates ─► Crash-safe Temp-Hold park unrelated videos ──►     │
    │                        `rusty_clean -t <dest>` (8-metric quality winner)  │
    │                        ──► Restore parked bystander videos                │
    │  3. Create deduplicated symlinks inside Rank #2–#4 secondary anchors      │
    │  4. Archive `dedup/*.json` + `clean/*.json` + flush HTML report live      │
    └───────────────────────────────────┬───────────────────────────────────────┘
                                        ▼
    ┌───────────────────────────────────────────────────────────────────────────┐
    │  Persistent Audit Store (`./reports/run_<timestamp>_<pid>/`)              │
    │  + Built-in HTTP Report Server (`0.0.0.0:8787`, Auto-Refresh every 5s)    │
    └───────────────────────────────────────────────────────────────────────────┘
```

---

## 6. Technical Feature Highlights

### 6.1 Non-Short-Circuiting Two-Channel Matcher & NLP Noun Weighting
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

### 6.2 Deterministic Multi-Anchor Ranking & Secondary Symlinks
- **4-Way Tie-Breaking Order**:
  1. Highest fused `confidence_score` (DESC)
  2. Earliest `clause_position` in title/path (ASC)
  3. Longest matched alias alphanumeric length (DESC)
  4. Declaration order in `anchors.yaml` (ASC)
- **Collision-Safe Symlink Creation**: Secondary anchors (`Ranks #2 .. #(1 + max_secondary_symlinks)`) receive clean symlinks pointing directly to Rank #1's surviving file—automatically skipping if an equivalent inode link already exists and appending numeric suffixes rather than clobbering unrelated filenames.

### 6.3 Crash-Safe Consolidation, `ARG_MAX` Chunking & Fast Moves
- **Targeted Per-File Deduplication (`ARG_MAX`-Safe)**: Invokes `rusty_dedup <anchor_root> <matched_file_1> <matched_file_2> ...` strictly on matched files (never scanning unrelated files sitting in the same unsorted folder) and automatically chunks massive batches well below OS command-line `ARG_MAX` limits.
- **Move-Never-Copy Fast Path**: When `0` duplicates are detected, files move via instant same-filesystem `rename()` or cross-volume 4 MiB streaming + `fsync()` + source unlink (rolling back partial destination writes if source unlink fails so half-written duplicates are never left behind).
- **Automatic Stranded Hold Recovery**: On startup, `recover_stranded_temp_holds` inspects `.rusty_scanner_hold_*` directories across target volumes, verifies whether the owning PID is no longer alive, and restores all parked files from `hold_manifest.json` before new work begins.

### 6.4 `rusty_triage` Interactive TUI Engine
- **Dual Browse Modes (`t`)**: Switch anytime between a collapsible **Parent Directory Tree** (stripping shared root prefixes in list rows while showing full paths in the footer) and a **Flat File List**, with `/` live substring filtering and `Space` multi-select.
- **Smart Suggestions (`[1]`–`[5]`)**: Powered by `src/suggestions.rs`, which strips numeric/date prefixes (`001_`, `2026-01-15_`), expands multi-word initials (`DBZ` $\rightarrow$ `Dragon Ball Z_h265`, `SXO` $\rightarrow$ `Studio X Official_h265`), and normalizes honorific/possessive variants (`Dr Grants` $\rightarrow$ `Doctor Grant_h265`).
- **Shared-Root Safety Guard on Whole-Folder Assign (`D`)**: Prompts a confirmation modal if you press `D` on a top-level shared parent folder (like `/downloads`) so you never accidentally sweep unrelated sub-batches into one anchor.
- **Live Alias Learning + Rayon Parallel Sweep (`a` / `Ctrl-a`)**: Non-destructively updates `anchors.yaml`, rebuilds the matcher in memory, and sweeps all remaining unassigned files in parallel via Rayon.
- **On-the-Fly New Anchor Creation (`n` / `Ctrl-n`)**: Infers root library volumes (e.g., `/Volumes/DriveA/Media`, `/Volumes/DriveB/Archive`) from `anchors.yaml`, pre-fills `<Name>_h265`, creates the directory on disk, registers it in `anchors.yaml`, and assigns + auto-sweeps matching files.

### 6.5 Persistent Job Report Store & 3-Level Drill-Down HTML Dashboard
- **Immutable Raw Job Outputs** inside `./reports/run_<YYYYMMDD_HHMMSS>_<pid>/`:
  - `dedup/<seq>_p<pass>_c<chunk>_<anchor>_dups.json` — Pristine `rusty_dedup` output JSON (saved even when 0 duplicates are found; `rusty_clean` runs against a scratch copy so `dedup/*_dups.json` is never mutated).
  - `clean/<seq>_p<pass>_c<chunk>_<anchor>_clean.json` — Full `rusty_clean -o` structured decision report JSON.
  - `dedup/<seq>_..._pairs.json` — Optional pair-level dHash frame match % and sliding-window offsets (`reports.capture_pair_categories_log: true`).
  - `run_manifest.json` & `execution_report.html` — Flushed atomically after every anchor batch.
- **Interactive Self-Contained HTML5 + CSS + JS Dashboard**:
  - **Level 0 — Clickable KPI Cards**: Filter instantly by *Anchor Batches*, *New Videos Moved (+GB)*, *Duplicate Groups*, *Anchor Upgrades ★*, *Disk Space Reclaimed*, or *Rank #2–#4 Symlinks*.
  - **Level 1 — Instant Search, Filter & Sort Toolbar**: Search across anchors, filenames, matched aliases, match tiers, and codecs (`AV1`, `H265`, `H264`, `WMV`); sort by reclaimed space, upgrades, duplicate count, moved count, execution order, or name; toggle Dark/Light theme.
  - **Level 2 — Per-Anchor Collapsible Accordions**: Tabbed drill-down into **Duplicate Groups**, **New Videos Moved**, and **Rank #2–#4 Symlinks**, plus 1-click links to raw `dups.json` and `clean.json`.
  - **Level 3 — Side-by-Side 8-Attribute Score Comparison**: Visual score bars (`X.XXXX / 9.0`) comparing `★ WINNER` vs `✖ DELETED` copies across V-Codec, Resolution, V-Bitrate (`2×` weight), Audio Codec & Bitrate, Runtime, and File Size, badged automatically as:
    - `★ ANCHOR UPGRADED` (incoming copy beat existing anchor copy)
    - `✓ EXISTING ANCHOR KEPT` (existing anchor copy beat incoming duplicate)
    - `⇄ MISC BATCH DEDUPED` (duplicate incoming files consolidated)
    - `⚙ INTRA-ANCHOR CLEANUP` (pre-existing duplicates inside the anchor resolved)
- **Offline HTML Rebuild (`--build-report <RUN_DIR>`)**: Regenerates `execution_report.html` anytime from `run_manifest.json` or directly from raw `dedup/*.json` + `clean/*.json` files.

### 6.6 Built-In Lightweight HTTP Report Server & Live Auto-Refresh
- Configured under `report_server:` in `scanner_config.yaml` (`listen_addr: 0.0.0.0:8787`, `live_refresh_secs: 5`):
  - **`GET /`** — **Execution Reports Hub**: Lists all live and historical runs with roll-up KPIs and periodic refresh.
  - **`GET /latest`** — Redirects straight to the newest run's `execution_report.html`.
  - **`GET /<run_id>/...`** — Serves dashboards and raw JSONs with `Cache-Control: no-store` and strict `403 Forbidden` path-traversal protection.
- **Background & 24/7 Modes**: Automatically hosts during live runs (`serve_during_run: true`, skipping cleanly if a persistent server is already bound to `listen_addr`), or runs 24/7 via `./scripts/report_server.sh start` / `rusty_scanner --serve-reports`.
- **State-Preserving Browser Auto-Refresh**: While a run is `IN_PROGRESS`, KPIs and anchor cards refresh every `live_refresh_secs` seconds without collapsing open accordions or clearing active search filters (with a **`⟳ Auto-Refresh: ON (5s)`** pause/resume toggle).

---

## 7. Build & Cross-Compilation

Requires a standard Rust toolchain (`rustup` / `cargo`).

```bash
cd rusty_scanner

# Native optimized release build (LTO + stripped symbols)
cargo build --release

# Cross-platform release packaging (macOS, Linux, Windows + SHA256SUMS.txt)
./scripts/build_binaries.sh macos arm64
./scripts/build_binaries.sh all
```

Compiled binaries:
- `target/release/rusty_scanner`
- `target/release/rusty_triage`

---

## 8. Quick-Start & End-to-End Workflows

1. **Create your `anchors.yaml`**: Copy [`anchor_example.yaml`](anchor_example.yaml) to `anchors.yaml` and list the absolute paths to your library's destination folders (plus any optional `aliases` or `destination` sub-folders).
2. **Review `scanner_config.yaml`**: Defaults work out of the box; `tools:` (`auto`) automatically locates `rusty_dedup`, `rusty_clean`, and `rusty_dedup_metrics_harness.py` if installed alongside or on your `PATH`.

```bash
# 1. (Optional) Start the persistent HTTP report server — view at http://<host>:8787/
./scripts/report_server.sh start

# 2. Safe Dry-Run Preview -> writes anchor_matches.csv + un.txt without touching any video files
./target/release/rusty_scanner -a anchors.yaml -c scanner_config.yaml \
  --dry-run -o anchor_matches.csv -u un.txt \
  "/Volumes/DriveA/Incoming/Downloads/"

# 3. Live Automated Consolidation -> runs visual dedup + quality cleanup + fast moves + symlinks,
#    archives all JSONs + live auto-refreshing HTML report under ./reports/, and writes un.txt
./target/release/rusty_scanner -a anchors.yaml -c scanner_config.yaml -u un.txt \
  "/Volumes/DriveA/Incoming/Downloads/"

# 4. Interactive TUI Triage for remaining unmatched files in un.txt
#    (Press `X` inside the TUI to execute immediately, or `w` to save manual_matches.csv)
./target/release/rusty_triage -a anchors.yaml -c scanner_config.yaml \
  -i un.txt -o manual_matches.csv -u un_remaining.txt

# 5. Execute saved manual_matches.csv later (if you saved with `w` instead of pressing `X`)
./target/release/rusty_scanner -a anchors.yaml -c scanner_config.yaml \
  --from-csv manual_matches.csv

# 6. Rebuild an HTML dashboard offline anytime from a saved run directory
./target/release/rusty_scanner --build-report ./reports/run_<YYYYMMDD_HHMMSS>_<pid>
```

---

## 9. CLI & Keybinding Reference

### `rusty_scanner` CLI Options

| Option / Argument | Description |
| :--- | :--- |
| `[INPUTS]...` | Unsorted video directories (scanned recursively) and/or individual video files |
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

### `rusty_triage` CLI Options

| Option / Argument | Description |
| :--- | :--- |
| `[INPUTS]...` | Optional unsorted directories/files to scan directly when `-i` is omitted |
| `-a, --anchors <PATH>` | Map-style anchors YAML (auto-detects `anchors.yaml` if present, else `anchor_dirs.yaml`) |
| `-c, --config <PATH>` | Optional path to `scanner_config.yaml` |
| `-i, --input-list <TXT>` | Unmatched file list (`un.txt` / `misc_files.txt`; auto-detected in CWD if omitted) |
| `-o, --output-csv <CSV>` | Output CSV for staged assignments (default: `manual_matches.csv`) |
| `-u, --unmatched-out <TXT>` | Optional path to write remaining unassigned files when saving/executing |
| `--report-dir <DIR>` | Override `reports.output_dir` when executing immediately with `X` |
| `-v, --verbose` | Enable verbose diagnostics during live execution (`X`) |

### `rusty_triage` Interactive Keybindings

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

### Configuration Summary (`scanner_config.yaml`)

- **`matching:`** — Tier base weights (`exact_clause_score`, `whole_token_score`, `compact_affix_score`, `fuzzy_*`), channel fusion weights (`dir_weight`, `title_weight`, `corroboration_boost`), `max_secondary_symlinks` (`3`), and `noise_tokens`.
- **`nlp:`** — POS tag weights (`proper_noun_weight`, `noun_weight`, `verb_weight`, `adjective_weight`, `honorific_weight`, `function_word_weight`), `nlp_influence`, and `min_effective_score`.
- **`tools:`** — `python_bin`, `dedup_metrics_harness` (`auto`), `harness_linger_secs`, `rusty_dedup_bin` (`auto`), `rusty_clean_bin` (`auto`), `dedup_config` (`auto`), `decision_config` (`auto`), and `states_dir` (`.states`).
- **`reports:`** — `enabled` (`true`), `output_dir` (`./reports`), and `capture_pair_categories_log` (`false`).
- **`report_server:`** — `enabled` (`true`), `listen_addr` (`0.0.0.0:8787`), `serve_during_run` (`true`), `linger_after_run_secs` (`0`), and `live_refresh_secs` (`5`).
