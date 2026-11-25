use crate::config::{AnchorEntry, normalize_path_lexical, path_dev_inode};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::{DirEntry, WalkDir};

/// Prefix used for temporary sibling holding directories created outside `misc_dir` during `rusty_clean`.
pub const TEMP_HOLD_PREFIX: &str = ".rusty_scanner_hold_";

/// A discovered miscellaneous video file candidate ready for anchor matching.
#[derive(Debug, Clone)]
pub struct MiscVideoCandidate {
    /// Absolute path to the video file on disk.
    pub path: PathBuf,
    /// `(dev, inode)` tuple used to deduplicate overlapping input paths.
    pub dev_inode: (u64, u64),
    /// File size in bytes.
    pub size_bytes: u64,
    /// When discovered via a directory argument, holds that directory's normalized root path
    /// so relative subdirectories beneath it can be inspected during matching.
    pub misc_root: Option<PathBuf>,
}

/// Checks if `path` has a supported video extension (case-insensitive).
pub fn is_supported_video_file(path: &Path, video_extensions: &HashSet<String>) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| video_extensions.contains(&ext.to_lowercase()))
        .unwrap_or(false)
}

fn is_ignored_dir_entry(entry: &DirEntry) -> bool {
    if entry.depth() == 0 {
        return false;
    }
    let name = entry.file_name().to_string_lossy();
    if name.starts_with(TEMP_HOLD_PREFIX) || name == ".states" || name.starts_with('.') {
        return entry.file_type().is_dir();
    }
    false
}

/// Discovers all candidate video files across user-passed `<INPUTS>...` (mix of directories and/or files).
///
/// - Deduplicates overlapping paths by `(dev, inode)`.
/// - Excludes symlinks (`entry.file_type().is_file()` with `follow_links(false)`), zero-byte files,
///   quarantined paths, and any file already located inside an anchor root or anchor destination directory.
pub fn discover_misc_videos(
    inputs: &[PathBuf],
    anchors: &[AnchorEntry],
    video_extensions: &HashSet<String>,
    quarantined_paths: &HashSet<PathBuf>,
    is_first_pass: bool,
    verbose: bool,
) -> Vec<MiscVideoCandidate> {
    // Build exclusion prefixes for all anchor roots & custom destinations so we never treat an
    // existing anchor video as a misc candidate.
    let mut protected_trees: Vec<PathBuf> = Vec::with_capacity(anchors.len() * 3);
    for a in anchors {
        protected_trees.push(a.anchor_path.clone());
        if a.canonical_anchor_path != a.anchor_path {
            protected_trees.push(a.canonical_anchor_path.clone());
        }
        protected_trees.push(a.destination_path.clone());
        if let Ok(can_dest) = fs::canonicalize(&a.destination_path) {
            if can_dest != a.destination_path {
                protected_trees.push(can_dest);
            }
        }
    }

    let is_inside_anchor = |p: &Path| -> bool {
        let lex = normalize_path_lexical(p);
        if protected_trees.iter().any(|root| lex.starts_with(root)) {
            return true;
        }
        if let Ok(can) = fs::canonicalize(p) {
            if protected_trees.iter().any(|root| can.starts_with(root)) {
                return true;
            }
        }
        false
    };

    let mut seen_inodes: HashSet<(u64, u64)> = HashSet::new();
    let mut candidates: Vec<MiscVideoCandidate> = Vec::new();

    for raw_input in inputs {
        let Ok(symlink_meta) = fs::symlink_metadata(raw_input) else {
            if is_first_pass {
                eprintln!(
                    "Warning: Input path {:?} does not exist or cannot be accessed, skipping.",
                    raw_input
                );
            }
            continue;
        };

        let norm_input = normalize_path_lexical(raw_input);

        if symlink_meta.file_type().is_symlink() {
            if verbose && is_first_pass {
                eprintln!(
                    "[Verbose][Scan] Skipping symlink input {:?} (only regular video files are processed).",
                    norm_input
                );
            }
            continue;
        }

        if symlink_meta.is_file() {
            if !is_supported_video_file(&norm_input, video_extensions) {
                if is_first_pass {
                    eprintln!(
                        "Warning: Explicit input file {:?} does not have a configured video extension, skipping.",
                        norm_input
                    );
                }
                continue;
            }
            let size = symlink_meta.len();
            if size == 0 || quarantined_paths.contains(&norm_input) {
                continue;
            }
            if is_inside_anchor(&norm_input) {
                if verbose && is_first_pass {
                    eprintln!(
                        "[Verbose][Scan] Skipping {:?} because it is already inside an anchor directory.",
                        norm_input
                    );
                }
                continue;
            }
            let dev_ino = path_dev_inode(&norm_input, &symlink_meta);
            if seen_inodes.insert(dev_ino) {
                candidates.push(MiscVideoCandidate {
                    path: norm_input,
                    dev_inode: dev_ino,
                    size_bytes: size,
                    misc_root: None,
                });
            }
            continue;
        }

        if symlink_meta.is_dir() {
            if is_inside_anchor(&norm_input) {
                if is_first_pass {
                    eprintln!(
                        "Warning: Misc directory {:?} is inside an anchor directory, skipping.",
                        norm_input
                    );
                }
                continue;
            }

            if verbose {
                eprintln!(
                    "[Verbose][Scan] Walking misc directory recursively: {:?}",
                    norm_input
                );
            }

            for entry in WalkDir::new(&norm_input)
                .follow_links(false)
                .into_iter()
                .filter_entry(|e| !is_ignored_dir_entry(e))
                .filter_map(|e| e.ok())
            {
                if !entry.file_type().is_file() {
                    continue;
                }
                let entry_path = normalize_path_lexical(entry.path());
                if !is_supported_video_file(&entry_path, video_extensions) {
                    continue;
                }
                if quarantined_paths.contains(&entry_path) {
                    continue;
                }
                if is_inside_anchor(&entry_path) {
                    if verbose && is_first_pass {
                        eprintln!(
                            "  [Verbose][Scan] Skipping file inside anchor subtree: {:?}",
                            entry_path
                        );
                    }
                    continue;
                }
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                let size = meta.len();
                if size == 0 {
                    continue;
                }
                let dev_ino = path_dev_inode(&entry_path, &meta);
                if seen_inodes.insert(dev_ino) {
                    candidates.push(MiscVideoCandidate {
                        path: entry_path,
                        dev_inode: dev_ino,
                        size_bytes: size,
                        misc_root: Some(norm_input.clone()),
                    });
                }
            }
        }
    }

    candidates.sort_by(|a, b| a.path.cmp(&b.path));
    candidates
}
