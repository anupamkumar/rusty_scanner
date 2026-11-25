use crate::nlp::NlpConfig;
use anyhow::{Context, Result, bail};
use std::collections::{HashMap, HashSet};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Default technical noise tokens stripped during name normalization (unless explicitly matched as a raw alias).
pub const DEFAULT_NOISE_TOKENS: &[&str] = &[
    "1080p", "720p", "2160p", "1440p", "4k", "480p", "360p", "240p", "x264", "x265", "h264",
    "h265", "hevc", "avc", "aac", "ac3", "eac3", "mp3", "opus", "flac", "webrip", "web-dl",
    "webdl", "bluray", "brrip", "dvdrip", "hdrip", "uhd", "60fps", "30fps",
];

/// Default supported video file extensions (case-insensitive, without leading dot).
pub const DEFAULT_VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "avi", "wmv", "mov", "flv", "webm", "m4v", "mpg", "mpeg", "ts", "mts", "m2ts",
    "vob", "ogv", "3gp", "asf", "rm", "rmvb", "divx", "f4v",
];

/// A single configured anchor directory entry parsed from `anchor_dirs.yaml`.
#[derive(Debug, Clone)]
pub struct AnchorEntry {
    /// Original absolute path string written as the map key in `anchor_dirs.yaml`.
    pub raw_key: String,
    /// Lexically normalized absolute path of the anchor root directory.
    pub anchor_path: PathBuf,
    /// Canonicalized path if the directory currently exists on disk, else `anchor_path`.
    pub canonical_anchor_path: PathBuf,
    /// Cached `(dev, inode)` identifier when the anchor directory exists on disk.
    pub dev_inode: Option<(u64, u64)>,
    /// Alternate names / acronyms configured under `aliases:`.
    pub aliases: Vec<String>,
    /// Optional raw `destination:` field from `anchor_dirs.yaml`.
    pub destination_raw: Option<String>,
    /// Resolved target directory where Rank #1 moves and Rank #2-#4 symlinks land.
    /// Defaults to `anchor_path` when `destination` is omitted.
    pub destination_path: PathBuf,
    /// Zero-based declaration index in `anchor_dirs.yaml` (used as final deterministic tie-breaker).
    pub yaml_order: usize,
}

impl AnchorEntry {
    /// Returns true if this anchor's resolved destination directory differs from its root directory.
    pub fn has_custom_destination(&self) -> bool {
        normalize_path_lexical(&self.destination_path) != normalize_path_lexical(&self.anchor_path)
    }
}

/// Parsed and validated collection of anchors from `anchor_dirs.yaml`.
#[derive(Debug, Clone)]
pub struct AnchorDirsConfig {
    pub version: String,
    pub anchors: Vec<AnchorEntry>,
}

/// Matching & ranking engine configuration parsed from `scanner_config.yaml`.
#[derive(Debug, Clone)]
pub struct MatchingConfig {
    pub exact_clause_score: f64,
    pub whole_token_score: f64,
    pub compact_affix_score: f64,
    pub compact_min_chars: usize,
    pub fuzzy_enabled: bool,
    pub fuzzy_min_similarity: f64,
    pub fuzzy_max_score: f64,
    pub fuzzy_min_chars: usize,
    pub match_relative_subdirs: bool,
    pub dir_weight: f64,
    pub title_weight: f64,
    pub corroboration_boost: f64,
    pub max_secondary_symlinks: usize,
    pub noise_tokens: Vec<String>,
    pub nlp: NlpConfig,
}

impl Default for MatchingConfig {
    fn default() -> Self {
        Self {
            exact_clause_score: 1.00,
            whole_token_score: 0.95,
            compact_affix_score: 0.90,
            compact_min_chars: 4,
            fuzzy_enabled: true,
            fuzzy_min_similarity: 0.88,
            fuzzy_max_score: 0.94,
            fuzzy_min_chars: 5,
            match_relative_subdirs: true,
            dir_weight: 0.97,
            title_weight: 0.95,
            corroboration_boost: 0.08,
            max_secondary_symlinks: 3,
            noise_tokens: DEFAULT_NOISE_TOKENS.iter().map(|s| s.to_string()).collect(),
            nlp: NlpConfig {
                min_effective_score: 0.84,
                ..NlpConfig::default()
            },
        }
    }
}

/// Controls how `rusty_dedup_metrics_harness.py` wraps `rusty_dedup` invocations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessSetting {
    /// Automatically locate and require `scripts/rusty_dedup_metrics_harness.py` (default).
    Auto,
    /// Explicit path to `rusty_dedup_metrics_harness.py`.
    Explicit(PathBuf),
    /// Explicitly bypass the Python metrics harness (`none` / `off` / `false`).
    Disabled,
}

/// External tool and state paths parsed from `scanner_config.yaml`.
#[derive(Debug, Clone)]
pub struct ToolsConfig {
    pub python_bin: String,
    pub dedup_metrics_harness: HarnessSetting,
    pub harness_linger_secs: Option<f64>,
    pub rusty_dedup_bin: Option<PathBuf>,
    pub rusty_clean_bin: Option<PathBuf>,
    pub dedup_config: Option<PathBuf>,
    pub decision_config: Option<PathBuf>,
    pub states_dir: PathBuf,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            python_bin: "python3".to_string(),
            dedup_metrics_harness: HarnessSetting::Auto,
            harness_linger_secs: None,
            rusty_dedup_bin: None,
            rusty_clean_bin: None,
            dedup_config: None,
            decision_config: None,
            states_dir: PathBuf::from(".states"),
        }
    }
}

/// Persistent job report archiving & interactive HTML audit dashboard configuration.
#[derive(Debug, Clone)]
pub struct ReportsConfig {
    pub enabled: bool,
    pub output_dir: PathBuf,
    pub capture_pair_categories_log: bool,
}

impl Default for ReportsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            output_dir: PathBuf::from("./reports"),
            capture_pair_categories_log: false,
        }
    }
}

/// Lightweight built-in HTTP server configuration for hosting HTML & JSON reports remotely.
#[derive(Debug, Clone)]
pub struct ReportServerConfig {
    pub enabled: bool,
    pub listen_addr: String,
    pub serve_during_run: bool,
    pub linger_after_run_secs: u64,
    pub live_refresh_secs: u64,
}

impl Default for ReportServerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listen_addr: "0.0.0.0:8787".to_string(),
            serve_during_run: true,
            linger_after_run_secs: 0,
            live_refresh_secs: 5,
        }
    }
}

/// Complete configuration loaded from `scanner_config.yaml`.
#[derive(Debug, Clone)]
pub struct ScannerConfig {
    pub version: String,
    pub matching: MatchingConfig,
    pub tools: ToolsConfig,
    pub reports: ReportsConfig,
    pub report_server: ReportServerConfig,
    pub video_extensions: HashSet<String>,
}

impl Default for ScannerConfig {
    fn default() -> Self {
        Self {
            version: "1.0".to_string(),
            matching: MatchingConfig::default(),
            tools: ToolsConfig::default(),
            reports: ReportsConfig::default(),
            report_server: ReportServerConfig::default(),
            video_extensions: DEFAULT_VIDEO_EXTENSIONS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

/// Resolved concrete binary, harness, and config paths required for Live Run execution.
#[derive(Debug, Clone)]
pub struct ResolvedTools {
    pub python_bin: PathBuf,
    pub dedup_metrics_harness: Option<PathBuf>,
    pub harness_linger_secs: Option<f64>,
    pub rusty_dedup_bin: PathBuf,
    pub rusty_clean_bin: PathBuf,
    pub dedup_config: Option<PathBuf>,
    pub decision_config: Option<PathBuf>,
    pub states_dir: PathBuf,
}

/// Lexically normalizes a path (resolving `.` and `..` without touching disk).
pub fn normalize_path_lexical(path: &Path) -> PathBuf {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(path)
    } else {
        path.to_path_buf()
    };
    let mut cleaned = PathBuf::new();
    for comp in abs.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                cleaned.pop();
            }
            other => cleaned.push(other.as_os_str()),
        }
    }
    cleaned
}

/// Computes `(dev, inode)` for deduplication and same-directory checks.
#[cfg(unix)]
pub fn path_dev_inode(_path: &Path, meta: &fs::Metadata) -> (u64, u64) {
    (meta.dev(), meta.ino())
}

#[cfg(not(unix))]
pub fn path_dev_inode(path: &Path, _meta: &fs::Metadata) -> (u64, u64) {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| normalize_path_lexical(path));
    let prefix_str = canonical
        .components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .unwrap_or_else(|| "default_vol".to_string());
    let dev_hash = blake3::hash(prefix_str.as_bytes());
    let db = dev_hash.as_bytes();
    let dev = u64::from_le_bytes([db[0], db[1], db[2], db[3], db[4], db[5], db[6], db[7]]);

    let path_norm = canonical.to_string_lossy().to_lowercase();
    let ino_hash = blake3::hash(path_norm.as_bytes());
    let ib = ino_hash.as_bytes();
    let ino = u64::from_le_bytes([ib[0], ib[1], ib[2], ib[3], ib[4], ib[5], ib[6], ib[7]]);
    (dev, ino)
}

/// Strips trailing YAML `# ...` comments while respecting single/double quotes and `#` inside paths
/// (a `#` only starts a comment if at index 0 or preceded by whitespace outside quotes).
pub fn strip_yaml_comment(line: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    let mut prev_char: Option<char> = None;

    for (byte_idx, ch) in line.char_indices() {
        match ch {
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                let escaped = prev_char == Some('\\');
                if !escaped {
                    in_double = !in_double;
                }
            }
            '#' if !in_single && !in_double => {
                if byte_idx == 0 || prev_char.map(|c| c.is_whitespace()).unwrap_or(false) {
                    return &line[..byte_idx];
                }
            }
            _ => {}
        }
        prev_char = Some(ch);
    }
    line
}

/// Unquotes a single- or double-quoted YAML scalar value.
pub fn unquote_yaml_scalar(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() >= 2 {
        if (trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
        {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
    }
    trimmed.to_string()
}

/// Splits a YAML `key: value` line on the first colon that represents a YAML mapping separator
/// (outside quotes, and either at end-of-line or followed by whitespace — preserving Windows `C:\...` drive colons!).
fn split_yaml_key_value(trimmed: &str) -> Option<(&str, &str)> {
    let mut in_single = false;
    let mut in_double = false;
    let mut prev_char: Option<char> = None;
    let bytes = trimmed.as_bytes();

    for (idx, ch) in trimmed.char_indices() {
        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single && prev_char != Some('\\') => in_double = !in_double,
            ':' if !in_single && !in_double => {
                let next_ok = if idx + 1 >= bytes.len() {
                    true
                } else {
                    trimmed[idx + 1..]
                        .chars()
                        .next()
                        .map(|nc| nc.is_whitespace())
                        .unwrap_or(true)
                };
                if next_ok {
                    return Some((trimmed[..idx].trim(), trimmed[idx + 1..].trim()));
                }
            }
            _ => {}
        }
        prev_char = Some(ch);
    }
    None
}

/// Parses an inline YAML sequence like `[a, "b, c", 'd']`.
fn parse_inline_yaml_list(val: &str) -> Option<Vec<String>> {
    let t = val.trim();
    if !t.starts_with('[') || !t.ends_with(']') {
        return None;
    }
    let inner = t[1..t.len() - 1].trim();
    if inner.is_empty() {
        return Some(Vec::new());
    }
    let mut items = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut prev_char: Option<char> = None;

    for ch in inner.chars() {
        match ch {
            '\'' if !in_double => {
                in_single = !in_single;
                cur.push(ch);
            }
            '"' if !in_single && prev_char != Some('\\') => {
                in_double = !in_double;
                cur.push(ch);
            }
            ',' if !in_single && !in_double => {
                let cleaned = unquote_yaml_scalar(&cur);
                if !cleaned.is_empty() {
                    items.push(cleaned);
                }
                cur.clear();
            }
            _ => cur.push(ch),
        }
        prev_char = Some(ch);
    }
    let tail = unquote_yaml_scalar(&cur);
    if !tail.is_empty() {
        items.push(tail);
    }
    Some(items)
}

fn is_yaml_empty_or_null(val: &str) -> bool {
    let t = val.trim();
    t.is_empty() || t == "{}" || t == "~" || t.eq_ignore_ascii_case("null")
}

#[derive(Default, Clone)]
struct RawAnchorBuilder {
    raw_key: String,
    line_no: usize,
    aliases: Vec<String>,
    destination: Option<String>,
}

impl AnchorDirsConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read anchor directories YAML at {:?}", path))?;
        Self::from_yaml_str(&content, path)
    }

    /// Parses `anchor_dirs.yaml` supporting:
    /// 1. Map style under `anchors:` (`/abs/path:\n  aliases: [...]\n  destination: ...`)
    /// 2. Bare top-level map keys (`/abs/path: ...`)
    /// 3. Inline alias list shorthand (`/abs/path: [alias1, alias2]`) or `{}` empty map
    /// 4. List fallback (`- path: /abs/path\n  aliases: [...]` or `- /abs/path`)
    pub fn from_yaml_str(content: &str, source_label: &Path) -> Result<Self> {
        let mut version = "1.0".to_string();
        let mut raw_entries: Vec<RawAnchorBuilder> = Vec::new();

        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Mode {
            Root,
            InAnchorsSection { section_indent: usize },
        }

        #[derive(Clone, Copy, PartialEq, Eq)]
        enum SubState {
            None,
            InAnchorEntry {
                entry_indent: usize,
            },
            InAliasesList {
                entry_indent: usize,
                aliases_key_indent: usize,
            },
        }

        let mut mode = Mode::Root;
        let mut sub_state = SubState::None;

        for (idx, raw_line) in content.lines().enumerate() {
            let line_no = idx + 1;
            let stripped = strip_yaml_comment(raw_line);
            let trimmed = stripped.trim();
            if trimmed.is_empty() || trimmed == "---" || trimmed == "..." {
                continue;
            }
            let indent = stripped.chars().take_while(|c| c.is_whitespace()).count();

            // Unwind nested states if indentation moved back out
            match sub_state {
                SubState::InAliasesList {
                    entry_indent,
                    aliases_key_indent,
                } => {
                    // Note: YAML allows sequence items `- foo` at the SAME indent as `aliases:` (`indent == aliases_key_indent`)
                    let is_seq_item = trimmed.starts_with("- ") || trimmed == "-";
                    if indent < aliases_key_indent || (indent == aliases_key_indent && !is_seq_item)
                    {
                        if indent <= entry_indent {
                            sub_state = SubState::None;
                        } else {
                            sub_state = SubState::InAnchorEntry { entry_indent };
                        }
                    }
                }
                SubState::InAnchorEntry { entry_indent } => {
                    if indent <= entry_indent {
                        sub_state = SubState::None;
                    }
                }
                SubState::None => {}
            }

            if let Mode::InAnchorsSection { section_indent } = mode {
                if indent <= section_indent && sub_state == SubState::None {
                    mode = Mode::Root;
                }
            }

            // Handle active `InAliasesList` items (`- AliasName`)
            if let SubState::InAliasesList { .. } = sub_state {
                if let Some(rest) = trimmed.strip_prefix('-') {
                    let alias_val = unquote_yaml_scalar(rest);
                    if !alias_val.is_empty() {
                        if let Some(last) = raw_entries.last_mut() {
                            last.aliases.push(alias_val);
                        }
                    }
                    continue;
                }
            }

            // Handle active `InAnchorEntry` fields (`aliases:`, `destination:`, `path:`)
            if let SubState::InAnchorEntry { entry_indent } = sub_state {
                if let Some((k_raw, v_raw)) = split_yaml_key_value(trimmed) {
                    let k = unquote_yaml_scalar(k_raw).to_lowercase();
                    match k.as_str() {
                        "path" => {
                            let p = unquote_yaml_scalar(v_raw);
                            if let Some(last) = raw_entries.last_mut() {
                                last.raw_key = p;
                            }
                            continue;
                        }
                        "aliases" | "alias" => {
                            if let Some(inline_items) = parse_inline_yaml_list(v_raw) {
                                if let Some(last) = raw_entries.last_mut() {
                                    last.aliases.extend(inline_items);
                                }
                            } else if is_yaml_empty_or_null(v_raw) {
                                sub_state = SubState::InAliasesList {
                                    entry_indent,
                                    aliases_key_indent: indent,
                                };
                            } else {
                                let single = unquote_yaml_scalar(v_raw);
                                if !single.is_empty() {
                                    if let Some(last) = raw_entries.last_mut() {
                                        last.aliases.push(single);
                                    }
                                }
                            }
                            continue;
                        }
                        "destination" | "dest" => {
                            if !is_yaml_empty_or_null(v_raw) {
                                let d = unquote_yaml_scalar(v_raw);
                                if !d.is_empty() {
                                    if let Some(last) = raw_entries.last_mut() {
                                        last.destination = Some(d);
                                    }
                                }
                            }
                            continue;
                        }
                        _ => {
                            bail!(
                                "{}:{}: Unknown anchor field '{}' (expected 'aliases' or 'destination')",
                                source_label.display(),
                                line_no,
                                k_raw
                            );
                        }
                    }
                }
            }

            // Otherwise we are at `Root` or directly inside `anchors:` expecting an anchor entry
            if let Some(rest) = trimmed.strip_prefix('-') {
                // Sequence fallback: `- /abs/path` or `- path: /abs/path`
                let item_trimmed = rest.trim();
                if let Some((k_raw, v_raw)) = split_yaml_key_value(item_trimmed) {
                    let k = unquote_yaml_scalar(k_raw).to_lowercase();
                    if k == "path" {
                        raw_entries.push(RawAnchorBuilder {
                            raw_key: unquote_yaml_scalar(v_raw),
                            line_no,
                            aliases: Vec::new(),
                            destination: None,
                        });
                        sub_state = SubState::InAnchorEntry {
                            entry_indent: indent,
                        };
                        continue;
                    }
                }
                let direct_path = unquote_yaml_scalar(item_trimmed);
                if !direct_path.is_empty() {
                    raw_entries.push(RawAnchorBuilder {
                        raw_key: direct_path,
                        line_no,
                        aliases: Vec::new(),
                        destination: None,
                    });
                }
                continue;
            }

            if let Some((k_raw, v_raw)) = split_yaml_key_value(trimmed) {
                let k_unquoted = unquote_yaml_scalar(k_raw);
                if mode == Mode::Root && k_unquoted == "version" {
                    if !v_raw.is_empty() {
                        version = unquote_yaml_scalar(v_raw);
                    }
                    continue;
                }
                if mode == Mode::Root && k_unquoted == "anchors" {
                    if is_yaml_empty_or_null(v_raw) {
                        mode = Mode::InAnchorsSection {
                            section_indent: indent,
                        };
                    }
                    continue;
                }

                // Treat `k_unquoted` as an anchor directory map key!
                let mut builder = RawAnchorBuilder {
                    raw_key: k_unquoted,
                    line_no,
                    aliases: Vec::new(),
                    destination: None,
                };
                if let Some(inline_aliases) = parse_inline_yaml_list(v_raw) {
                    builder.aliases = inline_aliases;
                    raw_entries.push(builder);
                } else if is_yaml_empty_or_null(v_raw) {
                    raw_entries.push(builder);
                    sub_state = SubState::InAnchorEntry {
                        entry_indent: indent,
                    };
                } else {
                    bail!(
                        "{}:{}: Unexpected inline value '{}' for anchor '{}' (expected nested 'aliases:'/'destination:', '{{}}', or '[alias1, ...]')",
                        source_label.display(),
                        line_no,
                        v_raw,
                        builder.raw_key
                    );
                }
            } else {
                bail!(
                    "{}:{}: Could not parse YAML line: {}",
                    source_label.display(),
                    line_no,
                    trimmed
                );
            }
        }

        if raw_entries.is_empty() {
            bail!(
                "No anchor directories configured in {:?}. Add absolute directory paths under 'anchors:'.",
                source_label
            );
        }

        // Validate and deduplicate anchors preserving declaration order
        let mut anchors: Vec<AnchorEntry> = Vec::with_capacity(raw_entries.len());
        let mut seen_by_inode: HashMap<(u64, u64), usize> = HashMap::new();
        let mut seen_by_path: HashMap<PathBuf, usize> = HashMap::new();

        for raw in raw_entries {
            let raw_path = PathBuf::from(&raw.raw_key);
            if !raw_path.is_absolute() {
                bail!(
                    "{}:{}: Anchor path {:?} must be an absolute path.",
                    source_label.display(),
                    raw.line_no,
                    raw.raw_key
                );
            }

            let anchor_path = normalize_path_lexical(&raw_path);
            let canonical_anchor_path =
                fs::canonicalize(&anchor_path).unwrap_or_else(|_| anchor_path.clone());
            let dev_inode = fs::metadata(&anchor_path)
                .ok()
                .map(|m| path_dev_inode(&anchor_path, &m));

            let destination_path = match &raw.destination {
                None => anchor_path.clone(),
                Some(d) if d.trim().is_empty() => anchor_path.clone(),
                Some(d) => {
                    let dp = Path::new(d.trim());
                    if dp.is_absolute() {
                        normalize_path_lexical(dp)
                    } else {
                        normalize_path_lexical(&anchor_path.join(dp))
                    }
                }
            };

            let mut dedup_aliases = Vec::new();
            let mut alias_seen = HashSet::new();
            for al in raw.aliases {
                let t = al.trim().to_string();
                if !t.is_empty() && alias_seen.insert(t.to_lowercase()) {
                    dedup_aliases.push(t);
                }
            }

            let existing_idx = dev_inode
                .and_then(|id| seen_by_inode.get(&id).copied())
                .or_else(|| seen_by_path.get(&canonical_anchor_path).copied());

            if let Some(idx) = existing_idx {
                let existing = &mut anchors[idx];
                let mut existing_lower: HashSet<String> =
                    existing.aliases.iter().map(|s| s.to_lowercase()).collect();
                for al in dedup_aliases {
                    if existing_lower.insert(al.to_lowercase()) {
                        existing.aliases.push(al);
                    }
                }
                if existing.destination_raw.is_none() && raw.destination.is_some() {
                    existing.destination_raw = raw.destination;
                    existing.destination_path = destination_path;
                }
            } else {
                let yaml_order = anchors.len();
                if let Some(id) = dev_inode {
                    seen_by_inode.insert(id, yaml_order);
                }
                seen_by_path.insert(canonical_anchor_path.clone(), yaml_order);

                anchors.push(AnchorEntry {
                    raw_key: raw.raw_key,
                    anchor_path,
                    canonical_anchor_path,
                    dev_inode,
                    aliases: dedup_aliases,
                    destination_raw: raw.destination,
                    destination_path,
                    yaml_order,
                });
            }
        }

        Ok(Self { version, anchors })
    }
}

impl ScannerConfig {
    /// Resolves and loads `scanner_config.yaml`:
    /// 1. Explicit `--config <PATH>` (errors if missing)
    /// 2. `./scanner_config.yaml` in CWD
    /// 3. `<exe_dir>/scanner_config.yaml` alongside `rusty_scanner`
    /// 4. Built-in defaults if none present
    pub fn load_or_default(explicit_path: Option<&Path>, verbose: bool) -> Result<(Self, Option<PathBuf>)> {
        if let Some(p) = explicit_path {
            if !p.exists() {
                bail!("Specified scanner config file does not exist: {:?}", p);
            }
            let cfg = Self::load(p)?;
            return Ok((cfg, Some(p.to_path_buf())));
        }

        let cwd_candidate = PathBuf::from("scanner_config.yaml");
        if cwd_candidate.exists() {
            let cfg = Self::load(&cwd_candidate)?;
            return Ok((cfg, Some(cwd_candidate)));
        }

        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                let exe_candidate = exe_dir.join("scanner_config.yaml");
                if exe_candidate.exists() {
                    let cfg = Self::load(&exe_candidate)?;
                    return Ok((cfg, Some(exe_candidate)));
                }
            }
        }

        if verbose {
            eprintln!("[Verbose] No scanner_config.yaml found; using built-in default configuration.");
        }
        Ok((Self::default(), None))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read scanner config at {:?}", path))?;
        Self::from_yaml_str(&content, path)
    }

    pub fn from_yaml_str(content: &str, source_label: &Path) -> Result<Self> {
        let mut cfg = Self::default();
        let mut custom_noise_started = false;
        let mut custom_exts_started = false;

        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Section {
            Root,
            Matching { indent: usize },
            NoiseTokens { matching_indent: usize, key_indent: usize },
            Nlp { indent: usize },
            Tools { indent: usize },
            Reports { indent: usize },
            ReportServer { indent: usize },
            VideoExtensions { key_indent: usize },
        }

        let mut section = Section::Root;

        for (idx, raw_line) in content.lines().enumerate() {
            let line_no = idx + 1;
            let stripped = strip_yaml_comment(raw_line);
            let trimmed = stripped.trim();
            if trimmed.is_empty() || trimmed == "---" || trimmed == "..." {
                continue;
            }
            let indent = stripped.chars().take_while(|c| c.is_whitespace()).count();
            let is_seq_item = trimmed.starts_with("- ") || trimmed == "-";

            // Unwind indentation
            match section {
                Section::NoiseTokens {
                    matching_indent,
                    key_indent,
                } => {
                    if indent < key_indent || (indent == key_indent && !is_seq_item) {
                        if indent <= matching_indent {
                            section = Section::Root;
                        } else {
                            section = Section::Matching {
                                indent: matching_indent,
                            };
                        }
                    }
                }
                Section::VideoExtensions { key_indent } => {
                    if indent < key_indent || (indent == key_indent && !is_seq_item) {
                        section = Section::Root;
                    }
                }
                Section::Matching { indent: sec_ind }
                | Section::Nlp { indent: sec_ind }
                | Section::Tools { indent: sec_ind }
                | Section::Reports { indent: sec_ind }
                | Section::ReportServer { indent: sec_ind } => {
                    if indent <= sec_ind {
                        section = Section::Root;
                    }
                }
                Section::Root => {}
            }

            match section {
                Section::NoiseTokens { .. } => {
                    if let Some(rest) = trimmed.strip_prefix('-') {
                        if !custom_noise_started {
                            cfg.matching.noise_tokens.clear();
                            custom_noise_started = true;
                        }
                        let tok = unquote_yaml_scalar(rest).to_lowercase();
                        if !tok.is_empty() {
                            cfg.matching.noise_tokens.push(tok);
                        }
                        continue;
                    }
                }
                Section::VideoExtensions { .. } => {
                    if let Some(rest) = trimmed.strip_prefix('-') {
                        if !custom_exts_started {
                            cfg.video_extensions.clear();
                            custom_exts_started = true;
                        }
                        let ext = unquote_yaml_scalar(rest)
                            .trim_start_matches('.')
                            .to_lowercase();
                        if !ext.is_empty() {
                            cfg.video_extensions.insert(ext);
                        }
                        continue;
                    }
                }
                _ => {}
            }

            let Some((k_raw, v_raw)) = split_yaml_key_value(trimmed) else {
                bail!(
                    "{}:{}: Invalid YAML line: {}",
                    source_label.display(),
                    line_no,
                    trimmed
                );
            };
            let key = unquote_yaml_scalar(k_raw);
            let val = unquote_yaml_scalar(v_raw);

            match section {
                Section::Root => match key.as_str() {
                    "version" => {
                        if !val.is_empty() {
                            cfg.version = val;
                        }
                    }
                    "matching" => {
                        section = Section::Matching { indent };
                    }
                    "nlp" => {
                        section = Section::Nlp { indent };
                    }
                    "tools" => {
                        section = Section::Tools { indent };
                    }
                    "reports" => {
                        section = Section::Reports { indent };
                    }
                    "report_server" => {
                        section = Section::ReportServer { indent };
                    }
                    "video_extensions" => {
                        if let Some(list) = parse_inline_yaml_list(v_raw) {
                            cfg.video_extensions = list
                                .into_iter()
                                .map(|s| s.trim_start_matches('.').to_lowercase())
                                .filter(|s| !s.is_empty())
                                .collect();
                        } else if is_yaml_empty_or_null(v_raw) {
                            section = Section::VideoExtensions { key_indent: indent };
                        }
                    }
                    other => {
                        bail!(
                            "{}:{}: Unknown top-level key '{}' in scanner_config.yaml",
                            source_label.display(),
                            line_no,
                            other
                        );
                    }
                },
                Section::Matching {
                    indent: matching_indent,
                } => match key.as_str() {
                    "exact_clause_score" => {
                        cfg.matching.exact_clause_score = parse_f64_score(&val, &key, source_label, line_no)?;
                    }
                    "whole_token_score" => {
                        cfg.matching.whole_token_score = parse_f64_score(&val, &key, source_label, line_no)?;
                    }
                    "compact_affix_score" => {
                        cfg.matching.compact_affix_score = parse_f64_score(&val, &key, source_label, line_no)?;
                    }
                    "compact_min_chars" => {
                        cfg.matching.compact_min_chars = parse_usize_field(&val, &key, source_label, line_no)?;
                    }
                    "fuzzy_enabled" => {
                        cfg.matching.fuzzy_enabled = parse_bool_field(&val, &key, source_label, line_no)?;
                    }
                    "fuzzy_min_similarity" => {
                        cfg.matching.fuzzy_min_similarity = parse_f64_score(&val, &key, source_label, line_no)?;
                    }
                    "fuzzy_max_score" => {
                        cfg.matching.fuzzy_max_score = parse_f64_score(&val, &key, source_label, line_no)?;
                    }
                    "fuzzy_min_chars" => {
                        cfg.matching.fuzzy_min_chars = parse_usize_field(&val, &key, source_label, line_no)?;
                    }
                    "match_relative_subdirs" => {
                        cfg.matching.match_relative_subdirs = parse_bool_field(&val, &key, source_label, line_no)?;
                    }
                    "dir_weight" => {
                        cfg.matching.dir_weight = parse_f64_score(&val, &key, source_label, line_no)?;
                    }
                    "title_weight" => {
                        cfg.matching.title_weight = parse_f64_score(&val, &key, source_label, line_no)?;
                    }
                    "corroboration_boost" => {
                        cfg.matching.corroboration_boost = parse_f64_score(&val, &key, source_label, line_no)?;
                    }
                    "max_secondary_symlinks" => {
                        cfg.matching.max_secondary_symlinks = parse_usize_field(&val, &key, source_label, line_no)?;
                    }
                    "noise_tokens" => {
                        if let Some(list) = parse_inline_yaml_list(v_raw) {
                            cfg.matching.noise_tokens = list
                                .into_iter()
                                .map(|s| s.to_lowercase())
                                .filter(|s| !s.is_empty())
                                .collect();
                        } else if is_yaml_empty_or_null(v_raw) {
                            section = Section::NoiseTokens {
                                matching_indent,
                                key_indent: indent,
                            };
                        }
                    }
                    other => {
                        bail!(
                            "{}:{}: Unknown matching key '{}' in scanner_config.yaml",
                            source_label.display(),
                            line_no,
                            other
                        );
                    }
                },
                Section::Nlp { .. } => match key.as_str() {
                    "enabled" => {
                        cfg.matching.nlp.enabled = parse_bool_field(&val, &key, source_label, line_no)?;
                    }
                    "proper_noun_weight" => {
                        cfg.matching.nlp.proper_noun_weight = parse_f64_score(&val, &key, source_label, line_no)?;
                    }
                    "noun_weight" => {
                        cfg.matching.nlp.noun_weight = parse_f64_score(&val, &key, source_label, line_no)?;
                    }
                    "verb_weight" => {
                        cfg.matching.nlp.verb_weight = parse_f64_score(&val, &key, source_label, line_no)?;
                    }
                    "adjective_weight" => {
                        cfg.matching.nlp.adjective_weight = parse_f64_score(&val, &key, source_label, line_no)?;
                    }
                    "honorific_weight" => {
                        cfg.matching.nlp.honorific_weight = parse_f64_score(&val, &key, source_label, line_no)?;
                    }
                    "function_word_weight" => {
                        cfg.matching.nlp.function_word_weight = parse_f64_score(&val, &key, source_label, line_no)?;
                    }
                    "nlp_influence" => {
                        cfg.matching.nlp.nlp_influence = parse_f64_score(&val, &key, source_label, line_no)?;
                    }
                    "min_effective_score" => {
                        cfg.matching.nlp.min_effective_score = parse_f64_score(&val, &key, source_label, line_no)?;
                    }
                    other => {
                        bail!(
                            "{}:{}: Unknown nlp key '{}' in scanner_config.yaml",
                            source_label.display(),
                            line_no,
                            other
                        );
                    }
                },
                Section::Tools { .. } => match key.as_str() {
                    "python_bin" => {
                        let t = val.trim();
                        if !t.is_empty() && !t.eq_ignore_ascii_case("auto") {
                            cfg.tools.python_bin = t.to_string();
                        }
                    }
                    "dedup_metrics_harness" => {
                        let t = val.trim();
                        if t.is_empty() || t.eq_ignore_ascii_case("auto") {
                            cfg.tools.dedup_metrics_harness = HarnessSetting::Auto;
                        } else if matches!(
                            t.to_lowercase().as_str(),
                            "none" | "null" | "~" | "off" | "false" | "disabled"
                        ) {
                            cfg.tools.dedup_metrics_harness = HarnessSetting::Disabled;
                        } else {
                            cfg.tools.dedup_metrics_harness =
                                HarnessSetting::Explicit(PathBuf::from(t));
                        }
                    }
                    "harness_linger_secs" => {
                        let t = val.trim();
                        if t.is_empty()
                            || matches!(
                                t.to_lowercase().as_str(),
                                "auto" | "default" | "none" | "null" | "~"
                            )
                        {
                            cfg.tools.harness_linger_secs = None;
                        } else {
                            let secs: f64 = t.parse().with_context(|| {
                                format!(
                                    "{}:{}: Expected non-negative float for 'harness_linger_secs', got {:?}",
                                    source_label.display(),
                                    line_no,
                                    t
                                )
                            })?;
                            if secs < 0.0 {
                                bail!(
                                    "{}:{}: 'harness_linger_secs' cannot be negative",
                                    source_label.display(),
                                    line_no
                                );
                            }
                            cfg.tools.harness_linger_secs = Some(secs);
                        }
                    }
                    "rusty_dedup_bin" => {
                        cfg.tools.rusty_dedup_bin = parse_auto_or_path(&val);
                    }
                    "rusty_clean_bin" => {
                        cfg.tools.rusty_clean_bin = parse_auto_or_path(&val);
                    }
                    "dedup_config" => {
                        cfg.tools.dedup_config = parse_auto_or_path(&val);
                    }
                    "decision_config" => {
                        cfg.tools.decision_config = parse_auto_or_path(&val);
                    }
                    "states_dir" => {
                        if !val.is_empty() && !val.eq_ignore_ascii_case("auto") {
                            cfg.tools.states_dir = PathBuf::from(&val);
                        }
                    }
                    other => {
                        bail!(
                            "{}:{}: Unknown tools key '{}' in scanner_config.yaml",
                            source_label.display(),
                            line_no,
                            other
                        );
                    }
                },
                Section::Reports { .. } => match key.as_str() {
                    "enabled" => {
                        cfg.reports.enabled =
                            parse_bool_field(&val, &key, source_label, line_no)?;
                    }
                    "output_dir" => {
                        if !val.is_empty() && !val.eq_ignore_ascii_case("auto") {
                            cfg.reports.output_dir = PathBuf::from(&val);
                        }
                    }
                    "capture_pair_categories_log" => {
                        cfg.reports.capture_pair_categories_log =
                            parse_bool_field(&val, &key, source_label, line_no)?;
                    }
                    other => {
                        bail!(
                            "{}:{}: Unknown reports key '{}' in scanner_config.yaml",
                            source_label.display(),
                            line_no,
                            other
                        );
                    }
                },
                Section::ReportServer { .. } => match key.as_str() {
                    "enabled" => {
                        cfg.report_server.enabled =
                            parse_bool_field(&val, &key, source_label, line_no)?;
                    }
                    "listen_addr" => {
                        let t = val.trim();
                        if !t.is_empty() && !t.eq_ignore_ascii_case("auto") {
                            cfg.report_server.listen_addr = t.to_string();
                        }
                    }
                    "serve_during_run" => {
                        cfg.report_server.serve_during_run =
                            parse_bool_field(&val, &key, source_label, line_no)?;
                    }
                    "linger_after_run_secs" => {
                        cfg.report_server.linger_after_run_secs =
                            parse_usize_field(&val, &key, source_label, line_no)? as u64;
                    }
                    "live_refresh_secs" => {
                        cfg.report_server.live_refresh_secs =
                            parse_usize_field(&val, &key, source_label, line_no)? as u64;
                    }
                    other => {
                        bail!(
                            "{}:{}: Unknown report_server key '{}' in scanner_config.yaml",
                            source_label.display(),
                            line_no,
                            other
                        );
                    }
                },
                _ => {}
            }
        }

        Ok(cfg)
    }

    /// Resolves `rusty_dedup`, `rusty_clean`, `dedup_config.yaml`, `decision_config.yaml`, and `states_dir`
    /// for Live Run mode, verifying required binaries exist on disk.
    pub fn resolve_tools(&self) -> Result<ResolvedTools> {
        let exe_name_suffix = if cfg!(windows) { ".exe" } else { "" };
        let dedup_bin_name = format!("rusty_dedup{}", exe_name_suffix);
        let clean_bin_name = format!("rusty_clean{}", exe_name_suffix);

        let rusty_dedup_bin = match &self.tools.rusty_dedup_bin {
            Some(p) => {
                if !p.is_file() {
                    bail!(
                        "Configured tools.rusty_dedup_bin {:?} does not exist or is not a file.",
                        p
                    );
                }
                normalize_path_lexical(p)
            }
            None => discover_binary(&dedup_bin_name).with_context(|| {
                format!(
                    "Could not auto-discover '{}'. Set tools.rusty_dedup_bin in scanner_config.yaml.",
                    dedup_bin_name
                )
            })?,
        };

        let rusty_clean_bin = match &self.tools.rusty_clean_bin {
            Some(p) => {
                if !p.is_file() {
                    bail!(
                        "Configured tools.rusty_clean_bin {:?} does not exist or is not a file.",
                        p
                    );
                }
                normalize_path_lexical(p)
            }
            None => discover_binary(&clean_bin_name).with_context(|| {
                format!(
                    "Could not auto-discover '{}'. Set tools.rusty_clean_bin in scanner_config.yaml.",
                    clean_bin_name
                )
            })?,
        };

        let dedup_config = match &self.tools.dedup_config {
            Some(p) => {
                if !p.is_file() {
                    bail!(
                        "Configured tools.dedup_config {:?} does not exist.",
                        p
                    );
                }
                Some(normalize_path_lexical(p))
            }
            None => discover_companion_file("dedup_config.yaml", &rusty_dedup_bin),
        };

        let decision_config = match &self.tools.decision_config {
            Some(p) => {
                if !p.is_file() {
                    bail!(
                        "Configured tools.decision_config {:?} does not exist.",
                        p
                    );
                }
                Some(normalize_path_lexical(p))
            }
            None => discover_companion_file("decision_config.yaml", &rusty_clean_bin),
        };

        let states_dir = normalize_path_lexical(&self.tools.states_dir);

        let python_bin = resolve_python_bin(&self.tools.python_bin)?;
        let dedup_metrics_harness = match &self.tools.dedup_metrics_harness {
            HarnessSetting::Disabled => None,
            HarnessSetting::Explicit(p) => {
                if !p.is_file() {
                    bail!(
                        "Configured tools.dedup_metrics_harness {:?} does not exist or is not a file.",
                        p
                    );
                }
                Some(normalize_path_lexical(p))
            }
            HarnessSetting::Auto => {
                let found = discover_metrics_harness(&rusty_dedup_bin).with_context(|| {
                    "Could not auto-discover 'rusty_dedup_metrics_harness.py'. Set tools.dedup_metrics_harness in scanner_config.yaml."
                })?;
                Some(found)
            }
        };

        Ok(ResolvedTools {
            python_bin,
            dedup_metrics_harness,
            harness_linger_secs: self.tools.harness_linger_secs,
            rusty_dedup_bin,
            rusty_clean_bin,
            dedup_config,
            decision_config,
            states_dir,
        })
    }
}

fn resolve_python_bin(configured: &str) -> Result<PathBuf> {
    let p = Path::new(configured);
    if p.components().count() > 1 {
        if !p.is_file() {
            bail!("Configured tools.python_bin {:?} does not exist.", p);
        }
        return Ok(normalize_path_lexical(p));
    }
    if let Some(found) = discover_binary(configured) {
        return Ok(found);
    }
    // Fallback to literal command name (e.g. `python3` on PATH)
    Ok(PathBuf::from(configured))
}

fn discover_metrics_harness(resolved_dedup_bin: &Path) -> Option<PathBuf> {
    let script_name = "rusty_dedup_metrics_harness.py";

    // 1. CWD (`./scripts/...` or `./...`)
    for rel in [format!("scripts/{}", script_name), script_name.to_string()] {
        let p = PathBuf::from(&rel);
        if p.is_file() {
            return Some(normalize_path_lexical(&p));
        }
    }

    // 2. Alongside running `rusty_scanner` / `rusty_triage` binary
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            for cand in [
                exe_dir.join("scripts").join(script_name),
                exe_dir.join(script_name),
            ] {
                if cand.is_file() {
                    return Some(normalize_path_lexical(&cand));
                }
            }
        }
    }

    // 3. Relative to `rusty_dedup` binary (e.g. `<repo>/target/release/rusty_dedup` -> `<repo>/scripts/rusty_dedup_metrics_harness.py`)
    let mut anc = resolved_dedup_bin.parent();
    for _ in 0..4 {
        let Some(dir) = anc else { break };
        for cand in [dir.join("scripts").join(script_name), dir.join(script_name)] {
            if cand.is_file() {
                return Some(normalize_path_lexical(&cand));
            }
        }
        anc = dir.parent();
    }

    // 4. Sibling `rusty_dedup` workspace roots
    for root in sibling_rusty_dedup_roots() {
        let cand = root.join("scripts").join(script_name);
        if cand.is_file() {
            return Some(normalize_path_lexical(&cand));
        }
    }

    None
}

fn parse_auto_or_path(val: &str) -> Option<PathBuf> {
    let t = val.trim();
    if t.is_empty()
        || t.eq_ignore_ascii_case("auto")
        || t.eq_ignore_ascii_case("none")
        || t.eq_ignore_ascii_case("null")
        || t == "~"
    {
        None
    } else {
        Some(PathBuf::from(t))
    }
}

fn parse_f64_score(val: &str, key: &str, src: &Path, line_no: usize) -> Result<f64> {
    let parsed: f64 = val.parse().with_context(|| {
        format!(
            "{}:{}: Expected float between 0.0 and 1.0 for '{}', got {:?}",
            src.display(),
            line_no,
            key,
            val
        )
    })?;
    if !(0.0..=1.0).contains(&parsed) {
        bail!(
            "{}:{}: Score '{}' must be between 0.0 and 1.0, got {}",
            src.display(),
            line_no,
            key,
            parsed
        );
    }
    Ok(parsed)
}

fn parse_usize_field(val: &str, key: &str, src: &Path, line_no: usize) -> Result<usize> {
    val.parse::<usize>().with_context(|| {
        format!(
            "{}:{}: Expected non-negative integer for '{}', got {:?}",
            src.display(),
            line_no,
            key,
            val
        )
    })
}

fn parse_bool_field(val: &str, key: &str, src: &Path, line_no: usize) -> Result<bool> {
    match val.trim().to_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        _ => bail!(
            "{}:{}: Expected boolean (true/false) for '{}', got {:?}",
            src.display(),
            line_no,
            key,
            val
        ),
    }
}

fn sibling_rusty_dedup_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd.join("../rusty_dedup"));
        roots.push(cwd.join("rusty_dedup"));
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut anc = exe.parent();
        while let Some(dir) = anc {
            let cand = dir.join("rusty_dedup");
            if cand.is_dir() {
                roots.push(cand);
            }
            anc = dir.parent();
        }
    }
    roots
}

fn discover_binary(bin_name: &str) -> Option<PathBuf> {
    // 1. Same directory as running `rusty_scanner` binary
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let cand = exe_dir.join(bin_name);
            if cand.is_file() {
                return Some(normalize_path_lexical(&cand));
            }
        }
    }

    // 2. System PATH
    if let Some(path_os) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_os) {
            let cand = dir.join(bin_name);
            if cand.is_file() {
                return Some(normalize_path_lexical(&cand));
            }
        }
    }

    // 3. Sibling `rusty_dedup` workspace (`target/release/` or `target/debug/`)
    for root in sibling_rusty_dedup_roots() {
        for sub in ["target/release", "target/debug"] {
            let cand = root.join(sub).join(bin_name);
            if cand.is_file() {
                return Some(normalize_path_lexical(&cand));
            }
        }
    }

    None
}

fn discover_companion_file(file_name: &str, resolved_bin: &Path) -> Option<PathBuf> {
    // 1. Current working directory
    let cwd_cand = PathBuf::from(file_name);
    if cwd_cand.is_file() {
        return Some(normalize_path_lexical(&cwd_cand));
    }

    // 2. Same directory as `rusty_scanner` binary
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let cand = exe_dir.join(file_name);
            if cand.is_file() {
                return Some(normalize_path_lexical(&cand));
            }
        }
    }

    // 3. Same directory as the resolved `rusty_dedup` / `rusty_clean` binary
    if let Some(bin_dir) = resolved_bin.parent() {
        let cand = bin_dir.join(file_name);
        if cand.is_file() {
            return Some(normalize_path_lexical(&cand));
        }
    }

    // 4. Sibling `rusty_dedup` workspace root
    for root in sibling_rusty_dedup_roots() {
        let cand = root.join(file_name);
        if cand.is_file() {
            return Some(normalize_path_lexical(&cand));
        }
    }

    None
}

/// Infers common parent root directories from configured anchors (e.g. `["/Volumes/f0m/f", "/Volumes/f1m/f"]`),
/// sorted by descending anchor count so the `n` (New Anchor) modal offers them as one-keystroke choices.
pub fn infer_anchor_root_volumes(anchors: &[AnchorEntry]) -> Vec<PathBuf> {
    let mut counts: HashMap<PathBuf, usize> = HashMap::new();
    for anc in anchors {
        if let Some(parent) = anc.anchor_path.parent() {
            if !parent.as_os_str().is_empty() {
                *counts.entry(parent.to_path_buf()).or_insert(0) += 1;
            }
        }
    }
    let mut pairs: Vec<(PathBuf, usize)> = counts.into_iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    pairs.into_iter().map(|(p, _)| p).collect()
}

fn format_yaml_quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Atomically appends `new_alias` to the anchor matching `target_anchor_path` inside `yaml_path`,
/// preserving all surrounding comments and formatting. Returns `Ok(true)` if added, `Ok(false)` if
/// the alias was already present on that anchor.
pub fn append_alias_to_anchor_yaml(
    yaml_path: &Path,
    target_anchor_path: &Path,
    new_alias: &str,
) -> Result<bool> {
    let alias_trimmed = new_alias.trim();
    if alias_trimmed.is_empty() {
        bail!("Alias cannot be empty.");
    }

    let existing_cfg = AnchorDirsConfig::load(yaml_path)?;
    let target_norm = normalize_path_lexical(target_anchor_path);
    let Some(target_entry) = existing_cfg
        .anchors
        .iter()
        .find(|a| a.anchor_path == target_norm)
    else {
        bail!(
            "Anchor {:?} not found in {:?}",
            target_anchor_path,
            yaml_path
        );
    };
    if target_entry
        .aliases
        .iter()
        .any(|a| a.eq_ignore_ascii_case(alias_trimmed))
    {
        return Ok(false);
    }

    let raw_content = fs::read_to_string(yaml_path)
        .with_context(|| format!("Failed to read {:?}", yaml_path))?;
    let mut lines: Vec<String> = raw_content.lines().map(|l| l.to_string()).collect();

    // Locate the anchor map key line matching `target_norm`
    let mut anchor_line_idx: Option<usize> = None;
    let mut anchor_indent = 0usize;

    for (idx, line) in lines.iter().enumerate() {
        let stripped = strip_yaml_comment(line);
        let trimmed = stripped.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((k_raw, _)) = split_yaml_key_value(trimmed) {
            let k_unq = unquote_yaml_scalar(k_raw);
            let k_path = Path::new(&k_unq);
            if k_path.is_absolute() && normalize_path_lexical(k_path) == target_norm {
                anchor_line_idx = Some(idx);
                anchor_indent = stripped.chars().take_while(|c| c.is_whitespace()).count();
                break;
            }
        }
    }

    let Some(a_idx) = anchor_line_idx else {
        bail!(
            "Could not locate map key for anchor {:?} in {:?}",
            target_anchor_path,
            yaml_path
        );
    };

    let child_indent_str = " ".repeat(anchor_indent + 2);
    let list_item_indent_str = " ".repeat(anchor_indent + 4);
    let quoted_alias = format_yaml_quoted(alias_trimmed);

    // Check if the anchor line itself has `{}` or inline `[...]`
    let anchor_stripped = strip_yaml_comment(&lines[a_idx]).to_string();
    if let Some((k_raw, v_raw)) = split_yaml_key_value(anchor_stripped.trim()) {
        if v_raw.trim() == "{}" {
            let prefix_ws = " ".repeat(anchor_indent);
            lines[a_idx] = format!("{}{}:", prefix_ws, k_raw.trim());
            lines.insert(
                a_idx + 1,
                format!("{}aliases:\n{}- {}", child_indent_str, list_item_indent_str, quoted_alias),
            );
            write_atomic_lines(yaml_path, &lines)?;
            return Ok(true);
        }
        if let Some(mut inline_items) = parse_inline_yaml_list(v_raw) {
            inline_items.push(alias_trimmed.to_string());
            let formatted_items: Vec<String> =
                inline_items.iter().map(|s| format_yaml_quoted(s)).collect();
            let prefix_ws = " ".repeat(anchor_indent);
            lines[a_idx] = format!(
                "{}{}: [{}]",
                prefix_ws,
                k_raw.trim(),
                formatted_items.join(", ")
            );
            write_atomic_lines(yaml_path, &lines)?;
            return Ok(true);
        }
    }

    // Scan child lines under `a_idx` for an existing `aliases:` key
    let mut aliases_line_idx: Option<usize> = None;
    let mut scan_idx = a_idx + 1;
    while scan_idx < lines.len() {
        let stripped = strip_yaml_comment(&lines[scan_idx]);
        let trimmed = stripped.trim();
        if trimmed.is_empty() {
            scan_idx += 1;
            continue;
        }
        let ind = stripped.chars().take_while(|c| c.is_whitespace()).count();
        if ind <= anchor_indent {
            break;
        }
        if let Some((ck, _)) = split_yaml_key_value(trimmed) {
            let ck_clean = unquote_yaml_scalar(ck).to_lowercase();
            if ck_clean == "aliases" || ck_clean == "alias" {
                aliases_line_idx = Some(scan_idx);
                break;
            }
        }
        scan_idx += 1;
    }

    if let Some(al_idx) = aliases_line_idx {
        let al_stripped = strip_yaml_comment(&lines[al_idx]).to_string();
        let al_indent = al_stripped.chars().take_while(|c| c.is_whitespace()).count();
        let al_indent_str = " ".repeat(al_indent);
        let item_indent_str = " ".repeat(al_indent + 2);

        if let Some((ck_raw, cv_raw)) = split_yaml_key_value(al_stripped.trim()) {
            if let Some(mut inline_items) = parse_inline_yaml_list(cv_raw) {
                inline_items.push(alias_trimmed.to_string());
                let formatted_items: Vec<String> =
                    inline_items.iter().map(|s| format_yaml_quoted(s)).collect();
                lines[al_idx] = format!(
                    "{}{}: [{}]",
                    al_indent_str,
                    ck_raw.trim(),
                    formatted_items.join(", ")
                );
            } else if is_yaml_empty_or_null(cv_raw) {
                lines.insert(al_idx + 1, format!("{}- {}", item_indent_str, quoted_alias));
            } else {
                let prev_single = unquote_yaml_scalar(cv_raw);
                lines[al_idx] = format!("{}{}:", al_indent_str, ck_raw.trim());
                lines.insert(
                    al_idx + 1,
                    format!(
                        "{}- {}\n{}- {}",
                        item_indent_str,
                        format_yaml_quoted(&prev_single),
                        item_indent_str,
                        quoted_alias
                    ),
                );
            }
        }
    } else {
        // No `aliases:` key under this anchor yet -> insert right beneath the anchor key
        lines.insert(
            a_idx + 1,
            format!(
                "{}aliases:\n{}- {}",
                child_indent_str, list_item_indent_str, quoted_alias
            ),
        );
    }

    write_atomic_lines(yaml_path, &lines)?;
    Ok(true)
}

/// Atomically appends a brand-new anchor directory entry (and optional initial aliases) to `yaml_path`.
pub fn append_new_anchor_to_yaml(
    yaml_path: &Path,
    new_anchor_dir: &Path,
    aliases: &[String],
) -> Result<()> {
    if !new_anchor_dir.is_absolute() {
        bail!(
            "New anchor path {:?} must be an absolute path.",
            new_anchor_dir
        );
    }

    let existing_cfg = AnchorDirsConfig::load(yaml_path)?;
    let norm_new = normalize_path_lexical(new_anchor_dir);
    if existing_cfg.anchors.iter().any(|a| a.anchor_path == norm_new) {
        bail!(
            "Anchor {:?} already exists in {:?}",
            norm_new,
            yaml_path
        );
    }

    let raw_content = fs::read_to_string(yaml_path)
        .with_context(|| format!("Failed to read {:?}", yaml_path))?;
    let has_anchors_section = raw_content.lines().any(|l| {
        let t = strip_yaml_comment(l).trim();
        t == "anchors:" || t.starts_with("anchors:")
    });
    let entry_indent = if has_anchors_section { "  " } else { "" };
    let field_indent = format!("{}  ", entry_indent);

    let mut lines: Vec<String> = raw_content.lines().map(|l| l.to_string()).collect();
    let key_str = format_yaml_quoted(&norm_new.to_string_lossy());
    lines.push(format!("{}{}:", entry_indent, key_str));

    let clean_aliases: Vec<String> = aliases
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(format_yaml_quoted)
        .collect();
    if clean_aliases.is_empty() {
        lines.push(format!("{}aliases: []", field_indent));
    } else {
        lines.push(format!(
            "{}aliases: [{}]",
            field_indent,
            clean_aliases.join(", ")
        ));
    }

    write_atomic_lines(yaml_path, &lines)?;
    Ok(())
}

fn write_atomic_lines(target: &Path, lines: &[String]) -> Result<()> {
    let fname = target
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("anchors.yaml");
    let tmp = target.with_file_name(format!(".{}.tmp.{}", fname, std::process::id()));
    let mut payload = lines.join("\n");
    payload.push('\n');
    fs::write(&tmp, payload.as_bytes())
        .with_context(|| format!("Failed writing temp YAML {:?}", tmp))?;
    fs::rename(&tmp, target)
        .with_context(|| format!("Failed renaming {:?} -> {:?}", tmp, target))?;
    Ok(())
}
