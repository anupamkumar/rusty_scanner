use crate::config::{AnchorDirsConfig, AnchorEntry, normalize_path_lexical, path_dev_inode};
use crate::matcher::{
    BatchMatchOutcome, FileMatchResult, MatchTier, PlannedAction, RankedAnchorMatch,
};
use crate::scanner::MiscVideoCandidate;
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

/// Escapes a CSV field according to RFC 4180.
pub fn csv_escape(field: &str) -> String {
    if field.contains(',')
        || field.contains('"')
        || field.contains('\n')
        || field.contains('\r')
    {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

/// Parses a single RFC-4180 CSV line into fields (supporting quoted commas and `""` escapes).
pub fn parse_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let chars: Vec<char> = line.trim_end_matches(['\r', '\n']).chars().collect();
    let mut i = 0usize;

    while i < chars.len() {
        let ch = chars[i];
        if in_quotes {
            if ch == '"' {
                if i + 1 < chars.len() && chars[i + 1] == '"' {
                    cur.push('"');
                    i += 2;
                    continue;
                } else {
                    in_quotes = false;
                }
            } else {
                cur.push(ch);
            }
        } else if ch == '"' && cur.is_empty() {
            in_quotes = true;
        } else if ch == ',' {
            fields.push(std::mem::take(&mut cur));
        } else {
            cur.push(ch);
        }
        i += 1;
    }
    fields.push(cur);
    fields
}

/// Writes the ranked anchor match table in RFC-4180 CSV format to `csv_out` (or stdout when `csv_out == "-"`).
pub fn write_matches_csv(outcome: &BatchMatchOutcome, csv_out: &Path) -> Result<usize> {
    let header = "misc_file_path,match_rank,total_matches,planned_action,matched_anchor_path,target_destination_path,matched_alias,match_tier,confidence_score\n";

    let write_body = |writer: &mut dyn Write| -> Result<usize> {
        writer.write_all(header.as_bytes())?;
        let mut count = 0usize;
        for file_res in &outcome.matched_files {
            let file_str = file_res.candidate.path.to_string_lossy();
            for m in &file_res.matches {
                let anchor_str = m.anchor_path.to_string_lossy();
                let dest_str = m.destination_path.to_string_lossy();
                let line = format!(
                    "{},{},{},{},{},{},{},{},{:.4}\n",
                    csv_escape(&file_str),
                    m.rank,
                    m.total_matches,
                    m.planned_action.as_str(),
                    csv_escape(&anchor_str),
                    csv_escape(&dest_str),
                    csv_escape(&m.matched_alias),
                    m.match_tier.as_str(),
                    m.confidence_score,
                );
                writer.write_all(line.as_bytes())?;
                count += 1;
            }
        }
        writer.flush()?;
        Ok(count)
    };

    let rows_written = if csv_out == Path::new("-") {
        let stdout = io::stdout();
        let mut handle = stdout.lock();
        write_body(&mut handle)?
    } else {
        if let Some(parent) = csv_out.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).with_context(|| {
                    format!("Failed to create parent directory for CSV {:?}", csv_out)
                })?;
            }
        }
        let file = File::create(csv_out)
            .with_context(|| format!("Failed to create CSV output file {:?}", csv_out))?;
        let mut writer = BufWriter::new(file);
        write_body(&mut writer)?
    };

    Ok(rows_written)
}

/// Loads a `manual_matches.csv` / `anchor_matches.csv` file back into `Vec<FileMatchResult>` so
/// `rusty_triage` can resume prior sessions and `rusty_scanner --from-csv` can execute curated matches.
pub fn read_matches_csv(
    csv_path: &Path,
    anchor_cfg: &AnchorDirsConfig,
) -> Result<Vec<FileMatchResult>> {
    let raw = fs::read_to_string(csv_path)
        .with_context(|| format!("Failed to read matches CSV at {:?}", csv_path))?;

    let mut anchor_lookup: HashMap<PathBuf, usize> = HashMap::new();
    for (idx, anc) in anchor_cfg.anchors.iter().enumerate() {
        anchor_lookup.insert(anc.anchor_path.clone(), idx);
        anchor_lookup.insert(anc.canonical_anchor_path.clone(), idx);
        anchor_lookup.insert(PathBuf::from(&anc.raw_key), idx);
    }

    let mut grouped: BTreeMap<PathBuf, Vec<RankedAnchorMatch>> = BTreeMap::new();

    for (line_idx, raw_line) in raw.lines().enumerate() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if line_idx == 0 && trimmed.starts_with("misc_file_path,") {
            continue;
        }

        let cols = parse_csv_line(raw_line);
        if cols.len() < 9 {
            bail!(
                "{}:{}: Expected 9 CSV columns, found {}",
                csv_path.display(),
                line_idx + 1,
                cols.len()
            );
        }

        let file_path = normalize_path_lexical(Path::new(&cols[0]));
        let rank: usize = cols[1].parse().unwrap_or(1);
        let total_matches: usize = cols[2].parse().unwrap_or(1);
        let planned_action = match cols[3].as_str() {
            "SYMLINK" => PlannedAction::Symlink,
            "RANK_EXCEEDED_NO_OP" => PlannedAction::RankExceededNoOp,
            _ => PlannedAction::PrimaryMove,
        };
        let matched_anchor_raw = normalize_path_lexical(Path::new(&cols[4]));
        let target_dest_raw = normalize_path_lexical(Path::new(&cols[5]));
        let matched_alias = cols[6].clone();
        let match_tier = match cols[7].as_str() {
            "DIR_AND_TITLE" => MatchTier::DirAndTitle,
            "EXACT_SUBDIR" => MatchTier::ExactSubdir,
            "DIR_SIGNAL" => MatchTier::DirSignal,
            "WHOLE_TOKEN_PHRASE" => MatchTier::WholeTokenPhrase,
            "COMPACT_AFFIX" => MatchTier::CompactAffix,
            "FUZZY_CLAUSE" => MatchTier::FuzzyClause,
            _ => MatchTier::ExactClause,
        };
        let confidence_score: f64 = cols[8].parse().unwrap_or(1.0);

        let Some(&anchor_idx) = anchor_lookup.get(&matched_anchor_raw) else {
            bail!(
                "{}:{}: Anchor path {:?} in CSV is not present in loaded anchors YAML",
                csv_path.display(),
                line_idx + 1,
                matched_anchor_raw
            );
        };
        let anc = &anchor_cfg.anchors[anchor_idx];

        grouped
            .entry(file_path)
            .or_default()
            .push(RankedAnchorMatch {
                rank,
                total_matches,
                planned_action,
                anchor_idx,
                anchor_path: anc.anchor_path.clone(),
                destination_path: if target_dest_raw.as_os_str().is_empty() {
                    anc.destination_path.clone()
                } else {
                    target_dest_raw
                },
                matched_alias_alnum_len: matched_alias
                    .chars()
                    .filter(|c| c.is_alphanumeric())
                    .count(),
                matched_alias,
                match_tier,
                confidence_score,
                clause_position: rank.saturating_sub(1),
                anchor_yaml_order: anc.yaml_order,
            });
    }

    let mut out = Vec::with_capacity(grouped.len());
    for (fpath, mut matches) in grouped {
        matches.sort_by_key(|m| m.rank);
        let total = matches.len();
        for (i, m) in matches.iter_mut().enumerate() {
            m.rank = i + 1;
            m.total_matches = total;
        }
        let (dev_inode, size_bytes) = match fs::metadata(&fpath) {
            Ok(meta) => (path_dev_inode(&fpath, &meta), meta.len()),
            Err(_) => ((0, 0), 0),
        };
        out.push(FileMatchResult {
            candidate: MiscVideoCandidate {
                path: fpath,
                dev_inode,
                size_bytes,
                misc_root: None,
            },
            matches,
        });
    }

    Ok(out)
}

/// Writes unmatched miscellaneous video paths (one absolute path per line, sorted) to `unmatched_out`.
pub fn write_unmatched_files(
    unmatched: &[MiscVideoCandidate],
    unmatched_out: &Path,
) -> Result<usize> {
    if let Some(parent) = unmatched_out.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create parent directory for unmatched list {:?}",
                    unmatched_out
                )
            })?;
        }
    }

    let file = File::create(unmatched_out).with_context(|| {
        format!(
            "Failed to create unmatched output file {:?}",
            unmatched_out
        )
    })?;
    let mut writer = BufWriter::new(file);
    let mut sorted_paths: Vec<String> = unmatched
        .iter()
        .map(|c| c.path.to_string_lossy().to_string())
        .collect();
    sorted_paths.sort();
    sorted_paths.dedup();

    for p in &sorted_paths {
        writeln!(writer, "{}", p)?;
    }
    writer.flush()?;
    Ok(sorted_paths.len())
}

/// Prints a concise human-readable match summary to stderr (so `--csv-out -` stays pure CSV on stdout).
pub fn print_match_summary(
    outcome: &BatchMatchOutcome,
    anchors: &[AnchorEntry],
    mode_label: &str,
) {
    let total_files = outcome.matched_files.len() + outcome.unmatched_files.len();
    let multi_matched_count = outcome
        .matched_files
        .iter()
        .filter(|r| r.matches.len() > 1)
        .count();
    let planned_symlinks: usize = outcome
        .matched_files
        .iter()
        .flat_map(|r| r.matches.iter())
        .filter(|m| m.planned_action == PlannedAction::Symlink)
        .count();

    let mut per_anchor_primary: BTreeMap<usize, usize> = BTreeMap::new();
    let mut per_anchor_symlinks: BTreeMap<usize, usize> = BTreeMap::new();
    for r in &outcome.matched_files {
        for m in &r.matches {
            match m.planned_action {
                PlannedAction::PrimaryMove => {
                    *per_anchor_primary.entry(m.anchor_idx).or_insert(0) += 1;
                }
                PlannedAction::Symlink => {
                    *per_anchor_symlinks.entry(m.anchor_idx).or_insert(0) += 1;
                }
                PlannedAction::RankExceededNoOp => {}
            }
        }
    }

    eprintln!("================================================================================");
    eprintln!("=== Rusty Intelli Scanner — Match Summary ({}) ===", mode_label);
    eprintln!("================================================================================");
    eprintln!("  Misc Videos Scanned        : {}", total_files);
    eprintln!(
        "  Matched Videos             : {} ({} multi-anchor matches)",
        outcome.matched_files.len(),
        multi_matched_count
    );
    eprintln!("  Secondary Symlinks Planned : {}", planned_symlinks);
    eprintln!(
        "  Unmatched Videos           : {}",
        outcome.unmatched_files.len()
    );

    if !per_anchor_primary.is_empty() || !per_anchor_symlinks.is_empty() {
        eprintln!("--------------------------------------------------------------------------------");
        eprintln!("  Per-Anchor Breakdown (Rank #1 Moves | Rank #2.. Secondary Symlinks):");
        for (idx, anchor) in anchors.iter().enumerate() {
            let prim = per_anchor_primary.get(&idx).copied().unwrap_or(0);
            let sym = per_anchor_symlinks.get(&idx).copied().unwrap_or(0);
            if prim > 0 || sym > 0 {
                eprintln!(
                    "    • {:?} -> dest {:?} : {} primary, {} symlink(s)",
                    anchor.anchor_path, anchor.destination_path, prim, sym
                );
            }
        }
    }
    eprintln!("================================================================================");
}
