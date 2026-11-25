use crate::config::AnchorEntry;
use crate::matcher::normalize_text_camel_aware;
use std::collections::HashSet;
use std::path::Path;

/// A recall-oriented candidate anchor suggestion shown in `rusty_triage`'s Smart Suggestions bar.
#[derive(Debug, Clone)]
pub struct AnchorSuggestion {
    pub anchor_idx: usize,
    pub display_name: String,
    pub volume_hint: String,
    pub reason: String,
    pub score: f64,
}

#[derive(Debug, Clone)]
struct IndexedSuggestionAnchor {
    anchor_idx: usize,
    display_name: String,
    volume_hint: String,
    norm_variants: Vec<String>,
    compact_variants: Vec<String>,
    core_tokens: Vec<String>,
    initials: HashSet<String>,
}

const HONORIFIC_PREFIXES: &[&str] = &[
    "mistress",
    "goddess",
    "goddexx",
    "princess",
    "lady",
    "madam",
    "madame",
    "miss",
    "ms",
    "mrs",
    "queen",
    "empress",
    "domina",
    "dominatrix",
    "domme",
    "lord",
    "enchantress",
    "diva",
];

const SUGGESTION_STOPWORDS: &[&str] = &[
    "the", "and", "of", "in", "on", "to", "for", "with", "by", "from", "at", "a", "an", "my",
    "his", "her", "your", "new", "complete", "movie", "compl", "cpl", "feat", "featuring", "ft",
    "part", "pt", "h265", "x265", "h264", "hevc", "mp4", "mkv", "1080p", "1080", "720p", "4k",
    "hd", "uhd",
];

/// Fast recall-oriented suggestion engine for `rusty_triage` (`[1]`..`[5]` Smart Suggestions,
/// plus pre-filled alias & new-anchor folder names).
#[derive(Debug, Clone)]
pub struct SuggestionEngine {
    anchors: Vec<IndexedSuggestionAnchor>,
    noise_set: HashSet<String>,
}

impl SuggestionEngine {
    pub fn new(anchors: &[AnchorEntry], noise_tokens: &[String]) -> Self {
        let mut noise_set: HashSet<String> =
            noise_tokens.iter().map(|s| s.to_lowercase()).collect();
        for sw in ["h265", "mp4", "mkv", "com", "net", "org", "pl"] {
            noise_set.insert(sw.to_string());
        }

        let indexed = anchors
            .iter()
            .enumerate()
            .map(|(idx, entry)| {
                let display_name = entry
                    .anchor_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&entry.raw_key)
                    .to_string();

                let volume_hint = entry
                    .anchor_path
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();

                let mut raw_names = vec![display_name.clone()];
                raw_names.extend(entry.aliases.iter().cloned());

                let mut norm_variants = Vec::new();
                let mut compact_variants = Vec::new();
                let mut core_tokens_set = HashSet::new();
                let mut initials = HashSet::new();

                for rn in &raw_names {
                    let norm = normalize_text_camel_aware(rn, Some(&noise_set));
                    if norm.is_empty() {
                        continue;
                    }
                    let toks: Vec<String> =
                        norm.split_whitespace().map(|s| s.to_string()).collect();

                    // Compute multi-word initials (`the english mansion` -> `tem`, `young goddess kim` -> `ygk`,
                    // `brat princess` -> `bp`)!
                    if toks.len() >= 2 {
                        let all_init: String =
                            toks.iter().filter_map(|t| t.chars().next()).collect();
                        if all_init.len() >= 2 {
                            initials.insert(all_init);
                        }
                        let content_init: String = toks
                            .iter()
                            .filter(|t| !matches!(t.as_str(), "the" | "of" | "and" | "in" | "n"))
                            .filter_map(|t| t.chars().next())
                            .collect();
                        if content_init.len() >= 2 {
                            initials.insert(content_init);
                        }
                    }

                    for t in &toks {
                        if !HONORIFIC_PREFIXES.contains(&t.as_str())
                            && !SUGGESTION_STOPWORDS.contains(&t.as_str())
                            && t.len() >= 3
                        {
                            core_tokens_set.insert(strip_possessive(t));
                        }
                    }

                    let compact = toks.join("");
                    if !norm_variants.contains(&norm) {
                        norm_variants.push(norm);
                    }
                    if compact.len() >= 3 && !compact_variants.contains(&compact) {
                        compact_variants.push(compact);
                    }
                }

                let mut core_tokens: Vec<String> = core_tokens_set.into_iter().collect();
                core_tokens.sort();

                IndexedSuggestionAnchor {
                    anchor_idx: idx,
                    display_name,
                    volume_hint,
                    norm_variants,
                    compact_variants,
                    core_tokens,
                    initials,
                }
            })
            .collect();

        Self {
            anchors: indexed,
            noise_set,
        }
    }

    /// Computes up to `limit` (typically 5) smart anchor suggestions for `item_path` (a video file
    /// or parent directory path).
    pub fn top_suggestions(&self, item_path: &Path, limit: usize) -> Vec<AnchorSuggestion> {
        if limit == 0 || self.anchors.is_empty() {
            return Vec::new();
        }

        let mut raw_pieces: Vec<String> = Vec::new();
        if let Some(stem) = item_path.file_stem().and_then(|s| s.to_str()) {
            raw_pieces.push(strip_leading_date_or_pack_code(stem));
        }
        if let Some(parent_name) = item_path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
        {
            let cleaned_p = strip_leading_date_or_pack_code(parent_name);
            if !raw_pieces.contains(&cleaned_p) {
                raw_pieces.push(cleaned_p);
            }
        }

        let mut clauses: Vec<String> = Vec::new();
        let mut all_tokens: Vec<String> = Vec::new();
        let mut raw_short_codes: HashSet<String> = HashSet::new();

        for piece in &raw_pieces {
            // Extract short uppercase/alphanumeric codes like `TEM`, `YGK`, `BP2` -> `bp`
            for raw_tok in piece.split(|c: char| !c.is_alphanumeric()) {
                if raw_tok.is_empty() {
                    continue;
                }
                let alpha_only: String = raw_tok
                    .chars()
                    .take_while(|c| c.is_alphabetic())
                    .flat_map(|c| c.to_lowercase())
                    .collect();
                if (2..=5).contains(&alpha_only.len())
                    && !SUGGESTION_STOPWORDS.contains(&alpha_only.as_str())
                {
                    raw_short_codes.insert(alpha_only);
                }
            }

            let delim_norm = piece
                .replace("_-_", " | ")
                .replace(" - ", " | ")
                .replace('-', " | ")
                .replace('[', " | ")
                .replace(']', " | ");
            for sub in delim_norm.split('|') {
                let norm = normalize_text_camel_aware(sub, Some(&self.noise_set));
                if !norm.is_empty() {
                    for t in norm.split_whitespace() {
                        let clean_t = strip_possessive(t);
                        if !clean_t.is_empty() && !all_tokens.contains(&clean_t) {
                            all_tokens.push(clean_t);
                        }
                    }
                    if !clauses.contains(&norm) {
                        clauses.push(norm);
                    }
                }
            }
        }

        let mut scored: Vec<AnchorSuggestion> = Vec::new();

        for anc in &self.anchors {
            let mut best_score = 0.0f64;
            let mut best_reason = String::new();

            // 1. Acronym initials match (`TEM` -> `the english mansion`, `YGK` -> `young goddess kim`, `bp2` -> `brat princess`)
            for code in &raw_short_codes {
                if anc.initials.contains(code) {
                    let s = if code.len() >= 3 { 0.86 } else { 0.78 };
                    if s > best_score {
                        best_score = s;
                        best_reason = format!("acronym '{}'", code.to_uppercase());
                    }
                }
            }

            // 2. Core proper-noun token overlap (handles `Miss Courtney` <-> `Mistress Courtneys`,
            //    `Mistress Katharina` <-> `MsKatharina`, `Fetish Liza` <-> `fetish liza.com`)
            if !anc.core_tokens.is_empty() {
                let mut matched_core = Vec::new();
                for ct in &anc.core_tokens {
                    if all_tokens.iter().any(|vt| {
                        vt == ct
                            || (ct.len() >= 5
                                && vt.len() >= 5
                                && strsim::normalized_levenshtein(vt, ct) >= 0.84)
                    }) {
                        matched_core.push(ct.as_str());
                    }
                }
                if !matched_core.is_empty() {
                    let ratio = matched_core.len() as f64 / anc.core_tokens.len() as f64;
                    let s = 0.62 + 0.30 * ratio;
                    if s > best_score {
                        best_score = s;
                        best_reason = format!("token '{}'", matched_core.join(" "));
                    }
                }
            }

            // 3. Relaxed clause / compact fuzzy match (`1704_HouseofSinn` -> `house of sinn`,
            //    `1303_Club Stiletto` -> `club stilletto`)
            for cl in &clauses {
                let cl_compact: String = cl.chars().filter(|c| !c.is_whitespace()).collect();
                if cl_compact.len() < 4 {
                    continue;
                }
                for nv in &anc.norm_variants {
                    let sim = strsim::normalized_levenshtein(cl, nv);
                    if sim >= 0.58 && sim > best_score {
                        best_score = sim;
                        best_reason = "fuzzy clause".to_string();
                    }
                }
                for cv in &anc.compact_variants {
                    if cl_compact.contains(cv) && cv.len() >= 5 {
                        let s = 0.88;
                        if s > best_score {
                            best_score = s;
                            best_reason = "compact match".to_string();
                        }
                    } else {
                        let sim = strsim::normalized_levenshtein(&cl_compact, cv);
                        if sim >= 0.65 && sim > best_score {
                            best_score = sim;
                            best_reason = "compact fuzzy".to_string();
                        }
                    }
                }
            }

            if best_score >= 0.58 {
                scored.push(AnchorSuggestion {
                    anchor_idx: anc.anchor_idx,
                    display_name: anc.display_name.clone(),
                    volume_hint: anc.volume_hint.clone(),
                    reason: best_reason,
                    score: best_score,
                });
            }
        }

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.display_name.cmp(&b.display_name))
        });
        scored.truncate(limit);
        scored
    }
}

/// Strips leading numeric/date/pack prefixes such as `2022-04-10 - `, `0101-1-_`, `1704_`, `1612_2-`, `1704TEM`.
pub fn strip_leading_date_or_pack_code(raw: &str) -> String {
    let mut s = raw.trim();

    // Strip `YYYY-MM-DD` or `YYYY.MM.DD` prefix
    if s.len() > 10
        && s.as_bytes()[..4].iter().all(|b| b.is_ascii_digit())
        && matches!(s.as_bytes()[4], b'-' | b'.' | b'_')
        && s.as_bytes()[5..7].iter().all(|b| b.is_ascii_digit())
        && matches!(s.as_bytes()[7], b'-' | b'.' | b'_')
        && s.as_bytes()[8..10].iter().all(|b| b.is_ascii_digit())
    {
        s = &s[10..];
        s = s.trim_start_matches(|c: char| !c.is_alphanumeric());
    }

    // Strip leading digit/separator run (`0101-1-_`, `1704_`, `1812_3-`, `20_`, `1704` before uppercase)
    let chars: Vec<char> = s.chars().collect();
    let mut idx = 0usize;
    while idx < chars.len() && (chars[idx].is_ascii_digit() || matches!(chars[idx], '-' | '_' | ' ')) {
        idx += 1;
    }
    if idx > 0 && idx < chars.len() {
        let rem: String = chars[idx..].iter().collect();
        if rem.chars().any(|c| c.is_alphabetic()) {
            return rem;
        }
    }

    s.to_string()
}

/// Extracts a clean default creator/alias candidate from an unmatched item path to pre-fill the
/// `a` (Add Alias) and `n` (New Anchor `<Name>_h265`) modals in `rusty_triage`.
pub fn extract_candidate_creator_label(item_path: &Path) -> String {
    let source = item_path
        .file_stem()
        .and_then(|s| s.to_str())
        .or_else(|| item_path.file_name().and_then(|s| s.to_str()))
        .unwrap_or("");

    let stripped = strip_leading_date_or_pack_code(source)
        .trim_end_matches("_h265")
        .trim_end_matches(" H265")
        .trim_end_matches(" h265")
        .trim()
        .to_string();

    // Take the first structural clause before ` - `, `_-_`, or `-`
    for sep in [" - ", "_-_", " – ", "-"] {
        if let Some((head, _)) = stripped.split_once(sep) {
            let h = head.trim();
            if h.len() >= 2 {
                return h.to_string();
            }
        }
    }

    stripped
}

fn strip_possessive(token: &str) -> String {
    if token.len() > 5 && token.ends_with('s') && !token.ends_with("ss") {
        // Normalize trailing plural/possessive `courtneys` -> `courtney`
        token[..token.len() - 1].to_string()
    } else {
        token.to_string()
    }
}
