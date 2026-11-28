use crate::config::{AnchorEntry, normalize_path_lexical};
use crate::matcher::FileMatchResult;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

// ============================================================================
// 1. Raw `rusty_dedup` & `rusty_clean` JSON Schemas (Tolerant Deserialization)
// ============================================================================

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DedupVideoAttributes {
    #[serde(default)]
    pub size_in_bytes: u64,
    #[serde(default)]
    pub runtime_in_seconds: u64,
    #[serde(default)]
    pub video_height: u32,
    #[serde(default)]
    pub video_width: u32,
    #[serde(default)]
    pub video_codec: String,
    #[serde(default)]
    pub audio_codec: String,
    #[serde(default)]
    pub video_bitrate_bps: u64,
    #[serde(default)]
    pub audio_bitrate_bps: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DedupGroupFull {
    #[serde(default)]
    pub dup_group_name: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub attributes: BTreeMap<String, DedupVideoAttributes>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DedupReportFull {
    #[serde(default)]
    pub input_directories: Vec<String>,
    #[serde(default)]
    pub duplicates: Vec<DedupGroupFull>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PairEvaluationEntry {
    #[serde(default)]
    pub file1: String,
    #[serde(default)]
    pub file2: String,
    #[serde(default)]
    pub initial_category: String,
    #[serde(default)]
    pub final_outcome: String,
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    pub matched_frames: usize,
    #[serde(default)]
    pub compared_runtime_secs: u64,
    #[serde(default)]
    pub best_offset_secs: i64,
    #[serde(default)]
    pub detection_method: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CleanScoreBreakdown {
    #[serde(default)]
    pub file_path: String,
    #[serde(default)]
    pub size_raw: u64,
    #[serde(default)]
    pub size_points: usize,
    #[serde(default)]
    pub size_normalized: f64,
    #[serde(default)]
    pub runtime_raw: u64,
    #[serde(default)]
    pub runtime_points: usize,
    #[serde(default)]
    pub runtime_normalized: f64,
    #[serde(default)]
    pub height_raw: u32,
    #[serde(default)]
    pub height_points: usize,
    #[serde(default)]
    pub height_normalized: f64,
    #[serde(default)]
    pub width_raw: u32,
    #[serde(default)]
    pub width_points: usize,
    #[serde(default)]
    pub width_normalized: f64,
    #[serde(default)]
    pub video_bitrate_raw: u64,
    #[serde(default)]
    pub video_bitrate_points: usize,
    #[serde(default)]
    pub video_bitrate_normalized: f64,
    #[serde(default)]
    pub audio_bitrate_raw: u64,
    #[serde(default)]
    pub audio_bitrate_points: usize,
    #[serde(default)]
    pub audio_bitrate_normalized: f64,
    #[serde(default)]
    pub video_codec_raw: String,
    #[serde(default)]
    pub video_codec_points: f64,
    #[serde(default)]
    pub video_codec_normalized: f64,
    #[serde(default)]
    pub audio_codec_raw: String,
    #[serde(default)]
    pub audio_codec_points: f64,
    #[serde(default)]
    pub audio_codec_normalized: f64,
    #[serde(default)]
    pub total_normalized_score: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CleanFileAction {
    #[serde(default)]
    pub file_path: String,
    #[serde(default)]
    pub decision: String,
    #[serde(default)]
    pub action_taken: String,
    #[serde(default)]
    pub moved_to: Option<String>,
    #[serde(default)]
    pub symlink_created: Option<String>,
    #[serde(default)]
    pub symlink_target: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CleanGroupDecision {
    #[serde(default)]
    pub dup_group_name: String,
    #[serde(default)]
    pub group_size: usize,
    #[serde(default)]
    pub retained_winner: String,
    #[serde(default)]
    pub winner_score: f64,
    #[serde(default)]
    pub tie_broken_by_shortest_path: bool,
    #[serde(default)]
    pub scores: Vec<CleanScoreBreakdown>,
    #[serde(default)]
    pub actions: Vec<CleanFileAction>,
}

// ============================================================================
// 2. Unified Correlated Run Manifest Data Model (`run_manifest.json` + HTML)
// ============================================================================

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunTotals {
    pub passes_executed: usize,
    pub anchor_batches: usize,
    pub dedup_runs: usize,
    pub clean_runs: usize,
    pub misc_files_evaluated: usize,
    pub direct_moves: usize,
    pub phase2_nondup_moves: usize,
    pub duplicate_groups: usize,
    pub duplicates_deleted: usize,
    pub anchor_upgrades: usize,
    pub existing_anchor_kept: usize,
    pub misc_batch_deduped: usize,
    pub intra_anchor_cleanup: usize,
    pub symlinks_created: usize,
    pub symlinks_skipped_existing: usize,
    pub bytes_moved: u64,
    pub bytes_reclaimed: u64,
    pub unmatched_count: usize,
    pub quarantined_error_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MovedVideoRecord {
    pub source_path: String,
    pub dest_path: String,
    pub size_bytes: u64,
    pub matched_alias: String,
    pub match_tier: String,
    pub confidence_score: f64,
    /// `"DIRECT_NO_DUP"` or `"CLEAN_PHASE2_NON_DUP"`
    pub move_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymlinkRecord {
    pub rank: usize,
    pub source_misc_path: String,
    pub secondary_anchor_path: String,
    pub link_path: String,
    pub target_winner_path: String,
    pub matched_alias: String,
    pub confidence_score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrelatedGroupMember {
    pub is_winner: bool,
    /// `"ANCHOR"` or `"MISC"`
    pub origin: String,
    pub file_path: String,
    pub decision: String,
    pub action_taken: String,
    pub moved_to: Option<String>,
    pub matched_alias: Option<String>,
    pub match_tier: Option<String>,
    pub scanner_confidence: Option<f64>,
    pub score: CleanScoreBreakdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrelatedDupGroup {
    pub dup_group_name: String,
    /// `"ANCHOR_UPGRADED"` | `"EXISTING_ANCHOR_KEPT"` | `"MISC_BATCH_DEDUPED"` | `"INTRA_ANCHOR_CLEANUP"`
    pub outcome: String,
    pub group_size: usize,
    pub retained_winner: String,
    pub winner_orig_path: String,
    pub winner_score: f64,
    pub max_possible_score: f64,
    pub tie_broken_by_shortest_path: bool,
    pub bytes_reclaimed: u64,
    pub bytes_moved: u64,
    pub members: Vec<CorrelatedGroupMember>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pair_evaluations: Vec<PairEvaluationEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorBatchAudit {
    pub seq_id: usize,
    pub pass_num: usize,
    pub chunk_idx: usize,
    pub anchor_name: String,
    pub anchor_path: String,
    pub destination_path: String,
    pub matched_input_count: usize,
    pub elapsed_ms: u64,
    pub dedup_report_rel_path: String,
    pub clean_report_rel_path: Option<String>,
    pub pairs_log_rel_path: Option<String>,
    pub direct_moves: Vec<MovedVideoRecord>,
    pub duplicate_groups: Vec<CorrelatedDupGroup>,
    pub symlinks_created: Vec<SymlinkRecord>,
    pub symlinks_skipped_existing: usize,
    pub bytes_moved: u64,
    pub bytes_reclaimed: u64,
    pub duplicates_deleted: usize,
    pub anchor_upgrades_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuarantinedRecord {
    pub file_path: String,
    pub anchor_path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunManifest {
    pub run_id: String,
    #[serde(default)]
    pub pid: u32,
    pub mode: String,
    /// `"IN_PROGRESS"` while executing, `"COMPLETED"` upon completion, `"INTERRUPTED"` if stopped/killed
    pub status: String,
    pub started_at_utc: String,
    pub started_at_unix_ms: u64,
    pub updated_at_utc: String,
    pub elapsed_secs: f64,
    #[serde(default = "default_live_refresh_secs")]
    pub live_refresh_secs: u64,
    pub inputs: Vec<String>,
    pub totals: RunTotals,
    pub anchor_batches: Vec<AnchorBatchAudit>,
    pub unmatched_files: Vec<String>,
    pub quarantined_files: Vec<QuarantinedRecord>,
}

fn default_live_refresh_secs() -> u64 {
    5
}

// ============================================================================
// 3. Persistent Per-Run Report Store (`reports/run_<YYYYMMDD_HHMMSS>_<pid>/`)
// ============================================================================

/// Planned persistent file paths for a single `(pass, seq, chunk, anchor)` execution step.
#[derive(Debug, Clone)]
pub struct BatchReportPaths {
    pub dedup_json_path: PathBuf,
    pub dedup_rel_path: String,
    pub clean_json_path: PathBuf,
    pub clean_rel_path: String,
    pub pairs_json_path: Option<PathBuf>,
    pub pairs_rel_path: Option<String>,
    /// Ephemeral copy path passed to `rusty_clean` so `dedup_json_path` stays 100% unmodified.
    pub scratch_clean_input_path: PathBuf,
    /// Ephemeral newline-delimited inputs list passed to `rusty_dedup --inputs-from` (eliminates ARG_MAX chunking).
    pub scratch_inputs_list_path: PathBuf,
    /// Ephemeral JSON move manifest passed to `rusty_dedup --record-moves`.
    pub scratch_moves_json_path: PathBuf,
}

pub struct RunReportStore {
    pub enabled_html: bool,
    pub capture_pairs_log: bool,
    pub reports_root: PathBuf,
    pub run_dir: PathBuf,
    pub dedup_dir: PathBuf,
    pub clean_dir: PathBuf,
    pub html_path: PathBuf,
    pub latest_html_path: PathBuf,
    pub manifest_path: PathBuf,
    pub manifest: RunManifest,
    start_instant: Instant,
}

impl RunReportStore {
    /// Initializes `reports/run_<YYYYMMDD_HHMMSS>_<pid>/{dedup,clean}/` on disk immediately
    /// so every `rusty_dedup` and `rusty_clean` JSON report is preserved even if interrupted.
    pub fn init(
        reports_root: &Path,
        enabled_html: bool,
        capture_pairs_log: bool,
        live_refresh_secs: u64,
        mode: &str,
        inputs: &[PathBuf],
    ) -> Result<Self> {
        let now_ms = current_unix_ms();
        let (compact_ts, human_utc) = format_utc_timestamps(now_ms / 1000);
        let pid = std::process::id();
        let run_id = format!("run_{}_{}", compact_ts, pid);

        let root_norm = normalize_path_lexical(reports_root);
        let run_dir = root_norm.join(&run_id);
        let dedup_dir = run_dir.join("dedup");
        let clean_dir = run_dir.join("clean");

        fs::create_dir_all(&dedup_dir)
            .with_context(|| format!("Failed to create persistent dedup report dir {:?}", dedup_dir))?;
        fs::create_dir_all(&clean_dir)
            .with_context(|| format!("Failed to create persistent clean report dir {:?}", clean_dir))?;

        let html_path = run_dir.join("execution_report.html");
        let latest_html_path = root_norm.join("latest_report.html");
        let manifest_path = run_dir.join("run_manifest.json");

        let manifest = RunManifest {
            run_id,
            pid,
            mode: mode.to_string(),
            status: "IN_PROGRESS".to_string(),
            started_at_utc: human_utc.clone(),
            started_at_unix_ms: now_ms,
            updated_at_utc: human_utc,
            elapsed_secs: 0.0,
            live_refresh_secs,
            inputs: inputs
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect(),
            totals: RunTotals::default(),
            anchor_batches: Vec::new(),
            unmatched_files: Vec::new(),
            quarantined_files: Vec::new(),
        };

        let mut store = Self {
            enabled_html,
            capture_pairs_log,
            reports_root: root_norm,
            run_dir,
            dedup_dir,
            clean_dir,
            html_path,
            latest_html_path,
            manifest_path,
            manifest,
            start_instant: Instant::now(),
        };
        store.flush_to_disk()?;
        Ok(store)
    }

    /// Computes collision-free, human-readable persistent JSON report paths inside `dedup/` and `clean/`.
    pub fn prepare_batch_paths(
        &self,
        pass_num: usize,
        seq_id: usize,
        chunk_idx: usize,
        anchor: &AnchorEntry,
    ) -> BatchReportPaths {
        let slug = slugify_anchor_name(&anchor.anchor_path);
        let prefix = format!("{:04}_p{}_c{}_{}", seq_id, pass_num, chunk_idx, slug);

        let dedup_rel = format!("dedup/{}_dups.json", prefix);
        let clean_rel = format!("clean/{}_clean.json", prefix);

        let (pairs_json_path, pairs_rel_path) = if self.capture_pairs_log {
            let prel = format!("dedup/{}_pairs.json", prefix);
            (Some(self.run_dir.join(&prel)), Some(prel))
        } else {
            (None, None)
        };

        let scratch_clean_input_path = self
            .clean_dir
            .join(format!(".scratch_input_{}.json", prefix));
        let scratch_inputs_list_path = self
            .clean_dir
            .join(format!(".scratch_inputs_{}.txt", prefix));
        let scratch_moves_json_path = self
            .clean_dir
            .join(format!(".scratch_moves_{}.json", prefix));

        BatchReportPaths {
            dedup_json_path: self.run_dir.join(&dedup_rel),
            dedup_rel_path: dedup_rel,
            clean_json_path: self.run_dir.join(&clean_rel),
            clean_rel_path: clean_rel,
            pairs_json_path,
            pairs_rel_path,
            scratch_clean_input_path,
            scratch_inputs_list_path,
            scratch_moves_json_path,
        }
    }

    /// Records a completed anchor sub-batch and immediately flushes `run_manifest.json` + HTML reports.
    pub fn record_anchor_batch(&mut self, batch_audit: AnchorBatchAudit) -> Result<()> {
        self.manifest.anchor_batches.push(batch_audit);
        self.recompute_totals();
        self.flush_to_disk()
    }

    pub fn record_quarantined(&mut self, file_path: &Path, anchor_path: &Path, reason: &str) {
        self.manifest.quarantined_files.push(QuarantinedRecord {
            file_path: file_path.to_string_lossy().to_string(),
            anchor_path: anchor_path.to_string_lossy().to_string(),
            reason: reason.to_string(),
        });
    }

    /// Marks the execution run `INTERRUPTED` (e.g. Ctrl-C / SIGTERM) and flushes final manifest + HTML.
    pub fn mark_interrupted(
        &mut self,
        passes_executed: usize,
        quarantined_error_count: usize,
    ) -> Result<()> {
        self.manifest.status = "INTERRUPTED".to_string();
        self.recompute_totals();
        if passes_executed > 0 {
            self.manifest.totals.passes_executed = passes_executed;
        }
        self.manifest.totals.quarantined_error_count =
            quarantined_error_count.max(self.manifest.quarantined_files.len());
        self.flush_to_disk()
    }

    /// Marks the execution run `COMPLETED`, records final pass & unmatched counts, and writes final HTML.
    pub fn finalize(
        &mut self,
        passes_executed: usize,
        unmatched_files: &[PathBuf],
        quarantined_error_count: usize,
    ) -> Result<()> {
        self.manifest.status = "COMPLETED".to_string();
        self.manifest.unmatched_files = unmatched_files
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        self.recompute_totals();
        self.manifest.totals.passes_executed = passes_executed;
        self.manifest.totals.unmatched_count = self.manifest.unmatched_files.len();
        self.manifest.totals.quarantined_error_count =
            quarantined_error_count.max(self.manifest.quarantined_files.len());
        self.flush_to_disk()
    }

    fn recompute_totals(&mut self) {
        let mut t = RunTotals {
            passes_executed: self
                .manifest
                .anchor_batches
                .iter()
                .map(|b| b.pass_num)
                .max()
                .unwrap_or(0),
            unmatched_count: self.manifest.unmatched_files.len(),
            quarantined_error_count: self.manifest.quarantined_files.len(),
            ..RunTotals::default()
        };

        for b in &self.manifest.anchor_batches {
            t.anchor_batches += 1;
            t.dedup_runs += 1;
            if b.clean_report_rel_path.is_some() {
                t.clean_runs += 1;
            }
            t.misc_files_evaluated += b.matched_input_count;
            for mv in &b.direct_moves {
                if mv.move_kind == "CLEAN_PHASE2_NON_DUP" {
                    t.phase2_nondup_moves += 1;
                } else {
                    t.direct_moves += 1;
                }
            }
            t.duplicate_groups += b.duplicate_groups.len();
            t.duplicates_deleted += b.duplicates_deleted;
            for grp in &b.duplicate_groups {
                match grp.outcome.as_str() {
                    "ANCHOR_UPGRADED" => t.anchor_upgrades += 1,
                    "EXISTING_ANCHOR_KEPT" => t.existing_anchor_kept += 1,
                    "MISC_BATCH_DEDUPED" => t.misc_batch_deduped += 1,
                    _ => t.intra_anchor_cleanup += 1,
                }
            }
            t.symlinks_created += b.symlinks_created.len();
            t.symlinks_skipped_existing += b.symlinks_skipped_existing;
            t.bytes_moved += b.bytes_moved;
            t.bytes_reclaimed += b.bytes_reclaimed;
        }
        self.manifest.totals = t;
    }

    pub fn flush_to_disk(&mut self) -> Result<()> {
        let now_ms = current_unix_ms();
        let (_, human_utc) = format_utc_timestamps(now_ms / 1000);
        self.manifest.updated_at_utc = human_utc;
        self.manifest.elapsed_secs = self.start_instant.elapsed().as_secs_f64();

        let manifest_json = serde_json::to_string_pretty(&self.manifest)?;
        write_file_atomic(&self.manifest_path, manifest_json.as_bytes())?;

        if self.enabled_html {
            let run_html = render_execution_report_html(&self.manifest, "");
            write_file_atomic(&self.html_path, run_html.as_bytes())?;

            let latest_prefix = format!("{}/", self.manifest.run_id);
            let latest_html = render_execution_report_html(&self.manifest, &latest_prefix);
            let _ = write_file_atomic(&self.latest_html_path, latest_html.as_bytes());
            // Also write `index.html` so even `python3 -m http.server --directory ./reports` works at `/`
            let index_fallback_path = self.reports_root.join("index.html");
            let _ = write_file_atomic(&index_fallback_path, latest_html.as_bytes());
        }
        Ok(())
    }
}

// ============================================================================
// 4. Correlation Logic: `rusty_dedup` + `rusty_clean` + Scanner Match Metadata
// ============================================================================

/// Correlates a `DedupReportFull` + `Vec<CleanGroupDecision>` + optional `Vec<PairEvaluationEntry>`
/// + scanner `batch` metadata into rich `CorrelatedDupGroup` items with outcome & byte accounting.
pub fn correlate_duplicate_groups(
    anchor_path: &Path,
    canonical_anchor_path: &Path,
    destination_path: &Path,
    batch: &[FileMatchResult],
    dedup_report: &DedupReportFull,
    clean_decisions: &[CleanGroupDecision],
    pair_logs: &[PairEvaluationEntry],
) -> (Vec<CorrelatedDupGroup>, u64, u64, usize, usize) {
    // Map normalized/canonical paths of incoming misc files -> `&FileMatchResult`
    let mut batch_by_path: HashMap<PathBuf, &FileMatchResult> = HashMap::new();
    for item in batch {
        batch_by_path.insert(normalize_path_lexical(&item.candidate.path), item);
        if let Ok(can) = fs::canonicalize(&item.candidate.path) {
            batch_by_path.insert(can, item);
        } else if let (Some(parent), Some(fname)) =
            (item.candidate.path.parent(), item.candidate.path.file_name())
        {
            if let Ok(can_p) = fs::canonicalize(parent) {
                batch_by_path.insert(can_p.join(fname), item);
            }
        }
    }

    // Also map `dedup_report.duplicates` by `dup_group_name` so we can backfill attributes if needed
    let mut dedup_group_map: HashMap<&str, &DedupGroupFull> = HashMap::new();
    for dg in &dedup_report.duplicates {
        dedup_group_map.insert(dg.dup_group_name.as_str(), dg);
    }

    let norm_anchor = normalize_path_lexical(anchor_path);
    let norm_dest = normalize_path_lexical(destination_path);
    let can_dest = fs::canonicalize(destination_path).unwrap_or_else(|_| norm_dest.clone());

    let is_anchor_origin = |p: &Path| -> bool {
        let np = normalize_path_lexical(p);
        if batch_by_path.contains_key(&np) || batch_by_path.contains_key(p) {
            return false;
        }
        np.starts_with(&norm_anchor)
            || np.starts_with(canonical_anchor_path)
            || np.starts_with(&norm_dest)
            || np.starts_with(&can_dest)
    };

    let mut out_groups = Vec::with_capacity(clean_decisions.len().max(dedup_report.duplicates.len()));
    let mut total_reclaimed = 0u64;
    let mut total_moved = 0u64;
    let mut total_deleted = 0usize;
    let mut total_upgrades = 0usize;

    for cgrp in clean_decisions {
        let dgrp = dedup_group_map.get(cgrp.dup_group_name.as_str()).copied();
        let mut action_by_path: HashMap<&str, &CleanFileAction> = HashMap::new();
        for act in &cgrp.actions {
            action_by_path.insert(act.file_path.as_str(), act);
        }

        // Determine winner index in `cgrp.scores`:
        // 1. Explicit `RETAIN*` action match
        // 2. Path matching `cgrp.retained_winner`
        // 3. Highest `total_normalized_score` (or index 0 fallback)
        let winner_idx = cgrp
            .scores
            .iter()
            .position(|s| {
                action_by_path
                    .get(s.file_path.as_str())
                    .map(|a| a.decision.starts_with("RETAIN"))
                    .unwrap_or(false)
            })
            .or_else(|| {
                cgrp.scores
                    .iter()
                    .position(|s| s.file_path == cgrp.retained_winner)
            })
            .or_else(|| {
                cgrp.scores
                    .iter()
                    .enumerate()
                    .max_by(|(_, a), (_, b)| {
                        a.total_normalized_score
                            .partial_cmp(&b.total_normalized_score)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .map(|(idx, _)| idx)
            })
            .unwrap_or(0);

        let mut members: Vec<CorrelatedGroupMember> = Vec::with_capacity(cgrp.scores.len());
        let mut had_anchor_member = false;
        let mut had_misc_member = false;
        let mut winner_was_misc = false;
        let mut grp_reclaimed = 0u64;
        let mut grp_moved = 0u64;
        let mut winner_orig_path = cgrp.retained_winner.clone();

        for (idx, raw_sc) in cgrp.scores.iter().enumerate() {
            let is_winner = idx == winner_idx;
            let p = PathBuf::from(&raw_sc.file_path);
            let norm_p = normalize_path_lexical(&p);

            let matched_item = batch_by_path
                .get(&norm_p)
                .or_else(|| batch_by_path.get(&p))
                .copied();

            let from_anchor = is_anchor_origin(&p);
            if from_anchor {
                had_anchor_member = true;
            } else {
                had_misc_member = true;
            }
            if is_winner {
                winner_was_misc = !from_anchor;
                winner_orig_path = raw_sc.file_path.clone();
            }

            // Backfill raw attributes from `dedup_report` or `matched_item` if `CleanScoreBreakdown` came from a minimal report
            let mut sc = raw_sc.clone();
            if let Some(dg) = dgrp {
                if let Some(attr) = dg.attributes.get(&sc.file_path) {
                    if sc.size_raw == 0 {
                        sc.size_raw = attr.size_in_bytes;
                    }
                    if sc.runtime_raw == 0 {
                        sc.runtime_raw = attr.runtime_in_seconds;
                    }
                    if sc.height_raw == 0 {
                        sc.height_raw = attr.video_height;
                    }
                    if sc.width_raw == 0 {
                        sc.width_raw = attr.video_width;
                    }
                    if sc.video_bitrate_raw == 0 {
                        sc.video_bitrate_raw = attr.video_bitrate_bps;
                    }
                    if sc.audio_bitrate_raw == 0 {
                        sc.audio_bitrate_raw = attr.audio_bitrate_bps;
                    }
                    if sc.video_codec_raw.is_empty() {
                        sc.video_codec_raw = attr.video_codec.clone();
                    }
                    if sc.audio_codec_raw.is_empty() {
                        sc.audio_codec_raw = attr.audio_codec.clone();
                    }
                }
            }
            if sc.size_raw == 0 {
                if let Some(mi) = matched_item {
                    sc.size_raw = mi.candidate.size_bytes;
                }
            }

            let act = action_by_path.get(raw_sc.file_path.as_str()).copied();
            let decision = act
                .map(|a| a.decision.clone())
                .unwrap_or_else(|| {
                    if is_winner {
                        "RETAIN".to_string()
                    } else {
                        "DELETE".to_string()
                    }
                });
            let moved_to = act.and_then(|a| a.moved_to.clone()).or_else(|| {
                if is_winner && cgrp.retained_winner != raw_sc.file_path && !cgrp.retained_winner.is_empty() {
                    Some(cgrp.retained_winner.clone())
                } else {
                    None
                }
            });
            let action_taken = act
                .map(|a| a.action_taken.clone())
                .unwrap_or_else(|| {
                    if is_winner {
                        if let Some(ref mt) = moved_to {
                            format!("Retained winner and moved to {}", mt)
                        } else {
                            "Retained highest-scoring file".to_string()
                        }
                    } else {
                        "Deleted duplicate file".to_string()
                    }
                });

            if is_winner {
                if moved_to.is_some() {
                    grp_moved += sc.size_raw;
                }
            } else {
                grp_reclaimed += sc.size_raw;
                total_deleted += 1;
            }

            let primary_match = matched_item.and_then(|m| m.matches.first());
            members.push(CorrelatedGroupMember {
                is_winner,
                origin: if from_anchor {
                    "ANCHOR".to_string()
                } else {
                    "MISC".to_string()
                },
                file_path: raw_sc.file_path.clone(),
                decision,
                action_taken,
                moved_to,
                matched_alias: primary_match.map(|pm| pm.matched_alias.clone()),
                match_tier: primary_match.map(|pm| pm.match_tier.as_str().to_string()),
                scanner_confidence: primary_match.map(|pm| pm.confidence_score),
                score: sc,
            });
        }

        // Sort members so Winner always appears first, followed by highest-scoring losers
        members.sort_by(|a, b| {
            b.is_winner.cmp(&a.is_winner).then_with(|| {
                b.score
                    .total_normalized_score
                    .partial_cmp(&a.score.total_normalized_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        });

        let outcome = if had_anchor_member && had_misc_member {
            if winner_was_misc {
                total_upgrades += 1;
                "ANCHOR_UPGRADED"
            } else {
                "EXISTING_ANCHOR_KEPT"
            }
        } else if had_misc_member {
            "MISC_BATCH_DEDUPED"
        } else {
            "INTRA_ANCHOR_CLEANUP"
        };

        // Attach any pair-evaluation log entries whose `file1` & `file2` belong to this group
        let member_set: std::collections::HashSet<&str> =
            members.iter().map(|m| m.file_path.as_str()).collect();
        let pair_evaluations: Vec<PairEvaluationEntry> = pair_logs
            .iter()
            .filter(|pl| {
                member_set.contains(pl.file1.as_str()) && member_set.contains(pl.file2.as_str())
            })
            .cloned()
            .collect();

        let group_size = if cgrp.group_size > 0 {
            cgrp.group_size
        } else {
            members.len()
        };
        let winner_score = if cgrp.winner_score > 0.0 {
            cgrp.winner_score
        } else {
            members
                .first()
                .map(|m| m.score.total_normalized_score)
                .unwrap_or(0.0)
        };

        total_reclaimed += grp_reclaimed;
        total_moved += grp_moved;

        out_groups.push(CorrelatedDupGroup {
            dup_group_name: cgrp.dup_group_name.clone(),
            outcome: outcome.to_string(),
            group_size,
            retained_winner: cgrp.retained_winner.clone(),
            winner_orig_path,
            winner_score,
            max_possible_score: 9.0,
            tie_broken_by_shortest_path: cgrp.tie_broken_by_shortest_path,
            bytes_reclaimed: grp_reclaimed,
            bytes_moved: grp_moved,
            members,
            pair_evaluations,
        });
    }

    (
        out_groups,
        total_reclaimed,
        total_moved,
        total_deleted,
        total_upgrades,
    )
}

// ============================================================================
// 5. Standalone `--build-report <RUN_DIR>` HTML Regeneration
// ============================================================================

/// Extracts the owning process ID from `manifest.pid` or trailing `run_<date>_<time>_<pid>`.
pub fn extract_manifest_pid(manifest: &RunManifest) -> Option<u32> {
    if manifest.pid > 0 {
        return Some(manifest.pid);
    }
    parse_trailing_pid(&manifest.run_id)
}

fn parse_trailing_pid(run_id: &str) -> Option<u32> {
    run_id
        .rsplit('_')
        .next()?
        .parse::<u32>()
        .ok()
        .filter(|&p| p > 0)
}

/// Zero-subprocess PID liveness check via POSIX `kill(pid, 0)`.
#[cfg(unix)]
pub fn is_pid_alive_fast(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    if pid == std::process::id() {
        return true;
    }
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    let rc = unsafe { kill(pid as i32, 0) };
    if rc == 0 {
        return true;
    }
    // EPERM (1) indicates the process is alive under a different UID; ESRCH (3) means exited.
    std::io::Error::last_os_error().raw_os_error() == Some(1)
}

#[cfg(not(unix))]
pub fn is_pid_alive_fast(pid: u32) -> bool {
    pid == std::process::id()
}

/// Reconciles dead-PID `"IN_PROGRESS"` runs to `"INTERRUPTED"` and normalizes any legacy
/// `seq_id * 100` jumps (`4100, 4101, 4102`) to contiguous `1..=N` execution order, persisting
/// the healed `run_manifest.json` + `execution_report.html` atomically when changed.
pub fn reconcile_and_persist_manifest(run_dir: &Path, manifest: &mut RunManifest) -> bool {
    let mut changed = false;
    if manifest.pid == 0 {
        if let Some(parsed_pid) = parse_trailing_pid(&manifest.run_id) {
            manifest.pid = parsed_pid;
        }
    }

    if manifest.status == "IN_PROGRESS" {
        if let Some(pid) = extract_manifest_pid(manifest) {
            if !is_pid_alive_fast(pid) {
                manifest.status = "INTERRUPTED".to_string();
                changed = true;
            }
        }
    }

    if manifest
        .anchor_batches
        .iter()
        .enumerate()
        .any(|(idx, b)| b.seq_id != idx + 1)
    {
        for (idx, b) in manifest.anchor_batches.iter_mut().enumerate() {
            b.seq_id = idx + 1;
        }
        changed = true;
    }

    if changed {
        let manifest_path = run_dir.join("run_manifest.json");
        if let Ok(json) = serde_json::to_string_pretty(manifest) {
            let _ = write_file_atomic(&manifest_path, json.as_bytes());
        }
        let html_path = run_dir.join("execution_report.html");
        let html = render_execution_report_html(manifest, "");
        let _ = write_file_atomic(&html_path, html.as_bytes());
    }

    changed
}

/// Refreshes `latest_report.html` and `index.html` in `reports_root` to match `manifest`.
pub fn refresh_latest_report_shortcuts(reports_root: &Path, manifest: &RunManifest) {
    let prefix = format!("{}/", manifest.run_id);
    let latest_html = render_execution_report_html(manifest, &prefix);
    let _ = write_file_atomic(&reports_root.join("latest_report.html"), latest_html.as_bytes());
    let _ = write_file_atomic(&reports_root.join("index.html"), latest_html.as_bytes());
}

/// Rebuilds `execution_report.html` (and updates `../latest_report.html` if inside a `reports/` parent)
/// from an existing run directory — using `run_manifest.json` if present, or reconstructing directly
/// from saved `dedup/*.json` and `clean/*.json` files!
pub fn rebuild_html_from_run_dir(run_dir: &Path) -> Result<PathBuf> {
    if !run_dir.is_dir() {
        bail!("Specified run directory {:?} does not exist or is not a directory.", run_dir);
    }
    let norm_run_dir = normalize_path_lexical(run_dir);
    let manifest_path = norm_run_dir.join("run_manifest.json");

    let mut manifest: RunManifest = if manifest_path.is_file() {
        let raw = fs::read_to_string(&manifest_path)
            .with_context(|| format!("Failed to read {:?}", manifest_path))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("Failed to parse {:?}", manifest_path))?
    } else {
        reconstruct_manifest_from_raw_jsons(&norm_run_dir)?
    };

    let reconciled = reconcile_and_persist_manifest(&norm_run_dir, &mut manifest);
    if !manifest_path.is_file() || reconciled {
        let json = serde_json::to_string_pretty(&manifest)?;
        write_file_atomic(&manifest_path, json.as_bytes())?;
    }

    let html_path = norm_run_dir.join("execution_report.html");
    let html = render_execution_report_html(&manifest, "");
    write_file_atomic(&html_path, html.as_bytes())?;

    if let Some(parent) = norm_run_dir.parent() {
        let latest_cand = parent.join("latest_report.html");
        if latest_cand.exists()
            || parent
                .file_name()
                .and_then(|s| s.to_str())
                .map(|n| n.contains("report"))
                .unwrap_or(false)
        {
            // Only overwrite `latest_report.html` / `index.html` if this run is the newest run in `parent`
            let is_newest = fs::read_dir(parent)
                .ok()
                .map(|rd| {
                    let mut run_names: Vec<String> = rd
                        .filter_map(|e| e.ok())
                        .filter(|e| e.path().is_dir())
                        .map(|e| e.file_name().to_string_lossy().to_string())
                        .filter(|n| n.starts_with("run_"))
                        .collect();
                    run_names.sort();
                    run_names.last().map(|s| s.as_str()) == Some(manifest.run_id.as_str())
                })
                .unwrap_or(true);
            if is_newest {
                refresh_latest_report_shortcuts(parent, &manifest);
            }
        }
    }

    Ok(html_path)
}

fn reconstruct_manifest_from_raw_jsons(run_dir: &Path) -> Result<RunManifest> {
    let dedup_dir = run_dir.join("dedup");
    let clean_dir = run_dir.join("clean");
    if !dedup_dir.is_dir() && !clean_dir.is_dir() {
        bail!(
            "No `run_manifest.json`, `dedup/`, or `clean/` subdirectories found inside {:?}",
            run_dir
        );
    }

    let run_id = run_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("rebuilt_run")
        .to_string();
    let now_ms = current_unix_ms();
    let (_, human_utc) = format_utc_timestamps(now_ms / 1000);

    let mut dedup_files: Vec<PathBuf> = Vec::new();
    if dedup_dir.is_dir() {
        for entry in fs::read_dir(&dedup_dir)?.filter_map(|e| e.ok()) {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if p.is_file() && name.ends_with("_dups.json") {
                dedup_files.push(p);
            }
        }
    }
    dedup_files.sort();

    let mut anchor_batches = Vec::new();
    let mut totals = RunTotals::default();

    for (idx, dpath) in dedup_files.iter().enumerate() {
        let dname = dpath
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("dups.json");
        let stem_prefix = dname.trim_end_matches("_dups.json");
        let dedup_rel = format!("dedup/{}", dname);

        let clean_cand_name = format!("{}_clean.json", stem_prefix);
        let clean_cand_path = clean_dir.join(&clean_cand_name);
        let clean_rel = if clean_cand_path.is_file() {
            Some(format!("clean/{}", clean_cand_name))
        } else {
            None
        };

        let pairs_cand_name = format!("{}_pairs.json", stem_prefix);
        let pairs_cand_path = dedup_dir.join(&pairs_cand_name);
        let pairs_rel = if pairs_cand_path.is_file() {
            Some(format!("dedup/{}", pairs_cand_name))
        } else {
            None
        };

        let dedup_raw = fs::read_to_string(dpath)?;
        let dedup_rep: DedupReportFull = serde_json::from_str(&dedup_raw).unwrap_or_default();

        let clean_decisions: Vec<CleanGroupDecision> = if clean_cand_path.is_file() {
            let craw = fs::read_to_string(&clean_cand_path)?;
            serde_json::from_str(&craw).unwrap_or_default()
        } else {
            Vec::new()
        };

        let pair_logs: Vec<PairEvaluationEntry> = if pairs_cand_path.is_file() {
            let praw = fs::read_to_string(&pairs_cand_path)?;
            serde_json::from_str(&praw).unwrap_or_default()
        } else {
            Vec::new()
        };

        let anchor_str = dedup_rep
            .input_directories
            .first()
            .cloned()
            .unwrap_or_else(|| stem_prefix.to_string());
        let anchor_path = PathBuf::from(&anchor_str);
        let anchor_name = anchor_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&anchor_str)
            .to_string();
        let matched_input_count = dedup_rep.input_directories.len().saturating_sub(1);

        let (dup_groups, reclaimed, moved, deleted, upgrades) = correlate_duplicate_groups(
            &anchor_path,
            &anchor_path,
            &anchor_path,
            &[],
            &dedup_rep,
            &clean_decisions,
            &pair_logs,
        );

        totals.anchor_batches += 1;
        totals.dedup_runs += 1;
        if clean_rel.is_some() {
            totals.clean_runs += 1;
        }
        totals.misc_files_evaluated += matched_input_count;
        totals.duplicate_groups += dup_groups.len();
        totals.duplicates_deleted += deleted;
        for g in &dup_groups {
            match g.outcome.as_str() {
                "ANCHOR_UPGRADED" => totals.anchor_upgrades += 1,
                "EXISTING_ANCHOR_KEPT" => totals.existing_anchor_kept += 1,
                "MISC_BATCH_DEDUPED" => totals.misc_batch_deduped += 1,
                _ => totals.intra_anchor_cleanup += 1,
            }
        }
        totals.bytes_reclaimed += reclaimed;
        totals.bytes_moved += moved;

        anchor_batches.push(AnchorBatchAudit {
            seq_id: idx + 1,
            pass_num: 1,
            chunk_idx: 0,
            anchor_name,
            anchor_path: anchor_str.clone(),
            destination_path: anchor_str,
            matched_input_count,
            elapsed_ms: 0,
            dedup_report_rel_path: dedup_rel,
            clean_report_rel_path: clean_rel,
            pairs_log_rel_path: pairs_rel,
            direct_moves: Vec::new(),
            duplicate_groups: dup_groups,
            symlinks_created: Vec::new(),
            symlinks_skipped_existing: 0,
            bytes_moved: moved,
            bytes_reclaimed: reclaimed,
            duplicates_deleted: deleted,
            anchor_upgrades_count: upgrades,
            errors: Vec::new(),
        });
    }

    totals.passes_executed = if anchor_batches.is_empty() { 0 } else { 1 };

    let pid = parse_trailing_pid(&run_id).unwrap_or(0);
    Ok(RunManifest {
        run_id,
        pid,
        mode: "REBUILT_FROM_JSON".to_string(),
        status: "COMPLETED".to_string(),
        started_at_utc: human_utc.clone(),
        started_at_unix_ms: now_ms,
        updated_at_utc: human_utc,
        elapsed_secs: 0.0,
        live_refresh_secs: 5,
        inputs: Vec::new(),
        totals,
        anchor_batches,
        unmatched_files: Vec::new(),
        quarantined_files: Vec::new(),
    })
}

// ============================================================================
// 6. Self-Contained Interactive Drill-Down HTML5 + CSS + JS Renderer
// ============================================================================

/// Renders a single, zero-dependency, offline-capable HTML5 + CSS + JS drill-down dashboard.
/// `rel_link_prefix` is `""` inside `run_<id>/execution_report.html` and `"run_<id>/"` inside `latest_report.html`.
pub fn render_execution_report_html(manifest: &RunManifest, rel_link_prefix: &str) -> String {
    let raw_json = serde_json::to_string(manifest).unwrap_or_else(|_| "{}".to_string());
    // Safely escape `</` and `<!--` so filenames inside JSON can never prematurely close `<script>`
    let safe_json = raw_json
        .replace("</", "<\\/")
        .replace("<!--", "<\\!--");
    let safe_prefix = serde_json::to_string(rel_link_prefix).unwrap_or_else(|_| "\"\"".to_string());

    let mut html = String::with_capacity(safe_json.len() + 48 * 1024);
    html.push_str(HTML_TEMPLATE_HEAD);
    html.push_str(&escape_html_text(&format!(
        "Rusty Intelli Scanner — Audit Report ({})",
        manifest.run_id
    )));
    html.push_str(HTML_TEMPLATE_STYLE_AND_BODY_START);
    html.push_str(&safe_json);
    html.push_str("</script>\n<script>\nconst REL_LINK_PREFIX = ");
    html.push_str(&safe_prefix);
    html.push_str(";\n");
    html.push_str(HTML_TEMPLATE_SCRIPT_AND_END);
    html
}

fn escape_html_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn slugify_anchor_name(anchor_path: &Path) -> String {
    let base = anchor_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("anchor");
    let mut slug = String::with_capacity(base.len());
    let mut prev_underscore = false;
    for ch in base.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_underscore = false;
        } else if !prev_underscore && !slug.is_empty() {
            slug.push('_');
            prev_underscore = true;
        }
        if slug.len() >= 48 {
            break;
        }
    }
    let trimmed = slug.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "anchor".to_string()
    } else {
        trimmed
    }
}

fn write_file_atomic(target: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp_name = format!(
        ".{}.tmp_{}",
        target
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("report"),
        std::process::id()
    );
    let tmp_path = target.with_file_name(tmp_name);
    {
        let mut f = File::create(&tmp_path)
            .with_context(|| format!("Failed to create temp file {:?}", tmp_path))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp_path, target)
        .with_context(|| format!("Failed to atomically replace {:?}", target))?;
    Ok(())
}

fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Pure-Rust civil UTC calendar conversion (Howard Hinnant's algorithm — zero external crates).
fn format_utc_timestamps(unix_secs: u64) -> (String, String) {
    let days = (unix_secs / 86_400) as i64;
    let rem = unix_secs % 86_400;
    let hour = rem / 3600;
    let min = (rem % 3600) / 60;
    let sec = rem % 60;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    let compact = format!("{:04}{:02}{:02}_{:02}{:02}{:02}", year, m, d, hour, min, sec);
    let human = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        year, m, d, hour, min, sec
    );
    (compact, human)
}

const HTML_TEMPLATE_HEAD: &str = r#"<!DOCTYPE html>
<html lang="en" data-theme="dark">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>"#;

const HTML_TEMPLATE_STYLE_AND_BODY_START: &str = r#"</title>
<style>
:root, [data-theme="dark"] {
  --bg: #0b0f17;
  --bg-card: #121826;
  --bg-elevated: #1a2234;
  --bg-hover: #212c42;
  --border: #28354f;
  --text: #e6edf7;
  --text-muted: #93a4bf;
  --accent: #4f9cf9;
  --accent-soft: rgba(79, 156, 249, 0.14);
  --green: #2dd4bf;
  --green-soft: rgba(45, 212, 191, 0.14);
  --emerald: #34d399;
  --emerald-soft: rgba(52, 211, 153, 0.16);
  --amber: #fbbf24;
  --amber-soft: rgba(251, 191, 36, 0.15);
  --purple: #a78bfa;
  --purple-soft: rgba(167, 139, 250, 0.15);
  --red: #f87171;
  --red-soft: rgba(248, 113, 113, 0.15);
  --mono: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  --sans: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
}
[data-theme="light"] {
  --bg: #f4f7fb;
  --bg-card: #ffffff;
  --bg-elevated: #eef3f9;
  --bg-hover: #e3ebf6;
  --border: #d0dbe8;
  --text: #0f172a;
  --text-muted: #475569;
  --accent: #2563eb;
  --accent-soft: rgba(37, 99, 235, 0.10);
  --green: #0d9488;
  --green-soft: rgba(13, 148, 136, 0.11);
  --emerald: #059669;
  --emerald-soft: rgba(5, 150, 105, 0.12);
  --amber: #d97706;
  --amber-soft: rgba(217, 119, 6, 0.12);
  --purple: #7c3aed;
  --purple-soft: rgba(124, 58, 237, 0.11);
  --red: #dc2626;
  --red-soft: rgba(220, 38, 38, 0.11);
}
* { box-sizing: border-box; }
body {
  margin: 0;
  padding: 0;
  background: var(--bg);
  color: var(--text);
  font-family: var(--sans);
  font-size: 14px;
  line-height: 1.45;
}
.container {
  max-width: 1540px;
  margin: 0 auto;
  padding: 20px 24px 64px;
}
.topbar {
  display: flex;
  flex-wrap: wrap;
  justify-content: space-between;
  align-items: center;
  gap: 14px;
  padding-bottom: 16px;
  border-bottom: 1px solid var(--border);
  margin-bottom: 18px;
}
.title-group h1 {
  margin: 0;
  font-size: 20px;
  font-weight: 700;
  letter-spacing: -0.01em;
  display: flex;
  align-items: center;
  gap: 10px;
}
.subtitle {
  margin-top: 4px;
  font-size: 12.5px;
  color: var(--text-muted);
  font-family: var(--mono);
}
.status-pill {
  font-size: 11px;
  font-weight: 700;
  padding: 3px 9px;
  border-radius: 999px;
  text-transform: uppercase;
  letter-spacing: 0.04em;
}
.status-completed { background: var(--emerald-soft); color: var(--emerald); border: 1px solid var(--emerald); }
.status-progress { background: var(--amber-soft); color: var(--amber); border: 1px solid var(--amber); }
.status-stopped { background: var(--red-soft); color: var(--red); border: 1px solid var(--red); }

.btn {
  background: var(--bg-elevated);
  color: var(--text);
  border: 1px solid var(--border);
  border-radius: 7px;
  padding: 7px 12px;
  font-size: 12.5px;
  font-weight: 600;
  cursor: pointer;
  transition: background 0.12s, border-color 0.12s;
}
.btn:hover { background: var(--bg-hover); border-color: var(--accent); }
.btn.active { background: var(--accent-soft); border-color: var(--accent); color: var(--accent); }

.kpi-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(195px, 1fr));
  gap: 12px;
  margin-bottom: 18px;
}
.kpi-card {
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: 10px;
  padding: 13px 15px;
  cursor: pointer;
  transition: transform 0.1s, border-color 0.15s, background 0.15s;
  user-select: none;
}
.kpi-card:hover { border-color: var(--accent); background: var(--bg-elevated); }
.kpi-card.selected { border-color: var(--accent); box-shadow: 0 0 0 1px var(--accent); background: var(--accent-soft); }
.kpi-label { font-size: 11.5px; text-transform: uppercase; letter-spacing: 0.04em; color: var(--text-muted); font-weight: 600; }
.kpi-value { font-size: 24px; font-weight: 800; margin-top: 4px; font-family: var(--mono); }
.kpi-sub { font-size: 12px; color: var(--text-muted); margin-top: 3px; }
.val-emerald { color: var(--emerald); }
.val-accent { color: var(--accent); }
.val-amber { color: var(--amber); }
.val-purple { color: var(--purple); }

.toolbar {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
  align-items: center;
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: 10px;
  padding: 11px 14px;
  margin-bottom: 16px;
  position: sticky;
  top: 8px;
  z-index: 20;
  box-shadow: 0 6px 20px rgba(0,0,0,0.22);
}
.search-box {
  flex: 1 1 280px;
  background: var(--bg);
  color: var(--text);
  border: 1px solid var(--border);
  border-radius: 7px;
  padding: 8px 12px;
  font-size: 13.5px;
  outline: none;
}
.search-box:focus { border-color: var(--accent); }
.select-box {
  background: var(--bg);
  color: var(--text);
  border: 1px solid var(--border);
  border-radius: 7px;
  padding: 7px 10px;
  font-size: 12.5px;
}
.filter-pills { display: flex; flex-wrap: wrap; gap: 6px; }

.results-meta {
  font-size: 12.5px;
  color: var(--text-muted);
  margin-bottom: 10px;
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.anchor-card {
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: 10px;
  margin-bottom: 11px;
  overflow: hidden;
}
.anchor-header {
  display: flex;
  flex-wrap: wrap;
  justify-content: space-between;
  align-items: center;
  gap: 10px;
  padding: 13px 16px;
  cursor: pointer;
  background: var(--bg-card);
  transition: background 0.12s;
}
.anchor-header:hover { background: var(--bg-hover); }
.anchor-left {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
  flex: 1 1 420px;
}
.chevron {
  font-size: 12px;
  width: 18px;
  text-align: center;
  color: var(--text-muted);
  transition: transform 0.15s;
}
.anchor-card.open .chevron { transform: rotate(90deg); color: var(--accent); }
.seq-badge {
  font-family: var(--mono);
  font-size: 11px;
  padding: 2px 7px;
  border-radius: 5px;
  background: var(--bg-elevated);
  color: var(--text-muted);
  border: 1px solid var(--border);
  white-space: nowrap;
}
.anchor-title-wrap { min-width: 0; }
.anchor-name { font-weight: 700; font-size: 14.5px; word-break: break-all; }
.anchor-path { font-family: var(--mono); font-size: 11.5px; color: var(--text-muted); word-break: break-all; }
.anchor-right {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 7px;
}
.badge {
  font-size: 11.5px;
  font-weight: 600;
  padding: 3px 8px;
  border-radius: 6px;
  font-family: var(--mono);
  white-space: nowrap;
}
.b-move { background: var(--accent-soft); color: var(--accent); }
.b-dup { background: var(--purple-soft); color: var(--purple); }
.b-upgrade { background: var(--emerald-soft); color: var(--emerald); border: 1px solid rgba(52,211,153,0.4); }
.b-kept { background: var(--green-soft); color: var(--green); }
.b-saved { background: var(--amber-soft); color: var(--amber); }
.b-err { background: var(--red-soft); color: var(--red); }
.b-muted { background: var(--bg-elevated); color: var(--text-muted); }

.json-link {
  font-family: var(--mono);
  font-size: 11px;
  padding: 3px 7px;
  border-radius: 5px;
  background: var(--bg-elevated);
  border: 1px solid var(--border);
  color: var(--accent);
  text-decoration: none;
}
.json-link:hover { background: var(--accent-soft); border-color: var(--accent); }

.anchor-body {
  display: none;
  border-top: 1px solid var(--border);
  padding: 14px 16px 16px;
  background: var(--bg);
}
.anchor-card.open .anchor-body { display: block; }

.tabs {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin-bottom: 12px;
  border-bottom: 1px solid var(--border);
  padding-bottom: 10px;
}
.tab-btn {
  background: var(--bg-card);
  color: var(--text-muted);
  border: 1px solid var(--border);
  border-radius: 7px;
  padding: 6px 12px;
  font-size: 12.5px;
  font-weight: 600;
  cursor: pointer;
}
.tab-btn.active {
  background: var(--accent-soft);
  color: var(--accent);
  border-color: var(--accent);
}

.dup-group {
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: 9px;
  margin-bottom: 12px;
  overflow: hidden;
}
.dup-group-hdr {
  display: flex;
  flex-wrap: wrap;
  justify-content: space-between;
  align-items: center;
  gap: 8px;
  padding: 10px 14px;
  background: var(--bg-elevated);
  border-bottom: 1px solid var(--border);
}
.dup-group-title {
  font-weight: 700;
  font-family: var(--mono);
  font-size: 13px;
  word-break: break-all;
}

.data-table-wrap { overflow-x: auto; }
table.data-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 12.5px;
}
table.data-table th {
  text-align: left;
  padding: 8px 10px;
  background: var(--bg-elevated);
  color: var(--text-muted);
  font-size: 11px;
  text-transform: uppercase;
  letter-spacing: 0.03em;
  border-bottom: 1px solid var(--border);
  white-space: nowrap;
}
table.data-table td {
  padding: 9px 10px;
  border-bottom: 1px solid var(--border);
  vertical-align: top;
}
table.data-table tr:last-child td { border-bottom: none; }
tr.row-winner { background: rgba(52, 211, 153, 0.06); }
tr.row-loser { background: rgba(248, 113, 113, 0.04); }

.score-bar-wrap {
  width: 120px;
  height: 8px;
  background: var(--bg-elevated);
  border-radius: 99px;
  overflow: hidden;
  margin-top: 4px;
  border: 1px solid var(--border);
}
.score-bar-fill { height: 100%; border-radius: 99px; }
.fill-win { background: linear-gradient(90deg, var(--green), var(--emerald)); }
.fill-lose { background: var(--red); opacity: 0.75; }

.mono-path { font-family: var(--mono); font-size: 12px; word-break: break-all; }
.dim-sub { font-size: 11px; color: var(--text-muted); font-family: var(--mono); margin-top: 2px; }
.origin-pill {
  display: inline-block;
  font-size: 10px;
  font-weight: 700;
  padding: 1px 6px;
  border-radius: 4px;
  margin-right: 5px;
  font-family: var(--mono);
}
.orig-misc { background: var(--accent-soft); color: var(--accent); }
.orig-anchor { background: var(--purple-soft); color: var(--purple); }

.drawer {
  margin-top: 24px;
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: 10px;
  overflow: hidden;
}
.drawer-hdr {
  padding: 12px 16px;
  background: var(--bg-elevated);
  cursor: pointer;
  display: flex;
  justify-content: space-between;
  align-items: center;
  font-weight: 700;
}
.drawer-body { display: none; padding: 14px 16px; border-top: 1px solid var(--border); }
.drawer.open .drawer-body { display: block; }
.unmatched-pre {
  max-height: 320px;
  overflow: auto;
  background: var(--bg);
  border: 1px solid var(--border);
  border-radius: 7px;
  padding: 10px 12px;
  font-family: var(--mono);
  font-size: 12px;
  margin: 8px 0 0;
  white-space: pre-wrap;
  word-break: break-all;
}
.empty-state {
  text-align: center;
  padding: 36px 20px;
  color: var(--text-muted);
  background: var(--bg-card);
  border: 1px dashed var(--border);
  border-radius: 10px;
}
</style>
</head>
<body>
<div class="container">
  <div class="topbar">
    <div class="title-group">
      <h1>
        <span>Rusty Intelli Scanner — Audit &amp; Dedup Drill-Down</span>
        <span id="run-status-pill" class="status-pill"></span>
      </h1>
      <div id="run-subtitle" class="subtitle"></div>
    </div>
    <div style="display:flex; gap:8px; align-items:center; flex-wrap:wrap;">
      <a class="btn" id="btn-all-runs" href="/" style="display:none; text-decoration:none;">← All Runs</a>
      <button class="btn" id="btn-live-poll" type="button" style="display:none;"></button>
      <button class="btn" id="btn-expand-all" type="button">Expand All</button>
      <button class="btn" id="btn-collapse-all" type="button">Collapse All</button>
      <button class="btn" id="btn-theme" type="button">Light / Dark</button>
    </div>
  </div>

  <div id="kpi-strip" class="kpi-grid"></div>

  <div class="toolbar">
    <input id="search-input" class="search-box" type="search" placeholder="Search anchors, video filenames, aliases, codecs (H265, AV1, WMV), tiers..." />
    <div class="filter-pills" id="filter-pills"></div>
    <select id="sort-select" class="select-box" aria-label="Sort anchors">
      <option value="reclaimed_desc">Sort: Space Reclaimed (High → Low)</option>
      <option value="upgrades_desc">Sort: Anchor Upgrades ★ (High → Low)</option>
      <option value="dups_desc">Sort: Duplicate Groups (High → Low)</option>
      <option value="moved_desc">Sort: Videos Moved (High → Low)</option>
      <option value="seq_asc">Sort: Execution Order (#1 → #N)</option>
      <option value="name_asc">Sort: Anchor Name (A → Z)</option>
    </select>
  </div>

  <div class="results-meta">
    <span id="results-count"></span>
    <span style="font-family:var(--mono); font-size:11.5px;">Click any Anchor card to drill into Duplicate Score Breakdowns, Moved Files &amp; Symlinks</span>
  </div>

  <div id="anchors-list"></div>

  <div id="unmatched-drawer" class="drawer" style="display:none;">
    <div class="drawer-hdr" id="unmatched-hdr">
      <span id="unmatched-title"></span>
      <button class="btn" id="btn-copy-unmatched" type="button">Copy Paths</button>
    </div>
    <div class="drawer-body">
      <input id="unmatched-search" class="search-box" style="width:100%;" type="search" placeholder="Filter unmatched file paths..." />
      <pre id="unmatched-pre" class="unmatched-pre"></pre>
    </div>
  </div>

  <div id="quarantined-drawer" class="drawer" style="display:none;">
    <div class="drawer-hdr" id="quarantined-hdr">
      <span id="quarantined-title"></span>
    </div>
    <div class="drawer-body" id="quarantined-body"></div>
  </div>
</div>

<script id="audit-manifest" type="application/json">"#;

const HTML_TEMPLATE_SCRIPT_AND_END: &str = r#"
(function() {
  const rawEl = document.getElementById('audit-manifest');
  let MANIFEST = JSON.parse(rawEl ? rawEl.textContent : '{}');
  const IS_HTTP = /^https?:$/i.test(window.location.protocol);
  const state = {
    filter: 'all',
    search: '',
    sort: 'reclaimed_desc',
    openSeqs: new Set(),
    activeTabBySeq: new Map(),
    livePollEnabled: true,
    pollTimer: null
  };

  function esc(val) {
    if (val === null || val === undefined) return '';
    return String(val)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#39;');
  }

  function fmtBytes(b) {
    const n = Number(b || 0);
    if (n === 0) return '0 B';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    let idx = 0;
    let v = n;
    while (v >= 1024 && idx < units.length - 1) { v /= 1024; idx++; }
    return (idx >= 2 ? v.toFixed(2) : v.toFixed(0)) + ' ' + units[idx];
  }

  function fmtBitrate(bps) {
    const n = Number(bps || 0);
    if (!n) return '—';
    if (n >= 1000000) return (n / 1000000).toFixed(2) + ' Mbps';
    return Math.round(n / 1000) + ' kbps';
  }

  function fmtRuntime(secs) {
    const s = Math.round(Number(secs || 0));
    if (!s) return '—';
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    const r = s % 60;
    if (h > 0) return h + 'h ' + String(m).padStart(2,'0') + 'm ' + String(r).padStart(2,'0') + 's';
    if (m > 0) return m + 'm ' + String(r).padStart(2,'0') + 's (' + s + 's)';
    return s + 's';
  }

  function fmtElapsedMs(ms) {
    const n = Number(ms || 0);
    if (n < 1000) return n + ' ms';
    const s = n / 1000;
    if (s < 60) return s.toFixed(1) + 's';
    return Math.floor(s / 60) + 'm ' + Math.round(s % 60) + 's';
  }

  function outcomeBadge(outcome) {
    switch (outcome) {
      case 'ANCHOR_UPGRADED':
        return '<span class="badge b-upgrade">★ ANCHOR UPGRADED (Misc Beat Anchor)</span>';
      case 'EXISTING_ANCHOR_KEPT':
        return '<span class="badge b-kept">✓ EXISTING ANCHOR KEPT (Misc Dup Deleted)</span>';
      case 'MISC_BATCH_DEDUPED':
        return '<span class="badge b-dup">⇄ MISC BATCH DEDUPED</span>';
      default:
        return '<span class="badge b-muted">⚙ INTRA-ANCHOR CLEANUP</span>';
    }
  }

  function normalizeManifestBatches() {
    (MANIFEST.anchor_batches || []).forEach((b, idx) => {
      b._exec_order = idx + 1;
    });
  }
  normalizeManifestBatches();

  function renderHeader() {
    const pill = document.getElementById('run-status-pill');
    const st = MANIFEST.status || 'COMPLETED';
    if (st === 'COMPLETED') {
      pill.textContent = 'COMPLETED';
      pill.className = 'status-pill status-completed';
    } else if (st === 'IN_PROGRESS') {
      pill.textContent = 'LIVE / IN PROGRESS';
      pill.className = 'status-pill status-progress';
    } else {
      pill.textContent = 'STOPPED';
      pill.className = 'status-pill status-stopped';
    }

    let localStart = MANIFEST.started_at_utc || '';
    if (MANIFEST.started_at_unix_ms) {
      try { localStart = new Date(MANIFEST.started_at_unix_ms).toLocaleString(); } catch (_) {}
    }
    const elapsed = Number(MANIFEST.elapsed_secs || 0).toFixed(1) + 's';
    const sub = document.getElementById('run-subtitle');
    sub.textContent =
      'Run: ' + (MANIFEST.run_id || '-') +
      '  •  Mode: ' + (MANIFEST.mode || '-') +
      '  •  Started: ' + localStart +
      '  •  Elapsed: ' + elapsed +
      '  •  Passes: ' + ((MANIFEST.totals && MANIFEST.totals.passes_executed) || 0);
  }

  function renderKpis() {
    const t = MANIFEST.totals || {};
    const totalMovedCount = (t.direct_moves || 0) + (t.phase2_nondup_moves || 0);
    const cards = [
      {
        id: 'all',
        label: 'Anchor Batches',
        val: String(t.anchor_batches || 0),
        cls: '',
        sub: (t.misc_files_evaluated || 0) + ' matched videos • ' + (t.dedup_runs || 0) + ' dedup jobs'
      },
      {
        id: 'moved',
        label: 'New Videos Moved',
        val: String(totalMovedCount),
        cls: 'val-accent',
        sub: fmtBytes(t.bytes_moved || 0) + ' consolidated (' + (t.direct_moves || 0) + ' direct)'
      },
      {
        id: 'dups',
        label: 'Duplicate Groups',
        val: String(t.duplicate_groups || 0),
        cls: 'val-purple',
        sub: (t.duplicates_deleted || 0) + ' inferior copies deleted (' + (t.clean_runs || 0) + ' clean jobs)'
      },
      {
        id: 'upgraded',
        label: 'Anchor Upgrades ★',
        val: String(t.anchor_upgrades || 0),
        cls: 'val-emerald',
        sub: (t.existing_anchor_kept || 0) + ' existing anchor winners kept'
      },
      {
        id: 'reclaimed',
        label: 'Disk Space Reclaimed',
        val: fmtBytes(t.bytes_reclaimed || 0),
        cls: 'val-amber',
        sub: 'From deleted lower-scoring duplicates'
      },
      {
        id: 'symlinks',
        label: 'Rank #2–#4 Symlinks',
        val: String(t.symlinks_created || 0),
        cls: '',
        sub: (t.symlinks_skipped_existing || 0) + ' skipped (already linked)'
      }
    ];

    const strip = document.getElementById('kpi-strip');
    strip.innerHTML = cards.map(c => {
      const sel = (state.filter === c.id || (c.id === 'reclaimed' && state.filter === 'dups')) ? ' selected' : '';
      return '<div class="kpi-card' + sel + '" data-kpi="' + c.id + '">' +
        '<div class="kpi-label">' + esc(c.label) + '</div>' +
        '<div class="kpi-value ' + c.cls + '">' + esc(c.val) + '</div>' +
        '<div class="kpi-sub">' + esc(c.sub) + '</div>' +
      '</div>';
    }).join('');

    strip.querySelectorAll('.kpi-card').forEach(el => {
      el.addEventListener('click', () => {
        const k = el.getAttribute('data-kpi');
        const targetFilter = (k === 'reclaimed') ? 'dups' : k;
        state.filter = (state.filter === targetFilter) ? 'all' : targetFilter;
        renderFilterPills();
        renderKpis();
        renderAnchors();
      });
    });
  }

  function renderFilterPills() {
    const defs = [
      ['all', 'All'],
      ['dups', 'Has Duplicates'],
      ['upgraded', 'Anchor Upgraded ★'],
      ['kept', 'Anchor Kept'],
      ['moved', 'New Moves'],
      ['symlinks', 'Symlinks'],
      ['errors', 'Errors']
    ];
    const box = document.getElementById('filter-pills');
    box.innerHTML = defs.map(([id, label]) =>
      '<button type="button" class="btn' + (state.filter === id ? ' active' : '') + '" data-filter="' + id + '">' + esc(label) + '</button>'
    ).join('');
    box.querySelectorAll('button').forEach(btn => {
      btn.addEventListener('click', () => {
        state.filter = btn.getAttribute('data-filter');
        renderFilterPills();
        renderKpis();
        renderAnchors();
      });
    });
  }

  function batchMatchesSearch(b, q) {
    if (!q) return true;
    const hay = [
      b.anchor_name,
      b.anchor_path,
      b.destination_path
    ];
    (b.direct_moves || []).forEach(m => hay.push(m.source_path, m.dest_path, m.matched_alias, m.match_tier));
    (b.symlinks_created || []).forEach(s => hay.push(s.link_path, s.target_winner_path, s.secondary_anchor_path, s.matched_alias));
    (b.duplicate_groups || []).forEach(g => {
      hay.push(g.dup_group_name, g.outcome, g.retained_winner);
      (g.members || []).forEach(m => {
        hay.push(m.file_path, m.decision, m.matched_alias || '', m.match_tier || '');
        if (m.score) hay.push(m.score.video_codec_raw, m.score.audio_codec_raw, m.score.height_raw + 'p');
      });
    });
    const joined = hay.join(' ').toLowerCase();
    return q.split(/\s+/).every(tok => !tok || joined.indexOf(tok) !== -1);
  }

  function batchMatchesFilter(b, f) {
    switch (f) {
      case 'dups': return (b.duplicate_groups || []).length > 0;
      case 'upgraded': return (b.anchor_upgrades_count || 0) > 0;
      case 'kept': return (b.duplicate_groups || []).some(g => g.outcome === 'EXISTING_ANCHOR_KEPT');
      case 'moved': return (b.direct_moves || []).length > 0;
      case 'symlinks': return (b.symlinks_created || []).length > 0;
      case 'errors': return (b.errors || []).length > 0;
      default: return true;
    }
  }

  function batchOrder(b) {
    return b._exec_order || b.seq_id || 0;
  }

  function sortBatches(list) {
    const mode = state.sort;
    return list.slice().sort((a, b) => {
      const oa = batchOrder(a);
      const ob = batchOrder(b);
      if (mode === 'reclaimed_desc') {
        return (b.bytes_reclaimed - a.bytes_reclaimed) || ((b.duplicate_groups || []).length - (a.duplicate_groups || []).length) || (oa - ob);
      }
      if (mode === 'upgrades_desc') {
        return ((b.anchor_upgrades_count || 0) - (a.anchor_upgrades_count || 0)) || (b.bytes_reclaimed - a.bytes_reclaimed) || (oa - ob);
      }
      if (mode === 'dups_desc') {
        return ((b.duplicate_groups || []).length - (a.duplicate_groups || []).length) || (b.bytes_reclaimed - a.bytes_reclaimed) || (oa - ob);
      }
      if (mode === 'moved_desc') {
        return ((b.direct_moves || []).length - (a.direct_moves || []).length) || (b.bytes_moved - a.bytes_moved) || (oa - ob);
      }
      if (mode === 'name_asc') {
        return String(a.anchor_name || '').localeCompare(String(b.anchor_name || '')) || (oa - ob);
      }
      return oa - ob;
    });
  }

  function renderDupGroupsHtml(groups) {
    if (!groups || !groups.length) {
      return '<div class="empty-state">0 duplicate groups in this anchor batch — all matched files were unique.</div>';
    }
    return groups.map((g, gi) => {
      const maxS = Number(g.max_possible_score || 9.0);
      const tieHtml = g.tie_broken_by_shortest_path
        ? '<span class="badge b-muted" title="Scores tied within 1e-9; shortest file path won">Tie-Breaker: Shortest Path</span>'
        : '';
      const rowsHtml = (g.members || []).map(m => {
        const sc = m.score || {};
        const tot = Number(sc.total_normalized_score || 0);
        const pct = Math.max(2, Math.min(100, (tot / maxS) * 100));
        const res = (sc.width_raw && sc.height_raw) ? (sc.width_raw + '×' + sc.height_raw) : '—';
        const matchMeta = m.matched_alias
          ? '<div class="dim-sub">Matched: <b>' + esc(m.matched_alias) + '</b> • ' + esc(m.match_tier || '') + ' (' + Number(m.scanner_confidence || 0).toFixed(4) + ')</div>'
          : '';
        const movedSub = m.moved_to
          ? '<div class="dim-sub" style="color:var(--emerald);">→ Moved to: ' + esc(m.moved_to) + '</div>'
          : '';
        return '<tr class="' + (m.is_winner ? 'row-winner' : 'row-loser') + '">' +
          '<td>' +
            '<div>' +
              '<span class="origin-pill ' + (m.origin === 'ANCHOR' ? 'orig-anchor' : 'orig-misc') + '">' + esc(m.origin) + '</span>' +
              '<b>' + (m.is_winner ? '★ WINNER' : '✖ DELETED') + '</b>' +
            '</div>' +
            '<div class="mono-path" style="margin-top:4px;">' + esc(m.file_path) + '</div>' +
            matchMeta + movedSub +
          '</td>' +
          '<td style="white-space:nowrap;">' +
            '<div style="font-family:var(--mono); font-weight:700;">' + tot.toFixed(4) + ' <span style="color:var(--text-muted); font-weight:400;">/ ' + maxS.toFixed(1) + '</span></div>' +
            '<div class="score-bar-wrap"><div class="score-bar-fill ' + (m.is_winner ? 'fill-win' : 'fill-lose') + '" style="width:' + pct.toFixed(1) + '%"></div></div>' +
          '</td>' +
          '<td style="white-space:nowrap;"><b>' + esc(sc.video_codec_raw || '—') + '</b><div class="dim-sub">norm ' + Number(sc.video_codec_normalized || 0).toFixed(3) + '</div></td>' +
          '<td style="white-space:nowrap;">' + esc(res) + '<div class="dim-sub">H:' + Number(sc.height_normalized || 0).toFixed(2) + ' W:' + Number(sc.width_normalized || 0).toFixed(2) + '</div></td>' +
          '<td style="white-space:nowrap;">' + esc(fmtBitrate(sc.video_bitrate_raw)) + '<div class="dim-sub">2× = ' + Number(sc.video_bitrate_normalized || 0).toFixed(3) + '</div></td>' +
          '<td style="white-space:nowrap;">' + esc(sc.audio_codec_raw || '—') + ' • ' + esc(fmtBitrate(sc.audio_bitrate_raw)) + '<div class="dim-sub">C:' + Number(sc.audio_codec_normalized || 0).toFixed(2) + ' B:' + Number(sc.audio_bitrate_normalized || 0).toFixed(2) + '</div></td>' +
          '<td style="white-space:nowrap;">' + esc(fmtRuntime(sc.runtime_raw)) + '<div class="dim-sub">norm ' + Number(sc.runtime_normalized || 0).toFixed(2) + '</div></td>' +
          '<td style="white-space:nowrap;">' + esc(fmtBytes(sc.size_raw)) + '<div class="dim-sub">norm ' + Number(sc.size_normalized || 0).toFixed(2) + '</div></td>' +
          '<td><span class="mono-path">' + esc(m.decision) + '</span><div class="dim-sub">' + esc(m.action_taken) + '</div></td>' +
        '</tr>';
      }).join('');

      let pairsHtml = '';
      if (g.pair_evaluations && g.pair_evaluations.length) {
        pairsHtml = '<div style="padding:8px 12px; border-top:1px solid var(--border); font-family:var(--mono); font-size:11.5px; color:var(--text-muted);">' +
          'dHash Pair Telemetry: ' + g.pair_evaluations.map(p =>
            esc(p.detection_method) + ' • conf ' + (Number(p.confidence || 0) * 100).toFixed(1) + '% (' +
            p.matched_frames + '/' + p.compared_runtime_secs + 's, offset ' + p.best_offset_secs + 's)'
          ).join(' | ') + '</div>';
      }

      return '<div class="dup-group">' +
        '<div class="dup-group-hdr">' +
          '<div style="display:flex; flex-wrap:wrap; align-items:center; gap:8px;">' +
            '<span class="seq-badge">Group #' + (gi + 1) + ' (' + g.group_size + ' files)</span>' +
            outcomeBadge(g.outcome) +
            tieHtml +
            '<span class="dup-group-title">' + esc(g.dup_group_name) + '</span>' +
          '</div>' +
          '<div style="display:flex; gap:8px; align-items:center;">' +
            '<span class="badge b-saved">Reclaimed: ' + esc(fmtBytes(g.bytes_reclaimed)) + '</span>' +
          '</div>' +
        '</div>' +
        '<div class="data-table-wrap">' +
          '<table class="data-table">' +
            '<thead><tr>' +
              '<th>File &amp; Origin</th>' +
              '<th>Total Score</th>' +
              '<th>V-Codec</th>' +
              '<th>Resolution</th>' +
              '<th>V-Bitrate (2×)</th>' +
              '<th>Audio</th>' +
              '<th>Runtime</th>' +
              '<th>Size (Lower=Better)</th>' +
              '<th>Decision</th>' +
            '</tr></thead>' +
            '<tbody>' + rowsHtml + '</tbody>' +
          '</table>' +
        '</div>' +
        pairsHtml +
      '</div>';
    }).join('');
  }

  function renderMovesHtml(moves) {
    if (!moves || !moves.length) {
      return '<div class="empty-state">No direct non-duplicate file moves in this batch.</div>';
    }
    return '<div class="data-table-wrap"><table class="data-table">' +
      '<thead><tr><th>#</th><th>Source Misc File</th><th>Moved To Destination</th><th>Size</th><th>Matched Alias &amp; Tier</th><th>Mode</th></tr></thead>' +
      '<tbody>' + moves.map((m, idx) =>
        '<tr>' +
          '<td style="font-family:var(--mono);">' + (idx + 1) + '</td>' +
          '<td class="mono-path">' + esc(m.source_path) + '</td>' +
          '<td class="mono-path" style="color:var(--emerald);">' + esc(m.dest_path) + '</td>' +
          '<td style="white-space:nowrap; font-family:var(--mono);">' + esc(fmtBytes(m.size_bytes)) + '</td>' +
          '<td style="white-space:nowrap;"><b>' + esc(m.matched_alias) + '</b><div class="dim-sub">' + esc(m.match_tier) + ' • ' + Number(m.confidence_score || 0).toFixed(4) + '</div></td>' +
          '<td style="white-space:nowrap;"><span class="badge b-move">' + esc(m.move_kind === 'CLEAN_PHASE2_NON_DUP' ? 'Clean Phase-2' : 'Direct Move') + '</span></td>' +
        '</tr>'
      ).join('') + '</tbody></table></div>';
  }

  function renderSymlinksHtml(links, skippedCount) {
    if (!links || !links.length) {
      return '<div class="empty-state">0 secondary symlinks created in this batch' +
        (skippedCount ? ' (' + skippedCount + ' already existed and were skipped).' : '.') + '</div>';
    }
    return '<div class="data-table-wrap"><table class="data-table">' +
      '<thead><tr><th>Rank</th><th>Secondary Anchor</th><th>Symlink Created</th><th>Points To Rank #1 Winner</th><th>Alias &amp; Score</th></tr></thead>' +
      '<tbody>' + links.map(s =>
        '<tr>' +
          '<td><span class="seq-badge">Rank #' + s.rank + '</span></td>' +
          '<td class="mono-path">' + esc(s.secondary_anchor_path) + '</td>' +
          '<td class="mono-path" style="color:var(--accent);">' + esc(s.link_path) + '</td>' +
          '<td class="mono-path">' + esc(s.target_winner_path) + '</td>' +
          '<td style="white-space:nowrap;"><b>' + esc(s.matched_alias) + '</b><div class="dim-sub">' + Number(s.confidence_score || 0).toFixed(4) + '</div></td>' +
        '</tr>'
      ).join('') + '</tbody></table></div>';
  }

  function renderAnchorBody(b) {
    const ord = batchOrder(b);
    const dupCount = (b.duplicate_groups || []).length;
    const moveCount = (b.direct_moves || []).length;
    const symCount = (b.symlinks_created || []).length;
    const defaultTab = dupCount > 0 ? 'dups' : (moveCount > 0 ? 'moves' : 'symlinks');
    const activeTab = state.activeTabBySeq.get(ord) || defaultTab;

    let errBanner = '';
    if (b.errors && b.errors.length) {
      errBanner = '<div style="margin-bottom:10px; padding:10px 12px; border-radius:7px; background:var(--red-soft); color:var(--red); font-family:var(--mono); font-size:12px;">' +
        b.errors.map(esc).join('<br>') + '</div>';
    }

    let contentHtml = '';
    if (activeTab === 'dups') contentHtml = renderDupGroupsHtml(b.duplicate_groups);
    else if (activeTab === 'moves') contentHtml = renderMovesHtml(b.direct_moves);
    else contentHtml = renderSymlinksHtml(b.symlinks_created, b.symlinks_skipped_existing);

    return errBanner +
      '<div class="tabs">' +
        '<button type="button" class="tab-btn' + (activeTab === 'dups' ? ' active' : '') + '" data-seq="' + ord + '" data-tab="dups">Duplicate Groups (' + dupCount + ')</button>' +
        '<button type="button" class="tab-btn' + (activeTab === 'moves' ? ' active' : '') + '" data-seq="' + ord + '" data-tab="moves">New Videos Moved (' + moveCount + ')</button>' +
        '<button type="button" class="tab-btn' + (activeTab === 'symlinks' ? ' active' : '') + '" data-seq="' + ord + '" data-tab="symlinks">Rank #2–#4 Symlinks (' + symCount + ')</button>' +
      '</div>' +
      '<div class="tab-panel">' + contentHtml + '</div>';
  }

  function renderAnchors() {
    const all = MANIFEST.anchor_batches || [];
    const q = state.search.trim().toLowerCase();
    const filtered = sortBatches(all.filter(b => batchMatchesFilter(b, state.filter) && batchMatchesSearch(b, q)));

    document.getElementById('results-count').textContent =
      'Showing ' + filtered.length + ' of ' + all.length + ' anchor execution batch(es)';

    const container = document.getElementById('anchors-list');
    if (!filtered.length) {
      container.innerHTML = '<div class="empty-state">No anchor batches match the active search/filter criteria.</div>';
      return;
    }

    container.innerHTML = filtered.map(b => {
      const ord = batchOrder(b);
      const isOpen = state.openSeqs.has(ord) || (q.length > 0 && filtered.length <= 12);
      const dupCount = (b.duplicate_groups || []).length;
      const moveCount = (b.direct_moves || []).length;
      const symCount = (b.symlinks_created || []).length;
      const upgCount = b.anchor_upgrades_count || 0;
      const customDest = (b.destination_path && b.destination_path !== b.anchor_path)
        ? '  →  dest: ' + b.destination_path
        : '';
      const chunkSuffix = b.chunk_idx > 0 ? (' • Part ' + (b.chunk_idx + 1)) : '';

      const badges = [];
      if (moveCount > 0) badges.push('<span class="badge b-move">+' + moveCount + ' Moved (' + esc(fmtBytes(b.bytes_moved)) + ')</span>');
      if (dupCount > 0) badges.push('<span class="badge b-dup">' + dupCount + ' Dup Group' + (dupCount === 1 ? '' : 's') + '</span>');
      if (upgCount > 0) badges.push('<span class="badge b-upgrade">★ ' + upgCount + ' Upgraded</span>');
      if (b.bytes_reclaimed > 0) badges.push('<span class="badge b-saved">' + esc(fmtBytes(b.bytes_reclaimed)) + ' Saved</span>');
      if (symCount > 0) badges.push('<span class="badge b-muted">' + symCount + ' Symlink' + (symCount === 1 ? '' : 's') + '</span>');
      if (b.errors && b.errors.length) badges.push('<span class="badge b-err">' + b.errors.length + ' Error</span>');
      badges.push('<span class="badge b-muted">' + esc(fmtElapsedMs(b.elapsed_ms)) + '</span>');

      const dedupHref = REL_LINK_PREFIX + b.dedup_report_rel_path;
      let linksHtml = '<a class="json-link" href="' + esc(dedupHref) + '" target="_blank" rel="noopener" onclick="event.stopPropagation();">dups.json</a>';
      if (b.clean_report_rel_path) {
        const cleanHref = REL_LINK_PREFIX + b.clean_report_rel_path;
        linksHtml += '<a class="json-link" href="' + esc(cleanHref) + '" target="_blank" rel="noopener" onclick="event.stopPropagation();">clean.json</a>';
      }
      if (b.pairs_log_rel_path) {
        const pairsHref = REL_LINK_PREFIX + b.pairs_log_rel_path;
        linksHtml += '<a class="json-link" href="' + esc(pairsHref) + '" target="_blank" rel="noopener" onclick="event.stopPropagation();">pairs.json</a>';
      }

      return '<div class="anchor-card' + (isOpen ? ' open' : '') + '" data-card-seq="' + ord + '">' +
        '<div class="anchor-header" data-hdr-seq="' + ord + '">' +
          '<div class="anchor-left">' +
            '<span class="chevron">▶</span>' +
            '<span class="seq-badge">#' + ord + ' • P' + b.pass_num + chunkSuffix + '</span>' +
            '<div class="anchor-title-wrap">' +
              '<div class="anchor-name">' + esc(b.anchor_name) + '</div>' +
              '<div class="anchor-path">' + esc(b.anchor_path + customDest) + '</div>' +
            '</div>' +
          '</div>' +
          '<div class="anchor-right">' +
            badges.join('') +
            linksHtml +
          '</div>' +
        '</div>' +
        '<div class="anchor-body">' + (isOpen ? renderAnchorBody(b) : '') + '</div>' +
      '</div>';
    }).join('');

    bindAnchorEvents(filtered);
  }

  function bindAnchorEvents(batches) {
    const bySeq = new Map(batches.map(b => [batchOrder(b), b]));
    document.querySelectorAll('[data-hdr-seq]').forEach(hdr => {
      hdr.addEventListener('click', () => {
        const seq = Number(hdr.getAttribute('data-hdr-seq'));
        const card = document.querySelector('[data-card-seq="' + seq + '"]');
        const body = card.querySelector('.anchor-body');
        if (card.classList.contains('open')) {
          card.classList.remove('open');
          state.openSeqs.delete(seq);
        } else {
          card.classList.add('open');
          state.openSeqs.add(seq);
          if (!body.innerHTML) {
            body.innerHTML = renderAnchorBody(bySeq.get(seq));
            bindTabButtons(bySeq);
          }
        }
      });
    });
    bindTabButtons(bySeq);
  }

  function bindTabButtons(bySeq) {
    document.querySelectorAll('.tab-btn').forEach(btn => {
      if (btn._bound) return;
      btn._bound = true;
      btn.addEventListener('click', (e) => {
        e.stopPropagation();
        const seq = Number(btn.getAttribute('data-seq'));
        const tab = btn.getAttribute('data-tab');
        state.activeTabBySeq.set(seq, tab);
        const card = document.querySelector('[data-card-seq="' + seq + '"]');
        if (card && bySeq.has(seq)) {
          card.querySelector('.anchor-body').innerHTML = renderAnchorBody(bySeq.get(seq));
          bindTabButtons(bySeq);
        }
      });
    });
  }

  function renderDrawers() {
    const unmatched = MANIFEST.unmatched_files || [];
    if (unmatched.length > 0) {
      const dr = document.getElementById('unmatched-drawer');
      dr.style.display = 'block';
      document.getElementById('unmatched-title').textContent =
        'Unmatched Miscellaneous Video Files (' + unmatched.length + ')';
      const pre = document.getElementById('unmatched-pre');
      pre.textContent = unmatched.join('\n');

      document.getElementById('unmatched-hdr').addEventListener('click', (e) => {
        if (e.target.id === 'btn-copy-unmatched') return;
        dr.classList.toggle('open');
      });
      document.getElementById('unmatched-search').addEventListener('input', (e) => {
        const q = e.target.value.trim().toLowerCase();
        pre.textContent = (q ? unmatched.filter(p => p.toLowerCase().indexOf(q) !== -1) : unmatched).join('\n');
      });
      document.getElementById('btn-copy-unmatched').addEventListener('click', (e) => {
        e.stopPropagation();
        if (navigator.clipboard) {
          navigator.clipboard.writeText(pre.textContent || '');
          e.target.textContent = 'Copied!';
          setTimeout(() => { e.target.textContent = 'Copy Paths'; }, 1500);
        }
      });
    }

    const quar = MANIFEST.quarantined_files || [];
    if (quar.length > 0) {
      const qdr = document.getElementById('quarantined-drawer');
      qdr.style.display = 'block';
      qdr.classList.add('open');
      document.getElementById('quarantined-title').textContent =
        'Quarantined / Skipped Files (' + quar.length + ')';
      document.getElementById('quarantined-hdr').addEventListener('click', () => qdr.classList.toggle('open'));
      document.getElementById('quarantined-body').innerHTML =
        '<div class="data-table-wrap"><table class="data-table"><thead><tr><th>File Path</th><th>Anchor</th><th>Reason</th></tr></thead><tbody>' +
        quar.map(q => '<tr><td class="mono-path">' + esc(q.file_path) + '</td><td class="mono-path">' + esc(q.anchor_path) + '</td><td style="color:var(--red);">' + esc(q.reason) + '</td></tr>').join('') +
        '</tbody></table></div>';
    }
  }

  document.getElementById('search-input').addEventListener('input', (e) => {
    state.search = e.target.value || '';
    renderAnchors();
  });
  document.getElementById('sort-select').addEventListener('change', (e) => {
    state.sort = e.target.value;
    renderAnchors();
  });
  document.getElementById('btn-expand-all').addEventListener('click', () => {
    (MANIFEST.anchor_batches || []).forEach(b => state.openSeqs.add(batchOrder(b)));
    renderAnchors();
  });
  document.getElementById('btn-collapse-all').addEventListener('click', () => {
    state.openSeqs.clear();
    renderAnchors();
  });
  document.getElementById('btn-theme').addEventListener('click', () => {
    const html = document.documentElement;
    html.setAttribute('data-theme', html.getAttribute('data-theme') === 'light' ? 'dark' : 'light');
  });

  // Auto-open first anchor with duplicates (or first anchor if <= 3 total) for instant insight
  const batches = MANIFEST.anchor_batches || [];
  const firstWithDup = batches.find(b => (b.duplicate_groups || []).length > 0);
  if (firstWithDup) state.openSeqs.add(batchOrder(firstWithDup));
  else if (batches.length > 0 && batches.length <= 3) state.openSeqs.add(batchOrder(batches[0]));

  renderHeader();
  renderFilterPills();
  renderKpis();
  renderAnchors();
  renderDrawers();

  // Enable Hub navigation (over HTTP) & automatic periodic refresh while run is IN_PROGRESS
  if (IS_HTTP) {
    const allRunsBtn = document.getElementById('btn-all-runs');
    if (allRunsBtn) allRunsBtn.style.display = 'inline-block';
  }

  const pollSecs = Number(MANIFEST.live_refresh_secs || 0);
  const pollBtn = document.getElementById('btn-live-poll');

  function isManifestActive() {
    return MANIFEST.status === 'IN_PROGRESS';
  }

  function updatePollButton() {
    if (!pollBtn || pollSecs <= 0) return;
    if (!isManifestActive() && !state.forceKeepPolling) {
      pollBtn.style.display = 'inline-block';
      pollBtn.textContent = MANIFEST.status === 'COMPLETED'
        ? '⟳ Auto-Refresh: Done (Click to Watch)'
        : '⟳ Auto-Refresh: Stopped (Click to Watch)';
      pollBtn.classList.remove('active');
      return;
    }
    pollBtn.style.display = 'inline-block';
    pollBtn.textContent = state.livePollEnabled
      ? ('⟳ Auto-Refresh: ON (' + pollSecs + 's)')
      : '⏸ Auto-Refresh: Paused';
    pollBtn.classList.toggle('active', state.livePollEnabled);
  }

  async function refreshReportTick() {
    if (!state.livePollEnabled) return;
    if (!isManifestActive() && !state.forceKeepPolling) return;

    if (IS_HTTP) {
      try {
        const res = await fetch(REL_LINK_PREFIX + 'run_manifest.json?t=' + Date.now(), { cache: 'no-store' });
        if (res.ok) {
          const fresh = await res.json();
          if (fresh && fresh.run_id === MANIFEST.run_id) {
            MANIFEST = fresh;
            normalizeManifestBatches();
            renderHeader();
            renderFilterPills();
            renderKpis();
            renderAnchors();
            renderDrawers();
            updatePollButton();
            return;
          }
        }
      } catch (_) {}
    }
    // Fallback (file:// mode, python http.server index swap, or new run): full periodic page reload
    window.location.reload();
  }

  if (pollSecs > 0 && pollBtn) {
    updatePollButton();
    pollBtn.addEventListener('click', () => {
      if (!isManifestActive() && !state.forceKeepPolling) {
        state.forceKeepPolling = true;
        state.livePollEnabled = true;
      } else {
        state.livePollEnabled = !state.livePollEnabled;
      }
      updatePollButton();
      if (state.livePollEnabled) refreshReportTick();
    });
    state.pollTimer = setInterval(refreshReportTick, pollSecs * 1000);
  }
})();
</script>
</body>
</html>
"#;
