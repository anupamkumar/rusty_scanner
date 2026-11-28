use crate::config::{
    AnchorDirsConfig, AnchorEntry, ResolvedTools, ScannerConfig, normalize_path_lexical,
    path_dev_inode,
};
use crate::csv_report::{print_match_summary, write_matches_csv, write_unmatched_files};
use crate::html_report::{
    AnchorBatchAudit, CleanGroupDecision, DedupReportFull, MovedVideoRecord, PairEvaluationEntry,
    RunReportStore, SymlinkRecord, correlate_duplicate_groups, is_pid_alive_fast,
};
use crate::matcher::{AnchorMatcher, FileMatchResult, PlannedAction};
use crate::report_server::BackgroundReportServer;
use crate::scanner::{TEMP_HOLD_PREFIX, discover_misc_videos, is_supported_video_file};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use walkdir::WalkDir;

/// Chunk size (4 MiB) for cross-volume file moves (matching `rusty_clean`).
const MOVE_COPY_CHUNK_SIZE: usize = 4 * 1024 * 1024;

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn handle_termination_signal(_sig: i32) {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
}

fn install_signal_handlers() {
    SHUTDOWN_REQUESTED.store(false, Ordering::SeqCst);
    #[cfg(unix)]
    unsafe {
        unsafe extern "C" {
            fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
        }
        const SIGHUP: i32 = 1;
        const SIGINT: i32 = 2;
        const SIGTERM: i32 = 15;
        let _ = signal(SIGHUP, handle_termination_signal);
        let _ = signal(SIGINT, handle_termination_signal);
        let _ = signal(SIGTERM, handle_termination_signal);
    }
}

fn is_shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::SeqCst)
}

const HOLD_MANIFEST_FILENAME: &str = "hold_manifest.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HoldManifestEntry {
    original_path: PathBuf,
    parked_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HoldManifest {
    pid: u32,
    entries: Vec<HoldManifestEntry>,
}

/// RAII crash-safe guard that parks unrelated non-duplicate files into sibling `.rusty_scanner_hold_*`
/// directories *outside* `misc_dir` (and `<anchor_root>` when custom `destination` is set) during
/// `rusty_clean`, and restores every parked file on completion or unwind/error.
struct TempHoldSession {
    hold_dirs: Vec<PathBuf>,
    parked: Vec<HoldManifestEntry>,
    restored: bool,
    verbose: bool,
}

impl TempHoldSession {
    fn new(verbose: bool) -> Self {
        Self {
            hold_dirs: Vec::new(),
            parked: Vec::new(),
            restored: false,
            verbose,
        }
    }

    /// Parks all regular video files under `scan_roots` that are NOT in `allowed_inodes`, NOT in
    /// `allowed_paths`, and NOT inside `target_dest_dir` into a same-volume sibling temp directory
    /// strictly outside `scan_root`.
    fn park_unrelated_videos(
        &mut self,
        scan_roots: &[PathBuf],
        allowed_inodes: &HashSet<(u64, u64)>,
        allowed_paths: &HashSet<PathBuf>,
        target_dest_dir: &Path,
        video_extensions: &HashSet<String>,
        seq_id: usize,
    ) -> Result<usize> {
        let pid = std::process::id();
        let norm_dest = normalize_path_lexical(target_dest_dir);
        let dest_inode = fs::metadata(&norm_dest)
            .ok()
            .map(|m| path_dev_inode(&norm_dest, &m));

        let mut hold_dir_by_parent: HashMap<PathBuf, PathBuf> = HashMap::new();
        let mut per_hold_entries: HashMap<PathBuf, Vec<HoldManifestEntry>> = HashMap::new();
        let mut seen_parked_inodes: HashSet<(u64, u64)> = HashSet::new();
        let mut parked_count = 0usize;

        // Collapse `scan_roots` to top-level non-overlapping ancestor roots so every `hold_parent`
        // (`top_root.parent()`) is guaranteed to sit strictly OUTSIDE all scanned roots!
        let norm_candidates: Vec<PathBuf> = scan_roots
            .iter()
            .filter(|r| r.exists())
            .map(|r| normalize_path_lexical(r))
            .collect();
        let mut top_roots: Vec<PathBuf> = Vec::new();
        for cand in &norm_candidates {
            let has_ancestor = norm_candidates
                .iter()
                .any(|other| other != cand && cand.starts_with(other));
            if !has_ancestor && !top_roots.contains(cand) {
                top_roots.push(cand.clone());
            }
        }

        for norm_root in top_roots {
            if are_same_dir(&norm_root, &norm_dest, dest_inode) {
                continue;
            }

            // Walk upward if needed so `hold_parent` is never inside any protected root
            let mut hold_parent = norm_root
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| norm_root.clone());
            while norm_candidates
                .iter()
                .any(|r| hold_parent != *r && hold_parent.starts_with(r))
            {
                if let Some(p) = hold_parent.parent().filter(|p| !p.as_os_str().is_empty()) {
                    hold_parent = p.to_path_buf();
                } else {
                    break;
                }
            }

            for entry in WalkDir::new(&norm_root)
                .follow_links(false)
                .into_iter()
                .filter_entry(|e| {
                    if e.depth() > 0 && e.file_type().is_dir() {
                        let name = e.file_name().to_string_lossy();
                        if name.starts_with(TEMP_HOLD_PREFIX) || name == ".states" {
                            return false;
                        }
                    }
                    true
                })
                .filter_map(|e| e.ok())
            {
                if !entry.file_type().is_file() {
                    continue;
                }
                let fpath = normalize_path_lexical(entry.path());
                if !is_supported_video_file(&fpath, video_extensions) {
                    continue;
                }

                // Never park files already inside `target_dest_dir`!
                if fpath.starts_with(&norm_dest) {
                    continue;
                }
                if let Some(fparent) = fpath.parent() {
                    if are_same_dir(fparent, &norm_dest, dest_inode) {
                        continue;
                    }
                }

                if allowed_paths.contains(&fpath) {
                    continue;
                }
                if let Ok(can_f) = fs::canonicalize(&fpath) {
                    if allowed_paths.contains(&can_f) {
                        continue;
                    }
                }

                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                if meta.len() == 0 {
                    continue;
                }
                let ino = path_dev_inode(&fpath, &meta);
                if allowed_inodes.contains(&ino) || !seen_parked_inodes.insert(ino) {
                    continue;
                }

                let hold_dir = hold_dir_by_parent
                    .entry(hold_parent.clone())
                    .or_insert_with(|| {
                        let d = hold_parent.join(format!(
                            "{}{}_{}_{}",
                            TEMP_HOLD_PREFIX,
                            pid,
                            seq_id,
                            hold_dir_by_parent_seed(&hold_parent)
                        ));
                        d
                    })
                    .clone();

                if !self.hold_dirs.contains(&hold_dir) {
                    fs::create_dir_all(&hold_dir).with_context(|| {
                        format!("Failed to create temp hold directory {:?}", hold_dir)
                    })?;
                    self.hold_dirs.push(hold_dir.clone());
                }

                let fname = fpath
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("parked.bin");
                let parked_path =
                     hold_dir.join(format!("{:06}_{}", parked_count + 1, fname));

                fs::rename(&fpath, &parked_path).with_context(|| {
                    format!(
                        "Failed to temporarily park unrelated file {:?} -> {:?} (same-volume rename)",
                        fpath, parked_path
                    )
                })?;

                let manifest_entry = HoldManifestEntry {
                    original_path: fpath,
                    parked_path,
                };
                self.parked.push(manifest_entry.clone());
                per_hold_entries
                    .entry(hold_dir)
                    .or_default()
                    .push(manifest_entry);
                parked_count += 1;
            }
        }

        // Persist `hold_manifest.json` in each hold dir BEFORE `rusty_clean` executes for crash safety
        for (hold_dir, entries) in &per_hold_entries {
            let manifest = HoldManifest {
                pid,
                entries: entries.clone(),
            };
            let manifest_path = hold_dir.join(HOLD_MANIFEST_FILENAME);
            let json = serde_json::to_string_pretty(&manifest)?;
            let mut f = File::create(&manifest_path)?;
            f.write_all(json.as_bytes())?;
            f.sync_all()?;
        }

        if self.verbose && parked_count > 0 {
            eprintln!(
                "  [Verbose][Temp-Hold] Parked {} unrelated non-duplicate video(s) into {:?}",
                parked_count, self.hold_dirs
            );
        }

        Ok(parked_count)
    }

    fn restore_all(&mut self) -> Result<usize> {
        if self.restored {
            return Ok(0);
        }
        let mut restored_count = 0usize;
        let mut first_err: Option<anyhow::Error> = None;

        for item in self.parked.drain(..).rev() {
            if !item.parked_path.exists() {
                continue;
            }
            if let Some(parent) = item.original_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            match fs::rename(&item.parked_path, &item.original_path) {
                Ok(_) => {
                    restored_count += 1;
                }
                Err(e) => {
                    let msg = format!(
                        "Failed to restore parked file {:?} -> {:?}: {}",
                        item.parked_path, item.original_path, e
                    );
                    eprintln!("ERROR: {}", msg);
                    if first_err.is_none() {
                        first_err = Some(anyhow::anyhow!(msg));
                    }
                }
            }
        }

        if first_err.is_none() {
            for hd in &self.hold_dirs {
                let manifest_path = hd.join(HOLD_MANIFEST_FILENAME);
                let _ = fs::remove_file(&manifest_path);
                let _ = fs::remove_dir(hd);
            }
            self.restored = true;
        }

        if self.verbose && restored_count > 0 {
            eprintln!(
                "  [Verbose][Temp-Hold] Restored {} parked video(s) back to original paths.",
                restored_count
            );
        }

        if let Some(err) = first_err {
            return Err(err);
        }
        Ok(restored_count)
    }
}

impl Drop for TempHoldSession {
    fn drop(&mut self) {
        if !self.restored && !self.parked.is_empty() {
            let _ = self.restore_all();
        }
    }
}

fn hold_dir_by_parent_seed(p: &Path) -> String {
    let h = blake3::hash(p.to_string_lossy().as_bytes()).to_hex();
    h[..8].to_string()
}

/// Recovers any stranded `.rusty_scanner_hold_*/hold_manifest.json` left behind by an interrupted process.
pub fn recover_stranded_temp_holds(inputs: &[PathBuf], anchors: &[AnchorEntry], verbose: bool) {
    let my_pid = std::process::id();
    let mut search_parents: HashSet<PathBuf> = HashSet::new();

    for inp in inputs {
        let norm = normalize_path_lexical(inp);
        if let Some(p) = norm.parent() {
            search_parents.insert(p.to_path_buf());
        }
        if norm.is_dir() {
            search_parents.insert(norm);
        }
    }
    for anc in anchors {
        if let Some(p) = anc.anchor_path.parent() {
            search_parents.insert(p.to_path_buf());
        }
    }

    for parent in search_parents {
        let Ok(rd) = fs::read_dir(&parent) else {
            continue;
        };
        for entry in rd.filter_map(|e| e.ok()) {
            let fname = entry.file_name().to_string_lossy().to_string();
            if !fname.starts_with(TEMP_HOLD_PREFIX) || !entry.path().is_dir() {
                continue;
            }
            let manifest_path = entry.path().join(HOLD_MANIFEST_FILENAME);
            let Ok(raw) = fs::read_to_string(&manifest_path) else {
                continue;
            };
            let Ok(manifest) = serde_json::from_str::<HoldManifest>(&raw) else {
                continue;
            };
            if manifest.pid == my_pid {
                continue;
            }
            // Check if the owning PID is no longer alive
            if is_pid_alive_fast(manifest.pid) {
                continue;
            }
            eprintln!(
                "[Recovery] Found stranded temp hold {:?} from dead PID {}; restoring {} file(s)...",
                entry.path(),
                manifest.pid,
                manifest.entries.len()
            );
            let mut all_ok = true;
            for item in manifest.entries.iter().rev() {
                if item.parked_path.exists() {
                    if let Some(orig_p) = item.original_path.parent() {
                        let _ = fs::create_dir_all(orig_p);
                    }
                    if let Err(e) = fs::rename(&item.parked_path, &item.original_path) {
                        eprintln!(
                            "  [Recovery] Failed restoring {:?} -> {:?}: {}",
                            item.parked_path, item.original_path, e
                        );
                        all_ok = false;
                    } else if verbose {
                        eprintln!(
                            "  [Recovery] Restored {:?} -> {:?}",
                            item.parked_path, item.original_path
                        );
                    }
                }
            }
            if all_ok {
                let _ = fs::remove_file(&manifest_path);
                let _ = fs::remove_dir(entry.path());
            }
        }
    }
}

/// Statistics accumulated across all live convergence passes.
#[derive(Debug, Default, Clone)]
pub struct LiveExecutionStats {
    pub passes_executed: usize,
    pub anchors_invoked: usize,
    pub dedup_runs: usize,
    pub clean_runs: usize,
    pub files_moved_directly: usize,
    pub files_resolved_via_clean: usize,
    pub secondary_symlinks_created: usize,
    pub secondary_symlinks_skipped_existing: usize,
    pub unmatched_written: usize,
    pub quarantined_errors: usize,
    pub bytes_moved: u64,
    pub bytes_reclaimed: u64,
    pub anchor_upgrades: usize,
    pub report_run_dir: PathBuf,
    pub html_report_path: Option<PathBuf>,
}

/// Executes the full Live Run iterative loop:
/// 1. Discovers remaining misc videos across `<INPUTS>...`
/// 2. Matches & ranks against `anchor_dirs.yaml` + `scanner_config.yaml`
/// 3. Groups by Rank #1 anchor -> runs `rusty_dedup` -> parks unrelated non-dups & runs `rusty_clean`
///    (if duplicates found) OR moves directly (if no duplicates) -> creates Rank #2–#4 symlinks,
///    saving every `dedup/*.json` and `clean/*.json` report permanently and updating `execution_report.html`
/// 4. Repeats until no matched files remain in misc inputs
/// 5. Outputs remaining unmatched files to `misc_files.txt` (`unmatched_out`)
pub fn run_live_loop(
    inputs: &[PathBuf],
    anchor_cfg: &AnchorDirsConfig,
    scanner_cfg: &ScannerConfig,
    matcher: &AnchorMatcher,
    tools: &ResolvedTools,
    csv_out: Option<&Path>,
    unmatched_out: &Path,
    verbose: bool,
) -> Result<LiveExecutionStats> {
    let global_start = Instant::now();
    install_signal_handlers();
    recover_stranded_temp_holds(inputs, &anchor_cfg.anchors, verbose);

    let mut report_store = RunReportStore::init(
        &scanner_cfg.reports.output_dir,
        scanner_cfg.reports.enabled,
        scanner_cfg.reports.capture_pair_categories_log,
        scanner_cfg.report_server.live_refresh_secs,
        "LIVE_LOOP",
        inputs,
    )?;
    eprintln!(
        "[Reports] Saving all rusty_dedup & rusty_clean job reports to {:?}",
        report_store.run_dir
    );
    if scanner_cfg.reports.enabled {
        eprintln!(
            "[Reports] Live drill-down HTML report: {:?}",
            report_store.html_path
        );
    }
    let mut bg_server =
        BackgroundReportServer::try_start(&scanner_cfg.reports.output_dir, &scanner_cfg.report_server);

    let mut stats = LiveExecutionStats {
        report_run_dir: report_store.run_dir.clone(),
        html_report_path: scanner_cfg
            .reports
            .enabled
            .then(|| report_store.html_path.clone()),
        ..LiveExecutionStats::default()
    };
    let mut quarantined_paths: HashSet<PathBuf> = HashSet::new();
    let mut final_unmatched_paths: Vec<PathBuf> = Vec::new();
    let mut seq_counter: usize = 0;

    'pass_loop: loop {
        if is_shutdown_requested() {
            break 'pass_loop;
        }
        let pass_num = stats.passes_executed + 1;
        let is_first_pass = pass_num == 1;

        let candidates = discover_misc_videos(
            inputs,
            &anchor_cfg.anchors,
            &scanner_cfg.video_extensions,
            &quarantined_paths,
            is_first_pass,
            verbose,
        );

        let outcome = matcher.match_batch(&candidates);

        if is_first_pass {
            print_match_summary(&outcome, &anchor_cfg.anchors, "LIVE RUN - PASS 1");
            if let Some(csv_p) = csv_out {
                let rows = write_matches_csv(&outcome, csv_p)?;
                eprintln!(
                    "[Live Run] Wrote {} initial match row(s) to {:?}",
                    rows, csv_p
                );
            }
        } else if verbose {
            eprintln!(
                "[Verbose][Pass {}] Remaining misc candidates: {} matched, {} unmatched.",
                pass_num,
                outcome.matched_files.len(),
                outcome.unmatched_files.len()
            );
        }

        if outcome.matched_files.is_empty() {
            // Convergence reached: zero actionable matched files remain!
            final_unmatched_paths = outcome
                .unmatched_files
                .iter()
                .map(|c| c.path.clone())
                .collect();
            let unmatched_count = write_unmatched_files(&outcome.unmatched_files, unmatched_out)?;
            stats.unmatched_written = unmatched_count;
            break;
        }

        stats.passes_executed = pass_num;
        eprintln!(
            "\n>>> [Pass {}] Processing {} matched miscellaneous video file(s) across Rank #1 anchors...",
            pass_num,
            outcome.matched_files.len()
        );

        // Group matched files by their Rank #1 (`PRIMARY_MOVE`) anchor index
        let mut by_primary_anchor: BTreeMap<usize, Vec<FileMatchResult>> = BTreeMap::new();
        for file_res in outcome.matched_files {
            if let Some(primary) = file_res.matches.first() {
                by_primary_anchor
                    .entry(primary.anchor_idx)
                    .or_default()
                    .push(file_res);
            }
        }

        let mut progressed_in_pass = 0usize;

        for (anchor_idx, group_files) in by_primary_anchor {
            if is_shutdown_requested() {
                break 'pass_loop;
            }
            let anchor = &anchor_cfg.anchors[anchor_idx];

            // Filter to files that still exist right now (defensive check)
            let active_files: Vec<FileMatchResult> = group_files
                .into_iter()
                .filter(|f| f.candidate.path.is_file())
                .collect();
            if active_files.is_empty() {
                continue;
            }

            if !anchor.anchor_path.is_dir() {
                eprintln!(
                    "! ERROR: Anchor directory {:?} does not exist on disk; quarantining {} matched file(s) for this anchor.",
                    anchor.anchor_path,
                    active_files.len()
                );
                for f in &active_files {
                    quarantined_paths.insert(f.candidate.path.clone());
                    report_store.record_quarantined(
                        &f.candidate.path,
                        &anchor.anchor_path,
                        "Anchor directory does not exist on disk",
                    );
                    stats.quarantined_errors += 1;
                }
                continue;
            }

            seq_counter += 1;
            stats.anchors_invoked += 1;

            match process_single_anchor_group(
                anchor,
                &active_files,
                scanner_cfg,
                tools,
                &mut report_store,
                pass_num,
                seq_counter,
                &mut stats,
                verbose,
            ) {
                Ok(progressed) => {
                    progressed_in_pass += progressed;
                    // Any file still sitting at its original misc path failed to move/delete -> quarantine so loop terminates
                    for f in &active_files {
                        if f.candidate.path.exists() {
                            eprintln!(
                                "Warning: File {:?} remained in misc after processing anchor {:?}; quarantining from retry loop.",
                                f.candidate.path, anchor.anchor_path
                            );
                            quarantined_paths.insert(f.candidate.path.clone());
                            report_store.record_quarantined(
                                &f.candidate.path,
                                &anchor.anchor_path,
                                "Remained at source path after anchor processing",
                            );
                            stats.quarantined_errors += 1;
                        }
                    }
                }
                Err(e) => {
                    if is_shutdown_requested() {
                        eprintln!(
                            "\n[Interrupted] Signal received while processing anchor {:?}; stopping cleanly...",
                            anchor.anchor_path
                        );
                        break 'pass_loop;
                    }
                    let err_str = format!("{:#}", e);
                    eprintln!(
                        "! ERROR processing anchor {:?}: {}",
                        anchor.anchor_path, err_str
                    );
                    for f in &active_files {
                        if f.candidate.path.exists() {
                            quarantined_paths.insert(f.candidate.path.clone());
                            report_store.record_quarantined(
                                &f.candidate.path,
                                &anchor.anchor_path,
                                &err_str,
                            );
                            stats.quarantined_errors += 1;
                        } else {
                            progressed_in_pass += 1;
                        }
                    }
                }
            }
        }

        if progressed_in_pass == 0 {
            eprintln!(
                "Warning: Pass {} made zero file progress; breaking convergence loop safely.",
                pass_num
            );
            let remaining = discover_misc_videos(
                inputs,
                &anchor_cfg.anchors,
                &scanner_cfg.video_extensions,
                &HashSet::new(),
                false,
                verbose,
            );
            let final_outcome = matcher.match_batch(&remaining);
            final_unmatched_paths = final_outcome
                .unmatched_files
                .iter()
                .map(|c| c.path.clone())
                .collect();
            stats.unmatched_written =
                write_unmatched_files(&final_outcome.unmatched_files, unmatched_out)?;
            break;
        }
    }

    if is_shutdown_requested() {
        eprintln!(
            "\n[Interrupted] Marking run {} as INTERRUPTED (STOPPED) in report manifest...",
            report_store.manifest.run_id
        );
        report_store.mark_interrupted(stats.passes_executed, stats.quarantined_errors)?;
        stats.bytes_moved = report_store.manifest.totals.bytes_moved;
        stats.bytes_reclaimed = report_store.manifest.totals.bytes_reclaimed;
        stats.anchor_upgrades = report_store.manifest.totals.anchor_upgrades;
        if let Some(ref mut srv) = bg_server {
            srv.stop();
        }
        return Ok(stats);
    }

    report_store.finalize(
        stats.passes_executed,
        &final_unmatched_paths,
        stats.quarantined_errors,
    )?;
    stats.bytes_moved = report_store.manifest.totals.bytes_moved;
    stats.bytes_reclaimed = report_store.manifest.totals.bytes_reclaimed;
    stats.anchor_upgrades = report_store.manifest.totals.anchor_upgrades;

    println!("================================================================================");
    println!(
        "=== Rusty Intelli Scanner — Live Execution Complete (elapsed {:.2?}) ===",
        global_start.elapsed()
    );
    println!("  Convergence Passes Run        : {}", stats.passes_executed);
    println!("  Anchor Batches Processed      : {}", stats.anchors_invoked);
    println!("  rusty_dedup Invocations       : {}", stats.dedup_runs);
    println!("  rusty_clean Invocations       : {}", stats.clean_runs);
    println!("  Files Moved Directly (No Dup) : {}", stats.files_moved_directly);
    println!(
        "  Files Resolved via rusty_clean: {} ({} anchor upgrade(s))",
        stats.files_resolved_via_clean, stats.anchor_upgrades
    );
    println!(
        "  Disk Space Reclaimed          : {:.2} MB",
        stats.bytes_reclaimed as f64 / (1024.0 * 1024.0)
    );
    println!(
        "  Rank #2.. Symlinks Created    : {} (skipped {} already existing)",
        stats.secondary_symlinks_created, stats.secondary_symlinks_skipped_existing
    );
    println!(
        "  Unmatched Files -> {:?} : {}",
        unmatched_out, stats.unmatched_written
    );
    println!("  Saved Dedup/Clean Reports Dir : {:?}", report_store.run_dir);
    if let Some(ref html_p) = stats.html_report_path {
        println!("  Interactive HTML Audit Report : {:?}", html_p);
        println!(
            "  Latest HTML Shortcut          : {:?}",
            report_store.latest_html_path
        );
    }
    if stats.quarantined_errors > 0 {
        println!(
            "  Quarantined File Errors       : {}",
            stats.quarantined_errors
        );
    }
    println!("================================================================================");

    if let Some(ref mut srv) = bg_server {
        let linger = scanner_cfg.report_server.linger_after_run_secs;
        if linger > 0 {
            eprintln!(
                "[Report Server] Holding HTTP dashboard server open on {} for {}s (linger_after_run_secs)...",
                srv.local_addr, linger
            );
            std::thread::sleep(std::time::Duration::from_secs(linger));
        }
        srv.stop();
    }

    Ok(stats)
}

/// Executes a pre-curated list of `FileMatchResult` assignments (from `rusty_scanner --from-csv`
/// or `rusty_triage`'s `X` Execute action) through the exact `rusty_dedup` -> Temp-Hold ->
/// `rusty_clean` / direct-move -> symlink pipeline, archiving all JSON & HTML reports.
pub fn execute_matched_files(
    matched_files: &[FileMatchResult],
    anchor_cfg: &AnchorDirsConfig,
    scanner_cfg: &ScannerConfig,
    tools: &ResolvedTools,
    verbose: bool,
) -> Result<LiveExecutionStats> {
    let global_start = Instant::now();
    install_signal_handlers();
    let input_paths: Vec<PathBuf> = matched_files
        .iter()
        .map(|m| m.candidate.path.clone())
        .collect();
    recover_stranded_temp_holds(&input_paths, &anchor_cfg.anchors, verbose);

    let mut report_store = RunReportStore::init(
        &scanner_cfg.reports.output_dir,
        scanner_cfg.reports.enabled,
        scanner_cfg.reports.capture_pair_categories_log,
        scanner_cfg.report_server.live_refresh_secs,
        "CURATED_BATCH",
        &input_paths,
    )?;
    eprintln!(
        "[Reports] Saving all rusty_dedup & rusty_clean job reports to {:?}",
        report_store.run_dir
    );
    if scanner_cfg.reports.enabled {
        eprintln!(
            "[Reports] Live drill-down HTML report: {:?}",
            report_store.html_path
        );
    }
    let mut bg_server =
        BackgroundReportServer::try_start(&scanner_cfg.reports.output_dir, &scanner_cfg.report_server);

    let mut stats = LiveExecutionStats {
        passes_executed: 1,
        report_run_dir: report_store.run_dir.clone(),
        html_report_path: scanner_cfg
            .reports
            .enabled
            .then(|| report_store.html_path.clone()),
        ..LiveExecutionStats::default()
    };

    let mut by_primary_anchor: BTreeMap<usize, Vec<FileMatchResult>> = BTreeMap::new();
    for file_res in matched_files {
        if let Some(primary) = file_res.matches.first() {
            by_primary_anchor
                .entry(primary.anchor_idx)
                .or_default()
                .push(file_res.clone());
        }
    }

    let mut seq_counter = 0usize;
    for (anchor_idx, group_files) in by_primary_anchor {
        if is_shutdown_requested() {
            break;
        }
        let anchor = &anchor_cfg.anchors[anchor_idx];
        let active_files: Vec<FileMatchResult> = group_files
            .into_iter()
            .filter(|f| f.candidate.path.is_file())
            .collect();
        if active_files.is_empty() {
            continue;
        }
        if !anchor.anchor_path.is_dir() {
            eprintln!(
                "! ERROR: Anchor directory {:?} does not exist on disk; skipping {} file(s).",
                anchor.anchor_path,
                active_files.len()
            );
            for f in &active_files {
                report_store.record_quarantined(
                    &f.candidate.path,
                    &anchor.anchor_path,
                    "Anchor directory does not exist on disk",
                );
            }
            stats.quarantined_errors += active_files.len();
            continue;
        }

        seq_counter += 1;
        stats.anchors_invoked += 1;
        match process_single_anchor_group(
            anchor,
            &active_files,
            scanner_cfg,
            tools,
            &mut report_store,
            1,
            seq_counter,
            &mut stats,
            verbose,
        ) {
            Ok(_) => {
                for f in &active_files {
                    if f.candidate.path.exists() {
                        report_store.record_quarantined(
                            &f.candidate.path,
                            &anchor.anchor_path,
                            "Remained at source path after anchor processing",
                        );
                        stats.quarantined_errors += 1;
                    }
                }
            }
            Err(e) => {
                if is_shutdown_requested() {
                    eprintln!(
                        "\n[Interrupted] Signal received while processing anchor {:?}; stopping cleanly...",
                        anchor.anchor_path
                    );
                    break;
                }
                let err_str = format!("{:#}", e);
                eprintln!(
                    "! ERROR processing anchor {:?}: {}",
                    anchor.anchor_path, err_str
                );
                for f in &active_files {
                    if f.candidate.path.exists() {
                        report_store.record_quarantined(
                            &f.candidate.path,
                            &anchor.anchor_path,
                            &err_str,
                        );
                        stats.quarantined_errors += 1;
                    }
                }
            }
        }
    }

    if is_shutdown_requested() {
        eprintln!(
            "\n[Interrupted] Marking curated run {} as INTERRUPTED (STOPPED) in report manifest...",
            report_store.manifest.run_id
        );
        report_store.mark_interrupted(1, stats.quarantined_errors)?;
        stats.bytes_moved = report_store.manifest.totals.bytes_moved;
        stats.bytes_reclaimed = report_store.manifest.totals.bytes_reclaimed;
        stats.anchor_upgrades = report_store.manifest.totals.anchor_upgrades;
        if let Some(ref mut srv) = bg_server {
            srv.stop();
        }
        return Ok(stats);
    }

    report_store.finalize(1, &[], stats.quarantined_errors)?;
    stats.bytes_moved = report_store.manifest.totals.bytes_moved;
    stats.bytes_reclaimed = report_store.manifest.totals.bytes_reclaimed;
    stats.anchor_upgrades = report_store.manifest.totals.anchor_upgrades;

    println!("================================================================================");
    println!(
        "=== Rusty Intelli Scanner — Curated Batch Execution Complete (elapsed {:.2?}) ===",
        global_start.elapsed()
    );
    println!("  Anchor Batches Processed      : {}", stats.anchors_invoked);
    println!("  rusty_dedup Invocations       : {}", stats.dedup_runs);
    println!("  rusty_clean Invocations       : {}", stats.clean_runs);
    println!("  Files Moved Directly (No Dup) : {}", stats.files_moved_directly);
    println!(
        "  Files Resolved via rusty_clean: {} ({} anchor upgrade(s))",
        stats.files_resolved_via_clean, stats.anchor_upgrades
    );
    println!(
        "  Disk Space Reclaimed          : {:.2} MB",
        stats.bytes_reclaimed as f64 / (1024.0 * 1024.0)
    );
    println!(
        "  Rank #2.. Symlinks Created    : {} (skipped {} already existing)",
        stats.secondary_symlinks_created, stats.secondary_symlinks_skipped_existing
    );
    println!("  Saved Dedup/Clean Reports Dir : {:?}", report_store.run_dir);
    if let Some(ref html_p) = stats.html_report_path {
        println!("  Interactive HTML Audit Report : {:?}", html_p);
        println!(
            "  Latest HTML Shortcut          : {:?}",
            report_store.latest_html_path
        );
    }
    if stats.quarantined_errors > 0 {
        println!(
            "  Quarantined File Errors       : {}",
            stats.quarantined_errors
        );
    }
    println!("================================================================================");

    if let Some(ref mut srv) = bg_server {
        let linger = scanner_cfg.report_server.linger_after_run_secs;
        if linger > 0 {
            eprintln!(
                "[Report Server] Holding HTTP dashboard server open on {} for {}s (linger_after_run_secs)...",
                srv.local_addr, linger
            );
            std::thread::sleep(std::time::Duration::from_secs(linger));
        }
        srv.stop();
    }

    Ok(stats)
}

/// Processes a single Rank #1 anchor batch in one unified pass via `rusty_dedup --inputs-from`:
/// 1. Writes `<anchor_root>\n<file1>\n...` to a scratch list and runs `rusty_dedup --inputs-from`
///    writing directly to persistent `reports/run_.../dedup/<seq>_..._dups.json` (zero ARG_MAX splitting!)
/// 2. If `dups.json` has NO duplicates -> moves matched files directly to `<destination>` (preserving mtime)
///    and updates their paths inside `<states_dir>/rusty_video_cache.db` via `--record-moves`
/// 3. If `dups.json` HAS duplicates -> parks unrelated non-dups outside `misc_dir` (and `<anchor_root>`
///    if custom `destination`), runs `rusty_clean <scratch_dups.json> -t <destination> --states-dir <states_dir> -o reports/run_.../clean/<seq>_..._clean.json`,
///    and restores parked files (preserving both raw `dups.json` and `clean.json` permanently!)
/// 4. Creates softlinks in Rank #2, #3, #4 anchors' `<destination>` pointing to the surviving file
/// 5. Appends `AnchorBatchAudit` (`seq_id = 1..N`) and incrementally updates `run_manifest.json` + `execution_report.html`
#[allow(clippy::too_many_arguments)]
fn process_single_anchor_group(
    anchor: &AnchorEntry,
    batch: &[FileMatchResult],
    scanner_cfg: &ScannerConfig,
    tools: &ResolvedTools,
    report_store: &mut RunReportStore,
    pass_num: usize,
    seq_id: usize,
    stats: &mut LiveExecutionStats,
    verbose: bool,
) -> Result<usize> {
    fs::create_dir_all(&anchor.destination_path).with_context(|| {
        format!(
            "Failed to create anchor destination directory {:?}",
            anchor.destination_path
        )
    })?;

    let mut progressed = 0usize;
    let chunk_idx = 0usize;
    let chunk_start = Instant::now();
    eprintln!(
        "  [Anchor {:?} -> dest {:?}] Checking {} matched file(s) with rusty_dedup...",
        anchor.anchor_path,
        anchor.destination_path,
        batch.len()
    );

    let bpaths = report_store.prepare_batch_paths(pass_num, seq_id, chunk_idx, anchor);

    // ---------------------------------------------------------------------
    // Step 1: Write `<anchor_root>\n<matched_file_1>\n...` and invoke `rusty_dedup --inputs-from`
    //         writing directly to persistent `reports/run_<id>/dedup/..._dups.json`
    // ---------------------------------------------------------------------
    {
        let mut list_buf = String::with_capacity((batch.len() + 1) * 96);
        list_buf.push_str(&anchor.anchor_path.to_string_lossy());
        list_buf.push('\n');
        for item in batch {
            list_buf.push_str(&item.candidate.path.to_string_lossy());
            list_buf.push('\n');
        }
        fs::write(&bpaths.scratch_inputs_list_path, list_buf.as_bytes()).with_context(|| {
            format!(
                "Failed to write scratch inputs list {:?}",
                bpaths.scratch_inputs_list_path
            )
        })?;
    }

    let dedup_res = run_rusty_dedup_command(
        anchor,
        &bpaths.scratch_inputs_list_path,
        tools,
        &bpaths.dedup_json_path,
        bpaths.pairs_json_path.as_deref(),
        verbose,
    );
    let _ = fs::remove_file(&bpaths.scratch_inputs_list_path);
    dedup_res?;
    stats.dedup_runs += 1;

    let dups_raw = fs::read_to_string(&bpaths.dedup_json_path).with_context(|| {
        format!(
            "Failed to read rusty_dedup output JSON at {:?}",
            bpaths.dedup_json_path
        )
    })?;
    let dedup_report: DedupReportFull = serde_json::from_str(&dups_raw).with_context(|| {
        format!(
            "Failed to parse rusty_dedup output JSON at {:?}",
            bpaths.dedup_json_path
        )
    })?;

    let pairs_rel_used = bpaths
        .pairs_json_path
        .as_ref()
        .filter(|p| p.is_file())
        .and_then(|_| bpaths.pairs_rel_path.clone());

    // Map each original misc file index -> surviving final winner path in Rank #1
    let mut surviving_winner_by_idx: HashMap<usize, PathBuf> =
        HashMap::with_capacity(batch.len());
    let mut direct_moves_audit: Vec<MovedVideoRecord> = Vec::new();
    let mut dup_groups_audit = Vec::new();
    let mut clean_rel_used: Option<String> = None;
    let mut batch_bytes_moved = 0u64;
    let mut batch_bytes_reclaimed = 0u64;
    let mut batch_dups_deleted = 0usize;
    let mut batch_upgrades = 0usize;

    if dedup_report.duplicates.is_empty() {
        // -----------------------------------------------------------------
        // Step 2A: Zero duplicates -> Move (NEVER copy) directly to `<destination>`
        // -----------------------------------------------------------------
        eprintln!(
            "  [Anchor {:?}] rusty_dedup found 0 duplicates; moving {} file(s) directly to {:?}...",
            anchor.anchor_path,
            batch.len(),
            anchor.destination_path
        );

        let mut reserved_dest_paths: HashSet<PathBuf> = HashSet::new();
        for (i, item) in batch.iter().enumerate() {
            let src = &item.candidate.path;
            if !src.exists() {
                continue;
            }
            let dest = compute_unique_destination(
                src,
                &anchor.destination_path,
                &mut reserved_dest_paths,
            );
            let moved_sz = move_file_never_copy(
                src,
                &dest,
                item.candidate.size_bytes,
                i + 1,
                batch.len(),
                verbose,
            )?;
            let pm = item.matches.first();
            direct_moves_audit.push(MovedVideoRecord {
                source_path: src.to_string_lossy().to_string(),
                dest_path: dest.to_string_lossy().to_string(),
                size_bytes: moved_sz,
                matched_alias: pm.map(|m| m.matched_alias.clone()).unwrap_or_default(),
                match_tier: pm
                    .map(|m| m.match_tier.as_str().to_string())
                    .unwrap_or_default(),
                confidence_score: pm.map(|m| m.confidence_score).unwrap_or(0.0),
                move_kind: "DIRECT_NO_DUP".to_string(),
            });
            batch_bytes_moved += moved_sz;
            surviving_winner_by_idx.insert(i, dest);
            stats.files_moved_directly += 1;
            progressed += 1;
        }

        // Sync direct moves into `<states_dir>/rusty_video_cache.db` so future passes hit SQLite in 0ms!
        if !direct_moves_audit.is_empty() {
            let move_specs: Vec<serde_json::Value> = direct_moves_audit
                .iter()
                .map(|mv| serde_json::json!({ "from": mv.source_path, "to": mv.dest_path }))
                .collect();
            if let Ok(json_bytes) = serde_json::to_vec(&move_specs) {
                if fs::write(&bpaths.scratch_moves_json_path, &json_bytes).is_ok() {
                    if let Err(e) = run_rusty_dedup_record_moves(
                        tools,
                        &bpaths.scratch_moves_json_path,
                        verbose,
                    ) {
                        eprintln!(
                            "  Warning: Failed to record moved paths in SQLite cache ({})",
                            e
                        );
                    }
                    let _ = fs::remove_file(&bpaths.scratch_moves_json_path);
                }
            }
        }
    } else {
        // -----------------------------------------------------------------
        // Step 2B: Duplicates found ->
        //   1. Park unrelated non-duplicates outside `misc_dir` (and `<anchor_root>` if custom dest)
        //   2. Run `rusty_clean` (passing a scratch copy of `dups.json` so the saved
        //      `dedup/<seq>_..._dups.json` remains 100% pristine!) and write `-o` directly
        //      into persistent `clean/<seq>_..._clean.json`
        //   3. Restore parked files immediately
        // -----------------------------------------------------------------
        eprintln!(
            "  [Anchor {:?}] rusty_dedup found {} duplicate group(s); parking unrelated misc files and invoking rusty_clean -> {:?}...",
            anchor.anchor_path,
            dedup_report.duplicates.len(),
            anchor.destination_path
        );

        let pre_dest_entries = snapshot_dir_files(&anchor.destination_path);

        let mut allowed_inodes: HashSet<(u64, u64)> = HashSet::new();
        let mut allowed_paths: HashSet<PathBuf> = HashSet::new();
        let mut batch_lookup_keys: Vec<Vec<PathBuf>> = Vec::with_capacity(batch.len());
        let mut scan_roots_to_protect: Vec<PathBuf> = Vec::new();
        let mut seen_scan_roots: HashSet<PathBuf> = HashSet::new();

        for item in batch {
            allowed_inodes.insert(item.candidate.dev_inode);
            let norm_p = normalize_path_lexical(&item.candidate.path);
            let mut keys_for_item = vec![norm_p.clone()];
            allowed_paths.insert(norm_p.clone());
            if let Ok(can_p) = fs::canonicalize(&item.candidate.path) {
                allowed_paths.insert(can_p.clone());
                if !keys_for_item.contains(&can_p) {
                    keys_for_item.push(can_p);
                }
            }
            batch_lookup_keys.push(keys_for_item);
            if let Some(parent) = norm_p.parent() {
                if seen_scan_roots.insert(parent.to_path_buf()) {
                    scan_roots_to_protect.push(parent.to_path_buf());
                }
            }
            if let Some(ref mr) = item.candidate.misc_root {
                if seen_scan_roots.insert(mr.clone()) {
                    scan_roots_to_protect.push(mr.clone());
                }
            }
        }

        for grp in &dedup_report.duplicates {
            for f_str in &grp.files {
                let fp = PathBuf::from(f_str);
                let norm_fp = normalize_path_lexical(&fp);
                allowed_paths.insert(norm_fp.clone());
                if let Ok(meta) = fs::metadata(&fp) {
                    allowed_inodes.insert(path_dev_inode(&fp, &meta));
                }
                if let Some(parent) = norm_fp.parent() {
                    let under_anchor = norm_fp.starts_with(&anchor.anchor_path)
                        || norm_fp.starts_with(&anchor.canonical_anchor_path);
                    if !under_anchor || anchor.has_custom_destination() {
                        if seen_scan_roots.insert(parent.to_path_buf()) {
                            scan_roots_to_protect.push(parent.to_path_buf());
                        }
                    }
                }
            }
        }

        if anchor.has_custom_destination() && seen_scan_roots.insert(anchor.anchor_path.clone()) {
            scan_roots_to_protect.push(anchor.anchor_path.clone());
        }

        fs::copy(&bpaths.dedup_json_path, &bpaths.scratch_clean_input_path).with_context(|| {
            format!(
                "Failed to stage working copy of {:?} -> {:?}",
                bpaths.dedup_json_path, bpaths.scratch_clean_input_path
            )
        })?;

        let mut hold_session = TempHoldSession::new(verbose);
        hold_session.park_unrelated_videos(
            &scan_roots_to_protect,
            &allowed_inodes,
            &allowed_paths,
            &anchor.destination_path,
            &scanner_cfg.video_extensions,
            seq_id,
        )?;

        let clean_res = run_rusty_clean_command(
            &bpaths.scratch_clean_input_path,
            &anchor.destination_path,
            &bpaths.clean_json_path,
            tools,
            verbose,
        );

        // ALWAYS restore parked files and remove only the ephemeral scratch input copy
        let restore_res = hold_session.restore_all();
        let _ = fs::remove_file(&bpaths.scratch_clean_input_path);
        clean_res?;
        restore_res?;
        stats.clean_runs += 1;

        let clean_reports: Vec<CleanGroupDecision> = if bpaths.clean_json_path.is_file() {
            clean_rel_used = Some(bpaths.clean_rel_path.clone());
            let raw = fs::read_to_string(&bpaths.clean_json_path)?;
            serde_json::from_str(&raw).unwrap_or_default()
        } else {
            Vec::new()
        };

        let pair_logs: Vec<PairEvaluationEntry> = if let Some(ref pp) = bpaths.pairs_json_path {
            if pp.is_file() {
                let praw = fs::read_to_string(pp)?;
                serde_json::from_str(&praw).unwrap_or_default()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        let (corr_groups, reclaimed_b, moved_winners_b, deleted_cnt, upgrades_cnt) =
            correlate_duplicate_groups(
                &anchor.anchor_path,
                &anchor.canonical_anchor_path,
                &anchor.destination_path,
                batch,
                &dedup_report,
                &clean_reports,
                &pair_logs,
            );
        dup_groups_audit = corr_groups;
        batch_bytes_reclaimed += reclaimed_b;
        batch_bytes_moved += moved_winners_b;
        batch_dups_deleted += deleted_cnt;
        batch_upgrades += upgrades_cnt;

        let mut winner_by_member_path: HashMap<PathBuf, PathBuf> = HashMap::new();
        for grp in &clean_reports {
            let winner_p = PathBuf::from(&grp.retained_winner);
            for sc in &grp.scores {
                let mp = PathBuf::from(&sc.file_path);
                winner_by_member_path.insert(normalize_path_lexical(&mp), winner_p.clone());
                if let Ok(can_m) = fs::canonicalize(&mp) {
                    winner_by_member_path.insert(can_m, winner_p.clone());
                } else if let (Some(parent), Some(fname)) = (mp.parent(), mp.file_name()) {
                    if let Ok(can_parent) = fs::canonicalize(parent) {
                        winner_by_member_path.insert(can_parent.join(fname), winner_p.clone());
                    }
                }
            }
        }

        let post_dest_entries = snapshot_dir_files(&anchor.destination_path);
        let new_dest_files: Vec<(PathBuf, (u64, u64), u64)> = post_dest_entries
            .into_iter()
            .filter(|(p, _, _)| !pre_dest_entries.iter().any(|(old_p, _, _)| old_p == p))
            .collect();

        for (i, item) in batch.iter().enumerate() {
            let orig_norm = normalize_path_lexical(&item.candidate.path);
            if !orig_norm.exists() {
                progressed += 1;
                stats.files_resolved_via_clean += 1;
            }

            if let Some(w) = batch_lookup_keys[i]
                .iter()
                .find_map(|k| winner_by_member_path.get(k))
            {
                if w.exists() {
                    surviving_winner_by_idx.insert(i, w.clone());
                    continue;
                }
            }

            if let Some(found) = resolve_moved_nondup_in_dest(
                &item.candidate.path,
                item.candidate.dev_inode,
                item.candidate.size_bytes,
                &anchor.destination_path,
                &new_dest_files,
            ) {
                let pm = item.matches.first();
                direct_moves_audit.push(MovedVideoRecord {
                    source_path: item.candidate.path.to_string_lossy().to_string(),
                    dest_path: found.to_string_lossy().to_string(),
                    size_bytes: item.candidate.size_bytes,
                    matched_alias: pm.map(|m| m.matched_alias.clone()).unwrap_or_default(),
                    match_tier: pm
                        .map(|m| m.match_tier.as_str().to_string())
                        .unwrap_or_default(),
                    confidence_score: pm.map(|m| m.confidence_score).unwrap_or(0.0),
                    move_kind: "CLEAN_PHASE2_NON_DUP".to_string(),
                });
                batch_bytes_moved += item.candidate.size_bytes;
                surviving_winner_by_idx.insert(i, found);
            }
        }
    }

    // ---------------------------------------------------------------------
    // Step 3: Create softlinks in Rank #2, #3, #4 anchors (`PlannedAction::Symlink`)
    // ---------------------------------------------------------------------
    let mut batch_symlinks: Vec<SymlinkRecord> = Vec::new();
    let mut batch_symlinks_skipped = 0usize;

    for (i, item) in batch.iter().enumerate() {
        let Some(winner_path) = surviving_winner_by_idx.get(&i) else {
            continue;
        };
        if !winner_path.exists() {
            continue;
        }
        let winner_inode = fs::metadata(winner_path)
            .ok()
            .map(|m| path_dev_inode(winner_path, &m));

        let orig_stem = item
            .candidate
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("video");
        let winner_ext = winner_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        for sec_match in &item.matches {
            if sec_match.planned_action != PlannedAction::Symlink {
                continue;
            }
            let sec_dest = &sec_match.destination_path;
            fs::create_dir_all(sec_dest).with_context(|| {
                format!(
                    "Failed to create secondary symlink destination {:?}",
                    sec_dest
                )
            })?;

            let sec_dest_inode = fs::metadata(sec_dest)
                .ok()
                .map(|m| path_dev_inode(sec_dest, &m));
            if let Some(wparent) = winner_path.parent() {
                if are_same_dir(wparent, sec_dest, sec_dest_inode) {
                    continue;
                }
            }

            if let Some(w_ino) = winner_inode {
                if dir_already_references_inode(sec_dest, w_ino) {
                    stats.secondary_symlinks_skipped_existing += 1;
                    batch_symlinks_skipped += 1;
                    if verbose {
                        eprintln!(
                            "    [Verbose][Symlink Rank #{}] {:?} already references winner inode; skipping.",
                            sec_match.rank, sec_dest
                        );
                    }
                    continue;
                }
            }

            let link_path = compute_unique_symlink_path(sec_dest, orig_stem, winner_ext);
            create_softlink(winner_path, &link_path).with_context(|| {
                format!(
                    "Failed to create Rank #{} symlink {:?} -> {:?}",
                    sec_match.rank, link_path, winner_path
                )
            })?;
            stats.secondary_symlinks_created += 1;
            batch_symlinks.push(SymlinkRecord {
                rank: sec_match.rank,
                source_misc_path: item.candidate.path.to_string_lossy().to_string(),
                secondary_anchor_path: sec_match.anchor_path.to_string_lossy().to_string(),
                link_path: link_path.to_string_lossy().to_string(),
                target_winner_path: winner_path.to_string_lossy().to_string(),
                matched_alias: sec_match.matched_alias.clone(),
                confidence_score: sec_match.confidence_score,
            });
            println!(
                "    ⇢ [Rank #{} Symlink] {:?} -> {:?}",
                sec_match.rank, link_path, winner_path
            );
        }
    }

    // ---------------------------------------------------------------------
    // Step 4: Persist incremental `run_manifest.json` + `execution_report.html`
    //         (Both `bpaths.dedup_json_path` and `bpaths.clean_json_path` stay permanently!)
    // ---------------------------------------------------------------------
    let anchor_name = anchor
        .anchor_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&anchor.raw_key)
        .to_string();
    report_store.record_anchor_batch(AnchorBatchAudit {
        seq_id,
        pass_num,
        chunk_idx: 0,
        anchor_name,
        anchor_path: anchor.anchor_path.to_string_lossy().to_string(),
        destination_path: anchor.destination_path.to_string_lossy().to_string(),
        matched_input_count: batch.len(),
        elapsed_ms: chunk_start.elapsed().as_millis() as u64,
        dedup_report_rel_path: bpaths.dedup_rel_path,
        clean_report_rel_path: clean_rel_used,
        pairs_log_rel_path: pairs_rel_used,
        direct_moves: direct_moves_audit,
        duplicate_groups: dup_groups_audit,
        symlinks_created: batch_symlinks,
        symlinks_skipped_existing: batch_symlinks_skipped,
        bytes_moved: batch_bytes_moved,
        bytes_reclaimed: batch_bytes_reclaimed,
        duplicates_deleted: batch_dups_deleted,
        anchor_upgrades_count: batch_upgrades,
        errors: Vec::new(),
    })?;

    Ok(progressed)
}

fn run_rusty_dedup_record_moves(
    tools: &ResolvedTools,
    moves_json: &Path,
    verbose: bool,
) -> Result<()> {
    let mut cmd = Command::new(&tools.rusty_dedup_bin);
    cmd.arg("--states-dir").arg(&tools.states_dir);
    cmd.arg("--record-moves").arg(moves_json);
    if verbose {
        cmd.arg("-v");
    }
    let status = cmd
        .status()
        .with_context(|| format!("Failed to launch {:?} --record-moves", tools.rusty_dedup_bin))?;
    if !status.success() {
        bail!("rusty_dedup --record-moves exited with status {}", status);
    }
    Ok(())
}

fn run_rusty_dedup_command(
    anchor: &AnchorEntry,
    inputs_list_file: &Path,
    tools: &ResolvedTools,
    output_json: &Path,
    categories_log_json: Option<&Path>,
    verbose: bool,
) -> Result<()> {
    let mut cmd = if let Some(ref harness_script) = tools.dedup_metrics_harness {
        // Always launch `rusty_dedup` under `rusty_dedup_metrics_harness.py` so all 3 layers of
        // Prometheus/Grafana metrics (Rust snapshot + OS kernel threads/children + Host CPU/RAM/GPU)
        // are captured accurately!
        let mut c = Command::new(&tools.python_bin);
        c.arg(harness_script);
        if let Some(ref cfg) = tools.dedup_config {
            c.arg("-c").arg(cfg);
        }
        if let Some(linger) = tools.harness_linger_secs {
            c.arg("--linger-secs").arg(format!("{}", linger));
        }
        if verbose {
            c.arg("-v");
        }
        c.arg("--");
        c.arg(&tools.rusty_dedup_bin);
        c
    } else {
        Command::new(&tools.rusty_dedup_bin)
    };

    if let Some(ref cfg) = tools.dedup_config {
        cmd.arg("-c").arg(cfg);
    }
    cmd.arg("--states-dir").arg(&tools.states_dir);
    cmd.arg("-o").arg(output_json);
    if let Some(cat_log) = categories_log_json {
        cmd.arg("--categories-log").arg(cat_log);
    }
    if verbose {
        cmd.arg("-v");
    }
    cmd.arg("--inputs-from").arg(inputs_list_file);

    if verbose {
        eprintln!("  [Verbose][Exec] {:?}", cmd);
    }

    let status = cmd.status().with_context(|| {
        if let Some(ref h) = tools.dedup_metrics_harness {
            format!(
                "Failed to launch rusty_dedup via Python metrics harness {:?} ({:?})",
                h, tools.python_bin
            )
        } else {
            format!("Failed to launch {:?}", tools.rusty_dedup_bin)
        }
    })?;
    if !status.success() {
        bail!(
            "rusty_dedup exited with non-zero status {} for anchor {:?}",
            status,
            anchor.anchor_path
        );
    }
    Ok(())
}

fn run_rusty_clean_command(
    dups_json: &Path,
    target_dest: &Path,
    clean_report_out: &Path,
    tools: &ResolvedTools,
    verbose: bool,
) -> Result<()> {
    let mut cmd = Command::new(&tools.rusty_clean_bin);
    cmd.arg(dups_json);
    if let Some(ref dec_cfg) = tools.decision_config {
        cmd.arg("-c").arg(dec_cfg);
    }
    cmd.arg("-t").arg(target_dest);
    cmd.arg("--states-dir").arg(&tools.states_dir);
    cmd.arg("-o").arg(clean_report_out);
    if verbose {
        cmd.arg("-v");
    }

    if verbose {
        eprintln!("  [Verbose][Exec] {:?}", cmd);
    }

    let status = cmd
        .status()
        .with_context(|| format!("Failed to launch {:?}", tools.rusty_clean_bin))?;
    if !status.success() {
        bail!(
            "rusty_clean exited with non-zero status {} for target {:?}",
            status,
            target_dest
        );
    }
    Ok(())
}

fn are_same_dir(dir_a: &Path, norm_b: &Path, id_b: Option<(u64, u64)>) -> bool {
    if dir_a == norm_b || normalize_path_lexical(dir_a) == norm_b {
        return true;
    }
    if let Some(target_id) = id_b {
        if let Ok(meta_a) = fs::metadata(dir_a) {
            if path_dev_inode(dir_a, &meta_a) == target_id {
                return true;
            }
        }
    }
    false
}

/// Computes a collision-free destination file path inside `target_dir` (`name.ext`, `name_1.ext`, ...).
pub fn compute_unique_destination(
    src: &Path,
    target_dir: &Path,
    reserved: &mut HashSet<PathBuf>,
) -> PathBuf {
    let file_name = src
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("video");
    let candidate = target_dir.join(file_name);

    let same_file = match (fs::metadata(src), fs::metadata(&candidate)) {
        (Ok(m1), Ok(m2)) => path_dev_inode(src, &m1) == path_dev_inode(&candidate, &m2),
        _ => false,
    };

    if (!candidate.exists() && candidate.symlink_metadata().is_err() || same_file)
        && !reserved.contains(&candidate)
    {
        reserved.insert(candidate.clone());
        return candidate;
    }

    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("video");
    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("");

    let mut counter = 1usize;
    loop {
        let numbered = if ext.is_empty() {
            format!("{}_{}", stem, counter)
        } else {
            format!("{}_{}.{}", stem, counter, ext)
        };
        let cand = target_dir.join(numbered);
        if !cand.exists() && cand.symlink_metadata().is_err() && !reserved.contains(&cand) {
            reserved.insert(cand.clone());
            return cand;
        }
        counter += 1;
    }
}

fn compute_unique_symlink_path(target_dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let base_name = if ext.is_empty() {
        stem.to_string()
    } else {
        format!("{}.{}", stem, ext)
    };
    let first = target_dir.join(base_name);
    if first.symlink_metadata().is_err() {
        return first;
    }

    let mut counter = 1usize;
    loop {
        let numbered = if ext.is_empty() {
            format!("{}_{}", stem, counter)
        } else {
            format!("{}_{}.{}", stem, counter, ext)
        };
        let cand = target_dir.join(numbered);
        if cand.symlink_metadata().is_err() {
            return cand;
        }
        counter += 1;
    }
}

/// Moves `src` -> `dest` (NEVER leaving a duplicate copy behind):
/// 1. Attempts instant atomic `fs::rename` (same filesystem).
/// 2. Falls back to 4 MiB chunked copy + `sync_all()` + `fs::remove_file(src)` across volumes.
pub fn move_file_never_copy(
    src: &Path,
    dest: &Path,
    expected_size: u64,
    move_idx: usize,
    total_moves: usize,
    verbose: bool,
) -> Result<u64> {
    let start = Instant::now();
    let size = if expected_size > 0 {
        expected_size
    } else {
        fs::metadata(src).map(|m| m.len()).unwrap_or(0)
    };

    if src == dest {
        return Ok(size);
    }

    match fs::rename(src, dest) {
        Ok(_) => {
            println!(
                "    ✓ [Move {}/{}] ({:.2} MB, atomic rename in {:.2?}): {:?} -> {:?}",
                move_idx,
                total_moves,
                size as f64 / (1024.0 * 1024.0),
                start.elapsed(),
                src,
                dest
            );
            return Ok(size);
        }
        Err(rename_err) => {
            if verbose {
                eprintln!(
                    "    [Verbose] Atomic rename {:?} -> {:?} returned {:?}; streaming across volumes...",
                    src,
                    dest,
                    rename_err.kind()
                );
            }
        }
    }

    let src_modified = fs::metadata(src).ok().and_then(|m| m.modified().ok());
    let mut reader = File::open(src)
        .with_context(|| format!("Failed to open {:?} for cross-volume move", src))?;
    let mut writer = File::create(dest)
        .with_context(|| format!("Failed to create {:?} for cross-volume move", dest))?;

    let mut buf = vec![0u8; MOVE_COPY_CHUNK_SIZE];
    let mut copied = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
        copied += n as u64;
    }
    if let Some(mtime) = src_modified {
        let _ = writer.set_modified(mtime);
    }
    writer.sync_all()?;
    drop(writer);
    drop(reader);

    if let Err(del_err) = fs::remove_file(src) {
        // Roll back partial destination copy so we never violate the move-not-copy invariant!
        let _ = fs::remove_file(dest);
        bail!(
            "Failed to remove source file {:?} after cross-volume transfer (rolled back {:?}): {}",
            src,
            dest,
            del_err
        );
    }

    println!(
        "    ✓ [Move {}/{}] ({:.2} MB, cross-volume move in {:.2?}): {:?} -> {:?}",
        move_idx,
        total_moves,
        copied as f64 / (1024.0 * 1024.0),
        start.elapsed(),
        src,
        dest
    );

    Ok(copied)
}

fn snapshot_dir_files(dir: &Path) -> Vec<(PathBuf, (u64, u64), u64)> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.filter_map(|e| e.ok()) {
        let p = normalize_path_lexical(&entry.path());
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                out.push((p.clone(), path_dev_inode(&p, &meta), meta.len()));
            }
        }
    }
    out
}

fn resolve_moved_nondup_in_dest(
    orig_src: &Path,
    orig_inode: (u64, u64),
    orig_size: u64,
    dest_dir: &Path,
    new_dest_files: &[(PathBuf, (u64, u64), u64)],
) -> Option<PathBuf> {
    // 1. Exact `(dev, inode)` match (always hits for same-volume moves)
    for (p, ino, _) in new_dest_files {
        if *ino == orig_inode {
            return Some(p.clone());
        }
    }

    // 2. Direct `<dest_dir>/<orig_file_name>` check
    if let Some(fname) = orig_src.file_name() {
        let direct = dest_dir.join(fname);
        if direct.is_file() {
            return Some(direct);
        }
    }

    // 3. Collision-suffixed `<stem>_<N>.<ext>` matching `orig_size` among newly added files
    let orig_stem = orig_src.file_stem()?.to_str()?;
    let orig_ext = orig_src
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let prefix = format!("{}_", orig_stem);

    for (p, _, sz) in new_dest_files {
        if *sz != orig_size {
            continue;
        }
        let p_stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let p_ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        if p_ext.eq_ignore_ascii_case(orig_ext) && p_stem.starts_with(&prefix) {
            let suffix = &p_stem[prefix.len()..];
            if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
                return Some(p.clone());
            }
        }
    }

    None
}

fn dir_already_references_inode(dir: &Path, target_inode: (u64, u64)) -> bool {
    let Ok(rd) = fs::read_dir(dir) else {
        return false;
    };
    for entry in rd.filter_map(|e| e.ok()) {
        let p = entry.path();
        // `fs::metadata` follows symlinks — so if a symlink in `dir` already points to `winner_path`,
        // `path_dev_inode` matches `target_inode`!
        if let Ok(meta) = fs::metadata(&p) {
            if path_dev_inode(&p, &meta) == target_inode {
                return true;
            }
        }
    }
    false
}

#[cfg(unix)]
fn create_softlink(original: &Path, link: &Path) -> std::io::Result<()> {
    symlink(original, link)
}

#[cfg(windows)]
fn create_softlink(original: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(original, link)
}
