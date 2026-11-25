use crate::config::{
    AnchorDirsConfig, ScannerConfig, append_alias_to_anchor_yaml, append_new_anchor_to_yaml,
    infer_anchor_root_volumes, normalize_path_lexical, path_dev_inode,
};
use crate::csv_report::{read_matches_csv, write_matches_csv, write_unmatched_files};
use crate::matcher::{
    AnchorMatcher, BatchMatchOutcome, FileMatchResult, MatchTier, PlannedAction, RankedAnchorMatch,
};
use crate::scanner::MiscVideoCandidate;
use crate::suggestions::{AnchorSuggestion, SuggestionEngine, extract_candidate_creator_label};
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Active focus pane in `rusty_triage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivePane {
    UnmatchedLeft,
    AnchorsRight,
}

/// View layout for the Left (Unmatched) pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftViewMode {
    TreeByParentDir,
    FlatFiles,
}

/// Active input field inside the `n` (New Anchor) modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewAnchorField {
    FolderName,
    InitialAlias,
}

/// Modal overlay currently open on top of the two-pane view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalState {
    None,
    /// `/` filter bar for unmatched folders/files in the Left Pane.
    LeftFilterInput {
        buf: String,
    },
    /// `a` modal: append a new alias to `anchor_idx` in `anchors.yaml` and auto-sweep remaining items.
    AddAlias {
        anchor_idx: usize,
        alias_buf: String,
    },
    /// `n` modal: create a new anchor directory under a chosen root volume, add to `anchors.yaml`, and assign.
    NewAnchor {
        root_choices: Vec<PathBuf>,
        selected_root_idx: usize,
        folder_name_buf: String,
        alias_buf: String,
        active_field: NewAnchorField,
    },
    /// Safety confirmation when pressing `D` on a file inside a top-level shared dump directory
    /// that contains multiple distinct subdirectories or > 30 files.
    ConfirmWholeParentDir {
        parent_dir: PathBuf,
        file_indices: Vec<usize>,
        anchor_idx: usize,
    },
    /// `X` confirmation before launching live `rusty_dedup` + `rusty_clean` + move execution.
    ConfirmExecute {
        assigned_count: usize,
    },
    /// `q` confirmation when unsaved staged assignments exist.
    ConfirmQuitUnsaved {
        assigned_count: usize,
    },
}

/// A visible row in the Left Pane (either a collapsible parent directory header or an individual video file).
#[derive(Debug, Clone)]
pub enum VisibleLeftRow {
    DirHeader {
        group_idx: usize,
        parent_path: PathBuf,
        display_label: String,
        expanded: bool,
        total_files: usize,
        assigned_files: usize,
        selected_files: usize,
    },
    FileItem {
        file_idx: usize,
        indented: bool,
        display_label: String,
        assigned_anchor: Option<usize>,
        selected: bool,
    },
}

#[derive(Debug, Clone)]
pub struct ParentDirGroup {
    pub parent_path: PathBuf,
    pub display_label: String,
    pub file_indices: Vec<usize>,
    pub expanded: bool,
}

#[derive(Debug, Clone)]
struct UndoEntry {
    description: String,
    previous_assignments: Vec<(usize, Option<usize>)>,
}

/// Exit outcome returned by the TUI loop to `rusty_triage::main`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiExitOutcome {
    QuitOnly,
    ExecuteNow,
}

/// Complete headless state machine for `rusty_triage` (unit-testable without a TTY).
pub struct TriageApp {
    pub anchors_yaml_path: PathBuf,
    pub output_csv_path: PathBuf,
    pub unmatched_out_path: Option<PathBuf>,
    pub anchor_cfg: AnchorDirsConfig,
    pub scanner_cfg: ScannerConfig,
    pub matcher: AnchorMatcher,
    pub suggestion_engine: SuggestionEngine,

    /// Master list of unmatched video file paths loaded at session start.
    pub files: Vec<PathBuf>,
    /// Common prefix across `files` stripped from Left Pane labels to maximize readable width.
    pub common_prefix: Option<PathBuf>,
    /// Grouped parent directory nodes for `LeftViewMode::TreeByParentDir`.
    pub dir_groups: Vec<ParentDirGroup>,
    /// Current staged primary anchor assignment (`Some(anchor_idx)`) per `file_idx`.
    pub assignments: Vec<Option<usize>>,
    /// Multi-selected `file_idx` set toggled via `Space`.
    pub multi_selected: HashSet<usize>,

    pub active_pane: ActivePane,
    pub left_view_mode: LeftViewMode,
    pub hide_assigned: bool,
    pub left_filter: String,
    pub anchor_search: String,

    pub visible_left_rows: Vec<VisibleLeftRow>,
    pub left_cursor: usize,
    pub filtered_anchor_indices: Vec<usize>,
    pub right_cursor: usize,
    pub current_suggestions: Vec<AnchorSuggestion>,

    pub modal: ModalState,
    undo_stack: Vec<UndoEntry>,
    pub dirty: bool,
    pub status_message: String,
}

impl TriageApp {
    pub fn new(
        anchors_yaml_path: PathBuf,
        output_csv_path: PathBuf,
        unmatched_out_path: Option<PathBuf>,
        anchor_cfg: AnchorDirsConfig,
        scanner_cfg: ScannerConfig,
        mut initial_files: Vec<PathBuf>,
    ) -> Result<Self> {
        for p in &mut initial_files {
            *p = normalize_path_lexical(p);
        }
        initial_files.sort();
        initial_files.dedup();

        let common_prefix = compute_common_parent_prefix(&initial_files);
        let dir_groups = build_parent_dir_groups(&initial_files, common_prefix.as_deref());
        let file_count = initial_files.len();

        let matcher = AnchorMatcher::new(&anchor_cfg.anchors, scanner_cfg.matching.clone());
        let suggestion_engine =
            SuggestionEngine::new(&anchor_cfg.anchors, &scanner_cfg.matching.noise_tokens);

        let mut app = Self {
            anchors_yaml_path,
            output_csv_path: output_csv_path.clone(),
            unmatched_out_path,
            anchor_cfg,
            scanner_cfg,
            matcher,
            suggestion_engine,
            files: initial_files,
            common_prefix,
            dir_groups,
            assignments: vec![None; file_count],
            multi_selected: HashSet::new(),
            active_pane: ActivePane::UnmatchedLeft,
            left_view_mode: LeftViewMode::TreeByParentDir,
            hide_assigned: false,
            left_filter: String::new(),
            anchor_search: String::new(),
            visible_left_rows: Vec::new(),
            left_cursor: 0,
            filtered_anchor_indices: Vec::new(),
            right_cursor: 0,
            current_suggestions: Vec::new(),
            modal: ModalState::None,
            undo_stack: Vec::new(),
            dirty: false,
            status_message: String::from(
                "Ready — [1-5] Quick Suggest | [Enter] Assign | [D] Whole Parent Dir | [a] Add Alias | [n] New Anchor",
            ),
        };

        // Resume any previously saved assignments in `output_csv_path` if it already exists
        if output_csv_path.is_file() {
            if let Ok(prior) = read_matches_csv(&output_csv_path, &app.anchor_cfg) {
                let mut file_index_by_path: HashMap<PathBuf, usize> = HashMap::new();
                for (i, fp) in app.files.iter().enumerate() {
                    file_index_by_path.insert(fp.clone(), i);
                }
                let mut resumed = 0usize;
                for item in prior {
                    if let (Some(&f_idx), Some(prim)) =
                        (file_index_by_path.get(&item.candidate.path), item.matches.first())
                    {
                        app.assignments[f_idx] = Some(prim.anchor_idx);
                        resumed += 1;
                    }
                }
                if resumed > 0 {
                    app.status_message = format!(
                        "Resumed {} prior assignment(s) from {:?}",
                        resumed, output_csv_path
                    );
                }
            }
        }

        app.rebuild_visible_left();
        app.rebuild_filtered_anchors();
        app.refresh_suggestions();
        Ok(app)
    }

    /// Loads unmatched video file paths from a newline-delimited file (`un.txt` / `misc_files.txt`).
    pub fn load_unmatched_list_file(list_path: &Path) -> Result<Vec<PathBuf>> {
        let content = fs::read_to_string(list_path)
            .with_context(|| format!("Failed to read unmatched list file {:?}", list_path))?;
        let paths: Vec<PathBuf> = content
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(PathBuf::from)
            .collect();
        Ok(paths)
    }

    pub fn total_files(&self) -> usize {
        self.files.len()
    }

    pub fn assigned_count(&self) -> usize {
        self.assignments.iter().filter(|a| a.is_some()).count()
    }

    pub fn unassigned_count(&self) -> usize {
        self.total_files().saturating_sub(self.assigned_count())
    }

    pub fn undo_depth(&self) -> usize {
        self.undo_stack.len()
    }

    /// Returns the primary path currently highlighted in the Left Pane (directory path or file path).
    pub fn highlighted_left_path(&self) -> Option<PathBuf> {
        match self.visible_left_rows.get(self.left_cursor)? {
            VisibleLeftRow::DirHeader { parent_path, .. } => Some(parent_path.clone()),
            VisibleLeftRow::FileItem { file_idx, .. } => self.files.get(*file_idx).cloned(),
        }
    }

    /// Returns the anchor index currently highlighted in the Right Pane.
    pub fn highlighted_anchor_idx(&self) -> Option<usize> {
        self.filtered_anchor_indices.get(self.right_cursor).copied()
    }

    pub fn rebuild_visible_left(&mut self) {
        let filter_lower = self.left_filter.trim().to_lowercase();
        let mut rows = Vec::new();

        match self.left_view_mode {
            LeftViewMode::TreeByParentDir => {
                for (g_idx, grp) in self.dir_groups.iter().enumerate() {
                    let mut matching_files: Vec<usize> = Vec::new();
                    let dir_matches_filter = filter_lower.is_empty()
                        || grp.display_label.to_lowercase().contains(&filter_lower);

                    for &f_idx in &grp.file_indices {
                        if self.hide_assigned && self.assignments[f_idx].is_some() {
                            continue;
                        }
                        if !dir_matches_filter {
                            let fname_lower = self.files[f_idx]
                                .file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("")
                                .to_lowercase();
                            if !fname_lower.contains(&filter_lower) {
                                continue;
                            }
                        }
                        matching_files.push(f_idx);
                    }

                    if matching_files.is_empty() {
                        continue;
                    }

                    let total_in_grp = grp.file_indices.len();
                    let assigned_in_grp = grp
                        .file_indices
                        .iter()
                        .filter(|&&fi| self.assignments[fi].is_some())
                        .count();
                    let selected_in_grp = grp
                        .file_indices
                        .iter()
                        .filter(|fi| self.multi_selected.contains(fi))
                        .count();

                    rows.push(VisibleLeftRow::DirHeader {
                        group_idx: g_idx,
                        parent_path: grp.parent_path.clone(),
                        display_label: grp.display_label.clone(),
                        expanded: grp.expanded,
                        total_files: total_in_grp,
                        assigned_files: assigned_in_grp,
                        selected_files: selected_in_grp,
                    });

                    if grp.expanded {
                        for f_idx in matching_files {
                            let fname = self.files[f_idx]
                                .file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("")
                                .to_string();
                            rows.push(VisibleLeftRow::FileItem {
                                file_idx: f_idx,
                                indented: true,
                                display_label: fname,
                                assigned_anchor: self.assignments[f_idx],
                                selected: self.multi_selected.contains(&f_idx),
                            });
                        }
                    }
                }
            }
            LeftViewMode::FlatFiles => {
                for (f_idx, fpath) in self.files.iter().enumerate() {
                    if self.hide_assigned && self.assignments[f_idx].is_some() {
                        continue;
                    }
                    let label = relative_display_path(fpath, self.common_prefix.as_deref());
                    if !filter_lower.is_empty() && !label.to_lowercase().contains(&filter_lower) {
                        continue;
                    }
                    rows.push(VisibleLeftRow::FileItem {
                        file_idx: f_idx,
                        indented: false,
                        display_label: label,
                        assigned_anchor: self.assignments[f_idx],
                        selected: self.multi_selected.contains(&f_idx),
                    });
                }
            }
        }

        self.visible_left_rows = rows;
        if self.left_cursor >= self.visible_left_rows.len() {
            self.left_cursor = self.visible_left_rows.len().saturating_sub(1);
        }
    }

    pub fn rebuild_filtered_anchors(&mut self) {
        let q = self.anchor_search.trim().to_lowercase();
        if q.is_empty() {
            self.filtered_anchor_indices = (0..self.anchor_cfg.anchors.len()).collect();
        } else {
            let query_tokens: Vec<&str> = q.split_whitespace().collect();
            let mut scored: Vec<(i32, usize)> = Vec::new();

            for (idx, anc) in self.anchor_cfg.anchors.iter().enumerate() {
                let base = anc
                    .anchor_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                let full = anc.anchor_path.to_string_lossy().to_lowercase();
                let aliases_lower: Vec<String> =
                    anc.aliases.iter().map(|a| a.to_lowercase()).collect();

                let all_tokens_match = query_tokens.iter().all(|qt| {
                    base.contains(qt)
                        || full.contains(qt)
                        || aliases_lower.iter().any(|al| al.contains(qt))
                });

                if all_tokens_match {
                    let priority = if base == q || aliases_lower.iter().any(|al| al == &q) {
                        400
                    } else if base.starts_with(&q)
                        || aliases_lower.iter().any(|al| al.starts_with(&q))
                    {
                        300
                    } else if base.contains(&q) {
                        200
                    } else {
                        100
                    };
                    scored.push((priority, idx));
                }
            }

            scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            self.filtered_anchor_indices = scored.into_iter().map(|(_, idx)| idx).collect();
        }

        if self.right_cursor >= self.filtered_anchor_indices.len() {
            self.right_cursor = self.filtered_anchor_indices.len().saturating_sub(1);
        }
    }

    pub fn refresh_suggestions(&mut self) {
        let target_path = match self.visible_left_rows.get(self.left_cursor) {
            Some(VisibleLeftRow::FileItem { file_idx, .. }) => self.files.get(*file_idx).cloned(),
            Some(VisibleLeftRow::DirHeader {
                group_idx,
                parent_path,
                ..
            }) => self
                .dir_groups
                .get(*group_idx)
                .and_then(|g| g.file_indices.first().map(|&fi| self.files[fi].clone()))
                .or_else(|| Some(parent_path.clone())),
            None => None,
        };

        self.current_suggestions = match target_path {
            Some(p) => self.suggestion_engine.top_suggestions(&p, 5),
            None => Vec::new(),
        };
    }

    pub fn move_cursor_up(&mut self, step: usize) {
        match self.active_pane {
            ActivePane::UnmatchedLeft => {
                self.left_cursor = self.left_cursor.saturating_sub(step);
                self.refresh_suggestions();
            }
            ActivePane::AnchorsRight => {
                self.right_cursor = self.right_cursor.saturating_sub(step);
            }
        }
    }

    pub fn move_cursor_down(&mut self, step: usize) {
        match self.active_pane {
            ActivePane::UnmatchedLeft => {
                if !self.visible_left_rows.is_empty() {
                    self.left_cursor =
                        (self.left_cursor + step).min(self.visible_left_rows.len() - 1);
                    self.refresh_suggestions();
                }
            }
            ActivePane::AnchorsRight => {
                if !self.filtered_anchor_indices.is_empty() {
                    self.right_cursor =
                        (self.right_cursor + step).min(self.filtered_anchor_indices.len() - 1);
                }
            }
        }
    }

    pub fn set_expand_current_dir(&mut self, expanded: bool) {
        if let Some(VisibleLeftRow::DirHeader { group_idx, .. }) =
            self.visible_left_rows.get(self.left_cursor).cloned()
        {
            if let Some(grp) = self.dir_groups.get_mut(group_idx) {
                grp.expanded = expanded;
                self.rebuild_visible_left();
            }
        }
    }

    pub fn toggle_expand_current_dir(&mut self) {
        if let Some(VisibleLeftRow::DirHeader {
            group_idx,
            expanded,
            ..
        }) = self.visible_left_rows.get(self.left_cursor).cloned()
        {
            if let Some(grp) = self.dir_groups.get_mut(group_idx) {
                grp.expanded = !expanded;
                self.rebuild_visible_left();
            }
        }
    }

    pub fn toggle_left_view_mode(&mut self) {
        self.left_view_mode = match self.left_view_mode {
            LeftViewMode::TreeByParentDir => LeftViewMode::FlatFiles,
            LeftViewMode::FlatFiles => LeftViewMode::TreeByParentDir,
        };
        self.rebuild_visible_left();
        self.refresh_suggestions();
    }

    pub fn toggle_hide_assigned(&mut self) {
        self.hide_assigned = !self.hide_assigned;
        self.rebuild_visible_left();
        self.refresh_suggestions();
        self.status_message = if self.hide_assigned {
            "Hiding already-assigned items (press H to show all).".to_string()
        } else {
            "Showing all items including assigned.".to_string()
        };
    }

    pub fn toggle_multi_select_current(&mut self) {
        match self.visible_left_rows.get(self.left_cursor).cloned() {
            Some(VisibleLeftRow::FileItem { file_idx, .. }) => {
                if !self.multi_selected.remove(&file_idx) {
                    self.multi_selected.insert(file_idx);
                }
            }
            Some(VisibleLeftRow::DirHeader { group_idx, .. }) => {
                if let Some(grp) = self.dir_groups.get(group_idx) {
                    let all_selected = grp
                        .file_indices
                        .iter()
                        .all(|fi| self.multi_selected.contains(fi));
                    if all_selected {
                        for fi in &grp.file_indices {
                            self.multi_selected.remove(fi);
                        }
                    } else {
                        for &fi in &grp.file_indices {
                            self.multi_selected.insert(fi);
                        }
                    }
                }
            }
            None => return,
        }
        let prev_cursor = self.left_cursor;
        self.rebuild_visible_left();
        if !self.visible_left_rows.is_empty() {
            self.left_cursor = (prev_cursor + 1).min(self.visible_left_rows.len() - 1);
            self.refresh_suggestions();
        }
    }

    /// Collects the target `file_indices` that an `Enter` or `[1-5]` assignment operates on:
    /// 1. If `multi_selected` is non-empty -> all multi-selected files (then clears `multi_selected`).
    /// 2. Else if cursor is on a `DirHeader` -> all files in that directory group!
    /// 3. Else if cursor is on a `FileItem` -> that single file.
    fn target_file_indices_for_current_selection(&mut self) -> Vec<usize> {
        if !self.multi_selected.is_empty() {
            let mut indices: Vec<usize> = self.multi_selected.drain().collect();
            indices.sort_unstable();
            return indices;
        }
        match self.visible_left_rows.get(self.left_cursor) {
            Some(VisibleLeftRow::DirHeader { group_idx, .. }) => self
                .dir_groups
                .get(*group_idx)
                .map(|g| g.file_indices.clone())
                .unwrap_or_default(),
            Some(VisibleLeftRow::FileItem { file_idx, .. }) => vec![*file_idx],
            None => Vec::new(),
        }
    }

    /// Assigns the current selection (file, directory node, or multi-selected set) to `anchor_idx`.
    pub fn assign_current_to_anchor(&mut self, anchor_idx: usize) {
        if anchor_idx >= self.anchor_cfg.anchors.len() {
            return;
        }
        let targets = self.target_file_indices_for_current_selection();
        if targets.is_empty() {
            self.status_message = "No unmatched item selected to assign.".to_string();
            return;
        }
        self.apply_assignment_batch(&targets, Some(anchor_idx), "Assigned");
        self.advance_to_next_unassigned();
    }

    /// Quick-assigns using `[1]`–`[5]` from `current_suggestions`.
    pub fn assign_to_suggestion_slot(&mut self, one_based_slot: usize) {
        let Some(sugg) = self
            .current_suggestions
            .get(one_based_slot.saturating_sub(1))
            .cloned()
        else {
            self.status_message = format!("Suggestion [{}] is not available.", one_based_slot);
            return;
        };
        self.assign_current_to_anchor(sugg.anchor_idx);
    }

    /// `D` hotkey: Assigns the **entire parent directory** of the currently highlighted item to `anchor_idx`.
    /// Prompts for confirmation if the parent directory is a shared root folder (either has child
    /// subdirectories in `dir_groups` or > 30 files).
    pub fn request_assign_whole_parent_dir(&mut self, anchor_idx: usize) {
        if anchor_idx >= self.anchor_cfg.anchors.len() {
            return;
        }
        let parent_dir = match self.visible_left_rows.get(self.left_cursor) {
            Some(VisibleLeftRow::DirHeader { parent_path, .. }) => parent_path.clone(),
            Some(VisibleLeftRow::FileItem { file_idx, .. }) => {
                match self.files.get(*file_idx).and_then(|p| p.parent()) {
                    Some(p) => p.to_path_buf(),
                    None => return,
                }
            }
            None => return,
        };

        let Some(grp) = self
            .dir_groups
            .iter()
            .find(|g| g.parent_path == parent_dir)
            .cloned()
        else {
            return;
        };

        let has_nested_subgroups = self
            .dir_groups
            .iter()
            .any(|other| other.parent_path != parent_dir && other.parent_path.starts_with(&parent_dir));

        if has_nested_subgroups || grp.file_indices.len() > 30 {
            self.modal = ModalState::ConfirmWholeParentDir {
                parent_dir,
                file_indices: grp.file_indices,
                anchor_idx,
            };
        } else {
            self.apply_assignment_batch(
                &grp.file_indices,
                Some(anchor_idx),
                "Assigned whole directory",
            );
            self.advance_to_next_unassigned();
        }
    }

    pub fn apply_assignment_batch(
        &mut self,
        file_indices: &[usize],
        new_anchor: Option<usize>,
        verb: &str,
    ) {
        let mut prev = Vec::with_capacity(file_indices.len());
        for &fi in file_indices {
            if fi < self.assignments.len() {
                prev.push((fi, self.assignments[fi]));
                self.assignments[fi] = new_anchor;
            }
        }
        if prev.is_empty() {
            return;
        }

        let desc = match new_anchor {
            Some(a_idx) => {
                let aname = self.anchor_cfg.anchors[a_idx]
                    .anchor_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("anchor");
                format!("{} {} file(s) -> {}", verb, prev.len(), aname)
            }
            None => format!("Cleared assignment on {} file(s)", prev.len()),
        };

        self.undo_stack.push(UndoEntry {
            description: desc.clone(),
            previous_assignments: prev,
        });
        self.dirty = true;
        self.status_message = desc;
        self.rebuild_visible_left();
        self.refresh_suggestions();
    }

    /// Unassigns the currently highlighted file or directory.
    pub fn unassign_current(&mut self) {
        let targets = self.target_file_indices_for_current_selection();
        if targets.is_empty() {
            return;
        }
        self.apply_assignment_batch(&targets, None, "Unassigned");
    }

    /// Reverts the most recent assignment / alias-sweep action on `undo_stack`.
    pub fn undo_last(&mut self) {
        let Some(entry) = self.undo_stack.pop() else {
            self.status_message = "Nothing to undo.".to_string();
            return;
        };
        for (fi, prev_anc) in entry.previous_assignments {
            if fi < self.assignments.len() {
                self.assignments[fi] = prev_anc;
            }
        }
        self.dirty = !self.undo_stack.is_empty();
        self.status_message = format!("Undid: {}", entry.description);
        self.rebuild_visible_left();
        self.refresh_suggestions();
    }

    fn advance_to_next_unassigned(&mut self) {
        if self.visible_left_rows.is_empty() {
            return;
        }
        for idx in (self.left_cursor + 1)..self.visible_left_rows.len() {
            let is_unassigned = match &self.visible_left_rows[idx] {
                VisibleLeftRow::DirHeader {
                    assigned_files,
                    total_files,
                    ..
                } => assigned_files < total_files,
                VisibleLeftRow::FileItem {
                    assigned_anchor, ..
                } => assigned_anchor.is_none(),
            };
            if is_unassigned {
                self.left_cursor = idx;
                self.refresh_suggestions();
                return;
            }
        }
    }

    /// Opens the `a` (Add Alias) modal pre-filled with a candidate creator/acronym label.
    pub fn open_add_alias_modal(&mut self) {
        let Some(anchor_idx) = self.highlighted_anchor_idx() else {
            self.status_message = "Highlight an anchor on the right pane first.".to_string();
            return;
        };
        let prefill = self
            .highlighted_left_path()
            .map(|p| extract_candidate_creator_label(&p))
            .unwrap_or_default();
        self.modal = ModalState::AddAlias {
            anchor_idx,
            alias_buf: prefill,
        };
    }

    /// Commits a new alias to `anchors.yaml`, reloads the matcher, and automatically assigns any
    /// currently-unassigned files in `self.files` that now match!
    pub fn commit_add_alias(&mut self, anchor_idx: usize, alias: &str) -> Result<usize> {
        let trimmed = alias.trim();
        if trimmed.is_empty() {
            bail!("Alias cannot be empty.");
        }
        let target_anchor_path = self.anchor_cfg.anchors[anchor_idx].anchor_path.clone();
        let added = append_alias_to_anchor_yaml(&self.anchors_yaml_path, &target_anchor_path, trimmed)?;

        self.reload_anchors_and_engines()?;

        // Auto-sweep all currently unassigned files in parallel via Rayon!
        let mut unassigned_indices = Vec::new();
        let mut unassigned_cands = Vec::new();
        for (fi, fpath) in self.files.iter().enumerate() {
            if self.assignments[fi].is_none() {
                unassigned_indices.push(fi);
                unassigned_cands.push(MiscVideoCandidate {
                    path: fpath.clone(),
                    dev_inode: (0, 0),
                    size_bytes: 1,
                    misc_root: self.common_prefix.clone(),
                });
            }
        }
        let batch_res = self.matcher.match_batch(&unassigned_cands);
        let mut index_by_path: HashMap<PathBuf, usize> = HashMap::with_capacity(unassigned_indices.len());
        for fi in unassigned_indices {
            index_by_path.insert(self.files[fi].clone(), fi);
        }

        let mut swept_prev: Vec<(usize, Option<usize>)> = Vec::new();
        for matched in batch_res.matched_files {
            if let (Some(&fi), Some(top)) = (
                index_by_path.get(&matched.candidate.path),
                matched.matches.first(),
            ) {
                swept_prev.push((fi, None));
                self.assignments[fi] = Some(top.anchor_idx);
            }
        }

        let swept_count = swept_prev.len();
        if !swept_prev.is_empty() {
            let desc = format!(
                "Alias '{}' -> auto-matched {} file(s)",
                trimmed, swept_count
            );
            self.undo_stack.push(UndoEntry {
                description: desc,
                previous_assignments: swept_prev,
            });
            self.dirty = true;
        }

        self.rebuild_visible_left();
        self.rebuild_filtered_anchors();
        self.refresh_suggestions();

        let anc_name = target_anchor_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("anchor");
        self.status_message = if added {
            format!(
                "Saved alias {:?} to {:?} -> auto-matched {} remaining file(s)!",
                trimmed, anc_name, swept_count
            )
        } else {
            format!(
                "Alias {:?} already existed on {:?} (swept {} file(s)).",
                trimmed, anc_name, swept_count
            )
        };
        Ok(swept_count)
    }

    /// Opens the `n` (New Anchor Directory) modal with inferred root volumes (`/Volumes/f0m/f`, `/Volumes/f1m/f`, ...).
    pub fn open_new_anchor_modal(&mut self) {
        let mut roots = infer_anchor_root_volumes(&self.anchor_cfg.anchors);
        if roots.is_empty() {
            roots.push(PathBuf::from("/Volumes"));
        }
        let base_label = self
            .highlighted_left_path()
            .map(|p| extract_candidate_creator_label(&p))
            .unwrap_or_else(|| "NewCreator".to_string());
        let folder_prefill = if base_label.to_lowercase().ends_with("h265") {
            base_label
        } else {
            format!("{}_h265", base_label)
        };

        self.modal = ModalState::NewAnchor {
            root_choices: roots,
            selected_root_idx: 0,
            folder_name_buf: folder_prefill,
            alias_buf: String::new(),
            active_field: NewAnchorField::FolderName,
        };
    }

    /// Creates a new anchor directory on disk, appends it to `anchors.yaml`, reloads engines,
    /// assigns the current selection to it, and auto-sweeps any matching unassigned files!
    pub fn commit_new_anchor(
        &mut self,
        root_dir: &Path,
        folder_name: &str,
        initial_alias: &str,
    ) -> Result<PathBuf> {
        let clean_folder = folder_name.trim().trim_matches('/');
        if clean_folder.is_empty() {
            bail!("Anchor folder name cannot be empty.");
        }
        let new_anchor_path = if Path::new(folder_name.trim()).is_absolute() {
            normalize_path_lexical(Path::new(folder_name.trim()))
        } else {
            normalize_path_lexical(&root_dir.join(clean_folder))
        };

        fs::create_dir_all(&new_anchor_path).with_context(|| {
            format!(
                "Failed to create new anchor directory {:?}",
                new_anchor_path
            )
        })?;

        let aliases: Vec<String> = initial_alias
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        append_new_anchor_to_yaml(&self.anchors_yaml_path, &new_anchor_path, &aliases)?;
        self.reload_anchors_and_engines()?;

        let Some(new_anchor_idx) = self
            .anchor_cfg
            .anchors
            .iter()
            .position(|a| a.anchor_path == new_anchor_path)
        else {
            bail!(
                "Newly appended anchor {:?} could not be located after reload.",
                new_anchor_path
            );
        };

        let mut targets = self.target_file_indices_for_current_selection();

        // Also auto-sweep any other unassigned files in parallel via Rayon that now match the new anchor!
        let mut unassigned_indices = Vec::new();
        let mut unassigned_cands = Vec::new();
        for (fi, fpath) in self.files.iter().enumerate() {
            if self.assignments[fi].is_none() && !targets.contains(&fi) {
                unassigned_indices.push(fi);
                unassigned_cands.push(MiscVideoCandidate {
                    path: fpath.clone(),
                    dev_inode: (0, 0),
                    size_bytes: 1,
                    misc_root: self.common_prefix.clone(),
                });
            }
        }
        let batch_res = self.matcher.match_batch(&unassigned_cands);
        let mut index_by_path: HashMap<PathBuf, usize> = HashMap::with_capacity(unassigned_indices.len());
        for fi in unassigned_indices {
            index_by_path.insert(self.files[fi].clone(), fi);
        }
        for matched in batch_res.matched_files {
            if let (Some(&fi), Some(top)) = (
                index_by_path.get(&matched.candidate.path),
                matched.matches.first(),
            ) {
                if top.anchor_idx == new_anchor_idx {
                    targets.push(fi);
                }
            }
        }
        targets.sort_unstable();
        targets.dedup();

        self.apply_assignment_batch(&targets, Some(new_anchor_idx), "Created anchor & assigned");
        self.rebuild_filtered_anchors();
        self.advance_to_next_unassigned();

        Ok(new_anchor_path)
    }

    fn reload_anchors_and_engines(&mut self) -> Result<()> {
        self.anchor_cfg = AnchorDirsConfig::load(&self.anchors_yaml_path)?;
        self.matcher =
            AnchorMatcher::new(&self.anchor_cfg.anchors, self.scanner_cfg.matching.clone());
        self.suggestion_engine = SuggestionEngine::new(
            &self.anchor_cfg.anchors,
            &self.scanner_cfg.matching.noise_tokens,
        );
        Ok(())
    }

    /// Converts all currently staged assignments into a `BatchMatchOutcome` ready for CSV export
    /// or live execution via `execute_matched_files`.
    pub fn build_staged_outcome(&self) -> BatchMatchOutcome {
        let mut matched_files = Vec::new();
        let mut unmatched_files = Vec::new();

        for (fi, fpath) in self.files.iter().enumerate() {
            let (dev_inode, size_bytes) = match fs::metadata(fpath) {
                Ok(m) => (path_dev_inode(fpath, &m), m.len()),
                Err(_) => ((0, 0), 0),
            };
            let candidate = MiscVideoCandidate {
                path: fpath.clone(),
                dev_inode,
                size_bytes,
                misc_root: self.common_prefix.clone(),
            };

            if let Some(anchor_idx) = self.assignments[fi] {
                if let Some(anc) = self.anchor_cfg.anchors.get(anchor_idx) {
                    let alias_label = anc
                        .anchor_path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or(&anc.raw_key)
                        .to_string();
                    matched_files.push(FileMatchResult {
                        candidate,
                        matches: vec![RankedAnchorMatch {
                            rank: 1,
                            total_matches: 1,
                            planned_action: PlannedAction::PrimaryMove,
                            anchor_idx,
                            anchor_path: anc.anchor_path.clone(),
                            destination_path: anc.destination_path.clone(),
                            matched_alias_alnum_len: alias_label.len(),
                            matched_alias: format!("MANUAL:{}", alias_label),
                            match_tier: MatchTier::ExactClause,
                            confidence_score: 1.0,
                            clause_position: 0,
                            anchor_yaml_order: anc.yaml_order,
                        }],
                    });
                    continue;
                }
            }
            unmatched_files.push(candidate);
        }

        BatchMatchOutcome {
            matched_files,
            unmatched_files,
        }
    }

    /// Writes `self.output_csv_path` (and optional `self.unmatched_out_path`) to disk.
    pub fn save_session(&mut self) -> Result<(usize, Option<usize>)> {
        let outcome = self.build_staged_outcome();
        let rows = write_matches_csv(&outcome, &self.output_csv_path)?;
        let unmatched_written = if let Some(ref u_out) = self.unmatched_out_path {
            Some(write_unmatched_files(&outcome.unmatched_files, u_out)?)
        } else {
            None
        };
        self.dirty = false;
        self.status_message = match unmatched_written {
            Some(u) => format!(
                "Saved {} assignment(s) to {:?} and {} remaining unmatched path(s).",
                rows, self.output_csv_path, u
            ),
            None => format!(
                "Saved {} manual assignment(s) to {:?}.",
                rows, self.output_csv_path
            ),
        };
        Ok((rows, unmatched_written))
    }
}

fn compute_common_parent_prefix(paths: &[PathBuf]) -> Option<PathBuf> {
    let first_parent = paths.first()?.parent()?;
    let mut prefix_comps: Vec<_> = first_parent.components().collect();

    for p in &paths[1..] {
        let Some(par) = p.parent() else {
            return None;
        };
        let comps: Vec<_> = par.components().collect();
        let shared = prefix_comps
            .iter()
            .zip(comps.iter())
            .take_while(|(a, b)| a == b)
            .count();
        prefix_comps.truncate(shared);
        if prefix_comps.is_empty() {
            return None;
        }
    }

    let mut out = PathBuf::new();
    for c in prefix_comps {
        out.push(c.as_os_str());
    }
    Some(out)
}

fn relative_display_path(path: &Path, common_prefix: Option<&Path>) -> String {
    if let Some(pref) = common_prefix {
        if let Ok(stripped) = path.strip_prefix(pref) {
            let s = stripped.to_string_lossy().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    path.file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

fn build_parent_dir_groups(
    files: &[PathBuf],
    common_prefix: Option<&Path>,
) -> Vec<ParentDirGroup> {
    let mut map: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
    for (idx, fpath) in files.iter().enumerate() {
        let parent = fpath
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("/"));
        map.entry(parent).or_default().push(idx);
    }

    let total_groups = map.len();
    map.into_iter()
        .map(|(parent_path, file_indices)| {
            let display_label = relative_display_path(&parent_path, common_prefix);
            ParentDirGroup {
                parent_path,
                display_label,
                // Auto-expand when there are <= 25 directory groups; collapse by default when browsing hundreds of folders!
                expanded: total_groups <= 25,
                file_indices,
            }
        })
        .collect()
}
