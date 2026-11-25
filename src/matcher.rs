use crate::config::{AnchorEntry, MatchingConfig};
use crate::nlp::{NlpAnalyzer, PosTag};
use crate::scanner::MiscVideoCandidate;
use rayon::prelude::*;
use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

/// Scoring tier / channel combination that produced an anchor match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchTier {
    /// Both enclosing directory AND video filename independently matched this anchor!
    DirAndTitle,
    /// Enclosing directory exact match (`.../Abbie Cat H265/clip.mp4`).
    ExactSubdir,
    /// Enclosing directory partial/phrase/fuzzy match (`.../Abbie Cat Pack 2025/clip.mp4`).
    DirSignal,
    /// Video filename clause exact match (`Abbie Cat - Clip.mp4`).
    ExactClause,
    /// Video filename contiguous whole-token phrase match (NLP POS-weighted).
    WholeTokenPhrase,
    /// Video filename spaceless compact prefix/suffix match (NLP POS-weighted).
    CompactAffix,
    /// Video filename fuzzy clause match (NLP POS-weighted).
    FuzzyClause,
}

impl MatchTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            MatchTier::DirAndTitle => "DIR_AND_TITLE",
            MatchTier::ExactSubdir => "EXACT_SUBDIR",
            MatchTier::DirSignal => "DIR_SIGNAL",
            MatchTier::ExactClause => "EXACT_CLAUSE",
            MatchTier::WholeTokenPhrase => "WHOLE_TOKEN_PHRASE",
            MatchTier::CompactAffix => "COMPACT_AFFIX",
            MatchTier::FuzzyClause => "FUZZY_CLAUSE",
        }
    }
}

impl fmt::Display for MatchTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Planned live-mode action assigned to each ranked anchor match for a misc video file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannedAction {
    /// Rank #1 anchor: deduplicate against `<anchor_root>` and move/retain into `<destination>`.
    PrimaryMove,
    /// Ranks #2..#(1 + max_secondary_symlinks): create softlink in `<destination>` -> Rank #1 winner.
    Symlink,
    /// Rank beyond `1 + max_secondary_symlinks`: reported in CSV, no symlink created.
    RankExceededNoOp,
}

impl PlannedAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            PlannedAction::PrimaryMove => "PRIMARY_MOVE",
            PlannedAction::Symlink => "SYMLINK",
            PlannedAction::RankExceededNoOp => "RANK_EXCEEDED_NO_OP",
        }
    }
}

impl fmt::Display for PlannedAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single ranked anchor match for a candidate misc video file.
#[derive(Debug, Clone)]
pub struct RankedAnchorMatch {
    /// 1-based rank among all anchors matched for this file (`1` = highest scoring).
    pub rank: usize,
    /// Total number of anchors matched by this file.
    pub total_matches: usize,
    /// Planned live-mode action (`PRIMARY_MOVE`, `SYMLINK`, or `RANK_EXCEEDED_NO_OP`).
    pub planned_action: PlannedAction,
    /// Index into `AnchorDirsConfig.anchors`.
    pub anchor_idx: usize,
    /// Root path of the matched anchor directory.
    pub anchor_path: PathBuf,
    /// Resolved target destination directory for this anchor (`destination` or `anchor_path`).
    pub destination_path: PathBuf,
    /// Raw name or alias that triggered the best score for this anchor.
    pub matched_alias: String,
    /// Scoring tier / channel combination that produced this match.
    pub match_tier: MatchTier,
    /// Composite confidence score in `[0.0, 1.0]` combining Directory + Title + NLP POS weighting.
    pub confidence_score: f64,
    /// Left-to-right clause/token position (used as tie-breaker #2).
    pub clause_position: usize,
    /// Alphanumeric length of the matched alias (used as tie-breaker #3).
    pub matched_alias_alnum_len: usize,
    /// Zero-based YAML declaration order of the anchor (used as tie-breaker #4).
    pub anchor_yaml_order: usize,
}

/// Complete matching result for a single miscellaneous video file.
#[derive(Debug, Clone)]
pub struct FileMatchResult {
    pub candidate: MiscVideoCandidate,
    /// Ranked matches (`rank = 1, 2, ...`), empty if unmatched.
    pub matches: Vec<RankedAnchorMatch>,
}

/// Partitioned batch outcome across all scanned miscellaneous video files.
#[derive(Debug, Clone)]
pub struct BatchMatchOutcome {
    pub matched_files: Vec<FileMatchResult>,
    pub unmatched_files: Vec<MiscVideoCandidate>,
}

#[derive(Debug, Clone)]
struct CompiledAlias {
    display_name: String,
    norm: String,
    tokens: Vec<String>,
    pos_tags: Vec<PosTag>,
    compact: String,
    alnum_len: usize,
    /// False for stylized single-letter-spaced names like `G I R L` (`["g", "i", "r", "l"]`)
    /// so they never collapse into common English words like `"girl"`.
    allow_compact_collapse: bool,
    numbers: Vec<String>,
}

#[derive(Debug, Clone)]
struct CompiledAnchor {
    anchor_idx: usize,
    entry: AnchorEntry,
    aliases: Vec<CompiledAlias>,
}

/// Precompiled non-short-circuiting matching, NLP POS-weighting, and ranking engine.
#[derive(Debug, Clone)]
pub struct AnchorMatcher {
    cfg: MatchingConfig,
    noise_set: HashSet<String>,
    nlp: NlpAnalyzer,
    anchors: Vec<CompiledAnchor>,
}

#[derive(Debug, Clone)]
struct ExtractedClause {
    position: usize,
    has_explicit_delim: bool,
    norm: String,
    tokens: Vec<String>,
    pos_tags: Vec<PosTag>,
    compact: String,
    numbers: Vec<String>,
}

#[derive(Debug, Clone)]
struct ChannelEvidence {
    tier: MatchTier,
    raw_score: f64,
    position: usize,
    alias_display: String,
    alias_alnum_len: usize,
}

/// Splits CamelCase / PascalCase words and non-alphanumeric separators into lowercase space-separated tokens,
/// optionally filtering out `noise_set`.
pub fn normalize_text_camel_aware(input: &str, noise_set: Option<&HashSet<String>>) -> String {
    let mut spaced = String::with_capacity(input.len() + 8);
    let chars: Vec<char> = input.chars().collect();
    for i in 0..chars.len() {
        let c = chars[i];
        if i > 0 {
            let prev = chars[i - 1];
            let next_is_lower = chars.get(i + 1).map(|nc| nc.is_lowercase()).unwrap_or(false);
            if (prev.is_lowercase() && c.is_uppercase())
                || (prev.is_uppercase() && c.is_uppercase() && next_is_lower)
            {
                spaced.push(' ');
            }
        }
        if c.is_alphanumeric() {
            for lc in c.to_lowercase() {
                spaced.push(lc);
            }
        } else {
            spaced.push(' ');
        }
    }

    spaced
        .split_whitespace()
        .filter(|tok| noise_set.map(|ns| !ns.contains(*tok)).unwrap_or(true))
        .collect::<Vec<_>>()
        .join(" ")
}

fn extract_numbers(tokens: &[String]) -> Vec<String> {
    let mut nums = Vec::new();
    for tok in tokens {
        if tok.chars().all(|c| c.is_ascii_digit()) {
            let trimmed = tok.trim_start_matches('0');
            nums.push(if trimmed.is_empty() {
                "0".to_string()
            } else {
                trimmed.to_string()
            });
        } else {
            for prefix in ["part", "pt", "episode", "ep", "volume", "vol", "scene", "sc"] {
                if let Some(rest) = tok.strip_prefix(prefix) {
                    if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                        let trimmed = rest.trim_start_matches('0');
                        nums.push(if trimmed.is_empty() {
                            "0".to_string()
                        } else {
                            trimmed.to_string()
                        });
                    }
                }
            }
        }
    }
    nums
}

/// Splits a raw text (either filename stem or directory segment) into ordered structural and
/// collaboration clauses and tags each token's Part-of-Speech.
fn extract_clauses_with_nlp(
    raw_text: &str,
    noise_set: &HashSet<String>,
    nlp: &NlpAnalyzer,
) -> Vec<ExtractedClause> {
    let mut normalized_delims = raw_text
        .replace("_-_", " | ")
        .replace(" - ", " | ")
        .replace("--", " | ")
        .replace('[', " | ")
        .replace(']', " | ")
        .replace('(', " | ")
        .replace(')', " | ");

    if !normalized_delims.contains('|') && normalized_delims.contains('-') {
        let parts: Vec<&str> = normalized_delims.split('-').collect();
        let mut rebuilt = String::new();
        for (idx, p) in parts.iter().enumerate() {
            if idx > 0 {
                let prev_trim = parts[idx - 1].trim().to_lowercase();
                let curr_trim = p.trim().to_lowercase();
                let is_sequel_hyphen = curr_trim.chars().all(|c| c.is_ascii_digit())
                    || prev_trim.ends_with("part")
                    || prev_trim.ends_with("pt")
                    || prev_trim.ends_with("ep")
                    || prev_trim.ends_with("vol");
                if is_sequel_hyphen {
                    rebuilt.push(' ');
                } else {
                    rebuilt.push_str(" | ");
                }
            }
            rebuilt.push_str(p);
        }
        normalized_delims = rebuilt;
    }

    let structural_pieces: Vec<&str> = normalized_delims
        .split('|')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let has_explicit_delim = structural_pieces.len() > 1;

    let mut out: Vec<ExtractedClause> = Vec::new();
    let mut seen_pos_norm: HashSet<(usize, String)> = HashSet::new();
    let mut pos_counter: usize = 0;

    let push_clause = |raw_piece: &str,
                       pos: usize,
                       explicit_delim: bool,
                       out_vec: &mut Vec<ExtractedClause>,
                       seen: &mut HashSet<(usize, String)>| {
        let stripped = normalize_text_camel_aware(raw_piece, Some(noise_set));
        let norm = if stripped.is_empty() {
            normalize_text_camel_aware(raw_piece, None)
        } else {
            stripped
        };
        if norm.is_empty() {
            return;
        }
        if seen.insert((pos, norm.clone())) {
            let tokens: Vec<String> = norm.split_whitespace().map(|s| s.to_string()).collect();
            let pos_tags = nlp.tag_tokens(&tokens, raw_piece);
            let compact = tokens.join("");
            let numbers = extract_numbers(&tokens);
            out_vec.push(ExtractedClause {
                position: pos,
                has_explicit_delim: explicit_delim,
                norm,
                tokens,
                pos_tags,
                compact,
                numbers,
            });
        }
    };

    for trimmed in structural_pieces {
        let start_pos = pos_counter;
        let collab_replaced = replace_collab_delimiters(trimmed);
        let sub_parts: Vec<&str> = collab_replaced
            .split(" || ")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();

        if sub_parts.len() > 1 {
            push_clause(trimmed, start_pos, true, &mut out, &mut seen_pos_norm);
            for sp in sub_parts {
                push_clause(sp, pos_counter, true, &mut out, &mut seen_pos_norm);
                pos_counter += 1;
            }
        } else {
            push_clause(
                trimmed,
                pos_counter,
                has_explicit_delim,
                &mut out,
                &mut seen_pos_norm,
            );
            pos_counter += 1;
        }
    }

    out
}

fn replace_collab_delimiters(clause: &str) -> String {
    let s = clause
        .replace('&', " || ")
        .replace('+', " || ")
        .replace(',', " || ");

    let padded = format!(" {} ", s);
    let mut working = padded;
    for kw in [
        " feat. ", " feat ", " ft. ", " ft ", " vs. ", " vs ", " FEAT. ", " FEAT ", " FT. ",
        " FT ", " VS. ", " VS ", " Feat. ", " Feat ", " Ft. ", " Ft ", " Vs. ", " Vs ",
    ] {
        working = working.replace(kw, " || ");
    }
    working.trim().to_string()
}

impl AnchorMatcher {
    pub fn new(anchors: &[AnchorEntry], cfg: MatchingConfig) -> Self {
        let noise_set: HashSet<String> = cfg
            .noise_tokens
            .iter()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();

        let mut all_display_names: Vec<String> = Vec::with_capacity(anchors.len() * 2);
        for entry in anchors {
            if let Some(base) = entry.anchor_path.file_name().and_then(|n| n.to_str()) {
                all_display_names.push(base.to_string());
            }
            for al in &entry.aliases {
                all_display_names.push(al.clone());
            }
        }

        let nlp = NlpAnalyzer::new(cfg.nlp.clone(), &all_display_names);

        let compiled_anchors: Vec<CompiledAnchor> = anchors
            .iter()
            .enumerate()
            .map(|(idx, entry)| {
                let mut raw_names: Vec<String> = Vec::with_capacity(entry.aliases.len() + 1);
                if let Some(base) = entry.anchor_path.file_name().and_then(|n| n.to_str()) {
                    if !base.trim().is_empty() {
                        raw_names.push(base.trim().to_string());
                    }
                }
                for al in &entry.aliases {
                    if !al.trim().is_empty() {
                        raw_names.push(al.trim().to_string());
                    }
                }

                let mut aliases: Vec<CompiledAlias> = Vec::new();
                let mut seen_norms: HashSet<String> = HashSet::new();

                for raw_name in raw_names {
                    let stripped = normalize_text_camel_aware(&raw_name, Some(&noise_set));
                    let unstripped = normalize_text_camel_aware(&raw_name, None);

                    for cand_norm in [stripped, unstripped] {
                        if cand_norm.is_empty() || !seen_norms.insert(cand_norm.clone()) {
                            continue;
                        }
                        let tokens: Vec<String> =
                            cand_norm.split_whitespace().map(|s| s.to_string()).collect();
                        let pos_tags = nlp.tag_tokens(&tokens, &raw_name);
                        let compact = tokens.join("");
                        let alnum_len = compact.chars().count();
                        if alnum_len == 0 {
                            continue;
                        }
                        let numbers = extract_numbers(&tokens);
                        let all_single_char_tokens =
                            tokens.len() >= 2 && tokens.iter().all(|t| t.chars().count() == 1);
                        aliases.push(CompiledAlias {
                            display_name: raw_name.clone(),
                            norm: cand_norm,
                            tokens,
                            pos_tags,
                            compact,
                            alnum_len,
                            allow_compact_collapse: !all_single_char_tokens,
                            numbers,
                        });
                    }
                }

                CompiledAnchor {
                    anchor_idx: idx,
                    entry: entry.clone(),
                    aliases,
                }
            })
            .collect();

        Self {
            cfg,
            noise_set,
            nlp,
            anchors: compiled_anchors,
        }
    }

    /// Matches and ranks a batch of miscellaneous video candidates in parallel using Rayon.
    pub fn match_batch(&self, candidates: &[MiscVideoCandidate]) -> BatchMatchOutcome {
        let results: Vec<FileMatchResult> = candidates
            .par_iter()
            .map(|cand| self.match_single(cand))
            .collect();

        let mut matched_files = Vec::new();
        let mut unmatched_files = Vec::new();

        for res in results {
            if res.matches.is_empty() {
                unmatched_files.push(res.candidate);
            } else {
                matched_files.push(res);
            }
        }

        BatchMatchOutcome {
            matched_files,
            unmatched_files,
        }
    }

    /// Evaluates a single `MiscVideoCandidate` against all compiled anchors WITHOUT short-circuiting:
    /// both the enclosing Directory Channel and the Filename/Title NLP Channel are evaluated for
    /// every anchor and fused via `dir_weight`, `title_weight`, and `corroboration_boost`.
    pub fn match_single(&self, candidate: &MiscVideoCandidate) -> FileMatchResult {
        let file_stem = candidate
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");

        // 1. Prepare Title Channel signals (clauses + POS-tagged token streams)
        let title_clauses = extract_clauses_with_nlp(file_stem, &self.noise_set, &self.nlp);

        let stem_norm_stripped = normalize_text_camel_aware(file_stem, Some(&self.noise_set));
        let stem_tokens_stripped: Vec<String> = stem_norm_stripped
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();
        let stem_tags_stripped = self.nlp.tag_tokens(&stem_tokens_stripped, file_stem);

        let stem_norm_raw = normalize_text_camel_aware(file_stem, None);
        let stem_tokens_raw: Vec<String> = stem_norm_raw
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();
        let stem_tags_raw = self.nlp.tag_tokens(&stem_tokens_raw, file_stem);

        // 2. Prepare Enclosing Directory Channel signals (all 4 tiers enabled on dir names too!)
        let mut dir_clauses: Vec<ExtractedClause> = Vec::new();
        let mut dir_tokens_stripped: Vec<String> = Vec::new();
        let mut dir_tags_stripped: Vec<PosTag> = Vec::new();
        let mut dir_tokens_raw: Vec<String> = Vec::new();
        let mut dir_tags_raw: Vec<PosTag> = Vec::new();

        if self.cfg.match_relative_subdirs {
            let dir_names =
                extract_enclosing_dir_names(&candidate.path, candidate.misc_root.as_deref());
            for (depth_idx, raw_dir) in dir_names.into_iter().enumerate() {
                let mut extracted =
                    extract_clauses_with_nlp(&raw_dir, &self.noise_set, &self.nlp);
                for c in &mut extracted {
                    c.position = depth_idx * 10 + c.position;
                    c.has_explicit_delim = true;
                }
                dir_clauses.extend(extracted);

                let d_stripped = normalize_text_camel_aware(&raw_dir, Some(&self.noise_set));
                let d_toks_s: Vec<String> = d_stripped
                    .split_whitespace()
                    .map(|s| s.to_string())
                    .collect();
                let d_tags_s = self.nlp.tag_tokens(&d_toks_s, &raw_dir);
                dir_tokens_stripped.extend(d_toks_s);
                dir_tags_stripped.extend(d_tags_s);

                let d_raw = normalize_text_camel_aware(&raw_dir, None);
                let d_toks_r: Vec<String> =
                    d_raw.split_whitespace().map(|s| s.to_string()).collect();
                let d_tags_r = self.nlp.tag_tokens(&d_toks_r, &raw_dir);
                dir_tokens_raw.extend(d_toks_r);
                dir_tags_raw.extend(d_tags_r);
            }
        }

        let title_pos_offset = dir_clauses.len() * 10 + 10;
        let mut raw_matches: Vec<RankedAnchorMatch> = Vec::new();

        for comp_anchor in &self.anchors {
            // ALWAYS evaluate BOTH channels (never short-circuit title evaluation when dir matches!)
            let dir_ev = if self.cfg.match_relative_subdirs && !dir_clauses.is_empty() {
                self.evaluate_channel(
                    comp_anchor,
                    &dir_clauses,
                    &dir_tokens_stripped,
                    &dir_tags_stripped,
                    &dir_tokens_raw,
                    &dir_tags_raw,
                    true,
                )
            } else {
                None
            };

            let title_ev = self.evaluate_channel(
                comp_anchor,
                &title_clauses,
                &stem_tokens_stripped,
                &stem_tags_stripped,
                &stem_tokens_raw,
                &stem_tags_raw,
                false,
            );

            if let Some(fused) =
                self.fuse_evidence(comp_anchor, dir_ev, title_ev, title_pos_offset)
            {
                raw_matches.push(fused);
            }
        }

        // Deterministic Multi-Anchor Ranking & Tie-Breaking:
        //   (1) Higher fused `confidence_score` (DESC, within 1e-6 epsilon)
        //   (2) Earlier `clause_position` (ASC)
        //   (3) Longer `matched_alias_alnum_len` (DESC)
        //   (4) Earlier `anchor_yaml_order` in `anchors.yaml` (ASC)
        raw_matches.sort_by(|a, b| {
            let score_diff = (a.confidence_score - b.confidence_score).abs();
            if score_diff > 1e-6 {
                b.confidence_score
                    .partial_cmp(&a.confidence_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            } else {
                a.clause_position
                    .cmp(&b.clause_position)
                    .then_with(|| b.matched_alias_alnum_len.cmp(&a.matched_alias_alnum_len))
                    .then_with(|| a.anchor_yaml_order.cmp(&b.anchor_yaml_order))
            }
        });

        let total = raw_matches.len();
        let max_symlink_rank = 1usize.saturating_add(self.cfg.max_secondary_symlinks);
        for (idx, m) in raw_matches.iter_mut().enumerate() {
            let rank = idx + 1;
            m.rank = rank;
            m.total_matches = total;
            m.planned_action = if rank == 1 {
                PlannedAction::PrimaryMove
            } else if rank <= max_symlink_rank {
                PlannedAction::Symlink
            } else {
                PlannedAction::RankExceededNoOp
            };
        }

        FileMatchResult {
            candidate: candidate.clone(),
            matches: raw_matches,
        }
    }

    /// Combines independent Directory evidence ($S_{\text{dir}}$) and Title evidence ($S_{\text{title}}$)
    /// without short-circuiting:
    /// - Directory influence is weighted by `dir_weight` (default `0.97`).
    /// - Title influence is weighted by `title_weight` (default `0.95`, already NLP POS-weighted).
    /// - When BOTH channels corroborate the same anchor, `corroboration_boost * min(S_dir, S_title)`
    ///   rewards the combined agreement up to `1.0000` (`MatchTier::DirAndTitle`)!
    fn fuse_evidence(
        &self,
        comp_anchor: &CompiledAnchor,
        dir_ev: Option<ChannelEvidence>,
        title_ev: Option<ChannelEvidence>,
        title_pos_offset: usize,
    ) -> Option<RankedAnchorMatch> {
        let min_score = if self.nlp.cfg.enabled {
            self.nlp.cfg.min_effective_score
        } else {
            self.cfg.fuzzy_min_similarity
        };

        let (final_tier, final_score, pos, alias_display, alias_len) = match (dir_ev, title_ev) {
            (Some(d), Some(t)) => {
                let d_weighted = self.cfg.dir_weight * d.raw_score;
                let t_weighted = self.cfg.title_weight * t.raw_score;
                let corrob =
                    self.cfg.corroboration_boost * d.raw_score.min(t.raw_score);
                let combined = (d_weighted.max(t_weighted) + corrob).clamp(0.0, 1.0);
                let (best_alias, best_len) = if t.alias_alnum_len > d.alias_alnum_len {
                    (t.alias_display, t.alias_alnum_len)
                } else {
                    (d.alias_display, d.alias_alnum_len)
                };
                (
                    MatchTier::DirAndTitle,
                    combined,
                    d.position.min(title_pos_offset + t.position),
                    best_alias,
                    best_len,
                )
            }
            (Some(d), None) => {
                let d_weighted = (self.cfg.dir_weight * d.raw_score).clamp(0.0, 1.0);
                let tier = if d.tier == MatchTier::ExactClause {
                    MatchTier::ExactSubdir
                } else {
                    MatchTier::DirSignal
                };
                (tier, d_weighted, d.position, d.alias_display, d.alias_alnum_len)
            }
            (None, Some(t)) => {
                let t_weighted = (self.cfg.title_weight * t.raw_score).clamp(0.0, 1.0);
                (
                    t.tier,
                    t_weighted,
                    title_pos_offset + t.position,
                    t.alias_display,
                    t.alias_alnum_len,
                )
            }
            (None, None) => return None,
        };

        if final_score + 1e-9 < min_score {
            return None;
        }

        Some(RankedAnchorMatch {
            rank: 0,
            total_matches: 0,
            planned_action: PlannedAction::PrimaryMove,
            anchor_idx: comp_anchor.anchor_idx,
            anchor_path: comp_anchor.entry.anchor_path.clone(),
            destination_path: comp_anchor.entry.destination_path.clone(),
            matched_alias: alias_display,
            match_tier: final_tier,
            confidence_score: final_score,
            clause_position: pos,
            matched_alias_alnum_len: alias_len,
            anchor_yaml_order: comp_anchor.entry.yaml_order,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_channel(
        &self,
        comp_anchor: &CompiledAnchor,
        clauses: &[ExtractedClause],
        tokens_stripped: &[String],
        tags_stripped: &[PosTag],
        tokens_raw: &[String],
        tags_raw: &[PosTag],
        is_dir_channel: bool,
    ) -> Option<ChannelEvidence> {
        let mut best: Option<ChannelEvidence> = None;

        let mut consider =
            |tier: MatchTier, score: f64, pos: usize, alias: &CompiledAlias| {
                let replace = match &best {
                    None => true,
                    Some(curr) => {
                        let diff = score - curr.raw_score;
                        if diff > 1e-6 {
                            true
                        } else if diff.abs() <= 1e-6 {
                            (pos, std::cmp::Reverse(alias.alnum_len))
                                < (curr.position, std::cmp::Reverse(curr.alias_alnum_len))
                        } else {
                            false
                        }
                    }
                };
                if replace {
                    best = Some(ChannelEvidence {
                        tier,
                        raw_score: score,
                        position: pos,
                        alias_display: alias.display_name.clone(),
                        alias_alnum_len: alias.alnum_len,
                    });
                }
            };

        for alias in &comp_anchor.aliases {
            // -----------------------------------------------------------------
            // Tier 1: Exact Clause Match
            // -----------------------------------------------------------------
            let mut matched_tier1 = false;
            for cl in clauses {
                let exact_norm = cl.norm == alias.norm;
                let exact_compact = alias.allow_compact_collapse
                    && alias.alnum_len >= self.cfg.compact_min_chars
                    && cl.compact == alias.compact
                    && sequel_numbers_compatible(&alias.numbers, &cl.numbers);

                if exact_norm || exact_compact {
                    let score_opt = if is_dir_channel {
                        Some(self.cfg.exact_clause_score)
                    } else if cl.has_explicit_delim && self.nlp.cfg.enabled {
                        let salience = self
                            .nlp
                            .phrase_pos_salience(&cl.pos_tags)
                            .max(self.nlp.phrase_pos_salience(&alias.pos_tags))
                            .max(0.80);
                        let factor = (1.0 - self.nlp.cfg.nlp_influence)
                            + (self.nlp.cfg.nlp_influence * salience);
                        Some((self.cfg.exact_clause_score * factor).clamp(0.0, 1.0))
                    } else {
                        self.nlp
                            .apply_pos_weight(self.cfg.exact_clause_score, &cl.pos_tags, false)
                    };

                    if let Some(s) = score_opt {
                        consider(MatchTier::ExactClause, s, cl.position, alias);
                        matched_tier1 = true;
                        break;
                    }
                }
            }
            if matched_tier1 {
                continue;
            }

            // -----------------------------------------------------------------
            // Tier 2: Whole-Token Contiguous Phrase Match (POS-weighted)
            // -----------------------------------------------------------------
            let mut matched_tier2 = false;
            for (token_stream, tag_stream) in
                [(tokens_stripped, tags_stripped), (tokens_raw, tags_raw)]
            {
                if let Some((tok_pos, tok_len)) = find_whole_token_phrase(
                    token_stream,
                    &alias.tokens,
                    &alias.compact,
                    alias.allow_compact_collapse && alias.alnum_len >= self.cfg.compact_min_chars,
                ) {
                    let matched_tags = &tag_stream[tok_pos..tok_pos + tok_len];
                    if let Some(s) = self.nlp.apply_pos_weight(
                        self.cfg.whole_token_score,
                        matched_tags,
                        false,
                    ) {
                        consider(MatchTier::WholeTokenPhrase, s, tok_pos, alias);
                        matched_tier2 = true;
                        break;
                    }
                }
            }
            if matched_tier2 {
                continue;
            }

            // -----------------------------------------------------------------
            // Tier 3: Spaceless Compact Prefix / Suffix Match (POS-weighted)
            // -----------------------------------------------------------------
            let mut matched_tier3 = false;
            if alias.allow_compact_collapse && alias.alnum_len >= self.cfg.compact_min_chars {
                for cl in clauses {
                    if cl.compact.len() > alias.compact.len()
                        && sequel_numbers_compatible(&alias.numbers, &cl.numbers)
                    {
                        let affix_hit = cl
                            .compact
                            .strip_prefix(&alias.compact)
                            .map(is_clean_compact_remainder)
                            .unwrap_or(false)
                            || cl
                                .compact
                                .strip_suffix(&alias.compact)
                                .map(is_clean_compact_remainder)
                                .unwrap_or(false);

                        if affix_hit {
                            if let Some(s) = self.nlp.apply_pos_weight(
                                self.cfg.compact_affix_score,
                                &alias.pos_tags,
                                false,
                            ) {
                                consider(MatchTier::CompactAffix, s, cl.position, alias);
                                matched_tier3 = true;
                                break;
                            }
                        }
                    }
                }
            }
            if matched_tier3 {
                continue;
            }

            // -----------------------------------------------------------------
            // Tier 4: POS-Weighted Fuzzy Clause Match (`strsim` + Noun weighting)
            // -----------------------------------------------------------------
            if self.cfg.fuzzy_enabled && alias.alnum_len >= self.cfg.fuzzy_min_chars {
                for cl in clauses {
                    let cl_len = cl.compact.chars().count();
                    if cl_len < self.cfg.fuzzy_min_chars {
                        continue;
                    }
                    let min_l = alias.alnum_len.min(cl_len) as f64;
                    let max_l = alias.alnum_len.max(cl_len) as f64;
                    if min_l / max_l < 0.70 {
                        continue;
                    }
                    if !sequel_numbers_strict_equal(&alias.numbers, &cl.numbers) {
                        continue;
                    }

                    let norm_lev = strsim::normalized_levenshtein(&alias.norm, &cl.norm);
                    let jw = strsim::jaro_winkler(&alias.norm, &cl.norm);
                    let char_blended = (norm_lev * 0.65) + (jw * 0.35);
                    let compact_lev = strsim::normalized_levenshtein(&alias.compact, &cl.compact);

                    let effective_sim = if self.nlp.cfg.enabled {
                        let pos_token_sim = self.nlp.weighted_token_fuzzy_similarity(
                            &alias.tokens,
                            &alias.pos_tags,
                            &cl.tokens,
                            &cl.pos_tags,
                        );
                        if cl.tokens.len() == 1 && alias.tokens.len() >= 2 {
                            compact_lev
                        } else {
                            char_blended.min(pos_token_sim).max(
                                if alias.tokens.len() == 1 && cl.tokens.len() == 1 {
                                    char_blended
                                } else {
                                    0.0
                                },
                            )
                        }
                    } else {
                        char_blended.max(compact_lev)
                    };

                    if effective_sim >= self.cfg.fuzzy_min_similarity {
                        let min_s = self.cfg.fuzzy_min_similarity;
                        let max_s = self.cfg.fuzzy_max_score.max(min_s);
                        let span = (1.0 - min_s).max(1e-6);
                        let raw_fuzzy_score = min_s
                            + ((effective_sim - min_s) / span).clamp(0.0, 1.0) * (max_s - min_s);

                        if let Some(s) = self.nlp.apply_pos_weight(
                            raw_fuzzy_score,
                            &alias.pos_tags,
                            false,
                        ) {
                            consider(MatchTier::FuzzyClause, s, cl.position, alias);
                        }
                    }
                }
            }
        }

        best
    }
}

fn find_whole_token_phrase(
    haystack: &[String],
    needle_tokens: &[String],
    needle_compact: &str,
    allow_single_token_compact: bool,
) -> Option<(usize, usize)> {
    if haystack.is_empty() || needle_tokens.is_empty() {
        return None;
    }
    let m = needle_tokens.len();
    if haystack.len() >= m {
        for start in 0..=(haystack.len() - m) {
            if haystack[start..start + m] == *needle_tokens {
                return Some((start, m));
            }
        }
    }
    if allow_single_token_compact && m >= 2 {
        for (idx, tok) in haystack.iter().enumerate() {
            if tok == needle_compact {
                return Some((idx, 1));
            }
        }
    }
    None
}

fn sequel_numbers_compatible(alias_nums: &[String], clause_nums: &[String]) -> bool {
    if alias_nums.is_empty() {
        return true;
    }
    alias_nums.iter().all(|n| clause_nums.contains(n))
}

fn sequel_numbers_strict_equal(alias_nums: &[String], clause_nums: &[String]) -> bool {
    alias_nums == clause_nums
}

fn is_clean_compact_remainder(rem: &str) -> bool {
    if rem.is_empty() {
        return false;
    }
    rem.chars().all(|c| c.is_ascii_digit()) || rem.chars().count() >= 3
}

fn extract_enclosing_dir_names(file_path: &Path, misc_root: Option<&Path>) -> Vec<String> {
    const SYSTEM_ROOT_NAMES: &[&str] = &[
        "volumes", "users", "home", "mnt", "media", "var", "tmp", "private",
    ];
    let mut dirs = Vec::new();
    let Some(parent) = file_path.parent() else {
        return dirs;
    };

    if let Some(root) = misc_root {
        if let Ok(rel_parent) = parent.strip_prefix(root) {
            let mut rel_comps: Vec<String> = rel_parent
                .components()
                .filter_map(|c| match c {
                    std::path::Component::Normal(os) => Some(os.to_string_lossy().to_string()),
                    _ => None,
                })
                .collect();
            rel_comps.reverse();
            dirs.extend(rel_comps);
        }
        if let Some(root_base) = root.file_name().and_then(|n| n.to_str()) {
            let lower = root_base.trim().to_lowercase();
            if !lower.is_empty() && !SYSTEM_ROOT_NAMES.contains(&lower.as_str()) {
                dirs.push(root_base.to_string());
            }
        }
    } else if let Some(parent_base) = parent.file_name().and_then(|n| n.to_str()) {
        let lower = parent_base.trim().to_lowercase();
        if !lower.is_empty() && !SYSTEM_ROOT_NAMES.contains(&lower.as_str()) {
            dirs.push(parent_base.to_string());
        }
    }

    dirs
}
