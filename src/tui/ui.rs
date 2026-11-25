use crate::tui::state::{
    ActivePane, LeftViewMode, ModalState, NewAnchorField, TriageApp, TuiExitOutcome,
    VisibleLeftRow,
};
use anyhow::Result;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::io::{self, Stdout};
use std::time::Duration;

struct TerminalCleanupGuard;

impl Drop for TerminalCleanupGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, LeaveAlternateScreen, DisableMouseCapture);
    }
}

/// Launches the full-screen `ratatui` + `crossterm` interactive TUI event loop.
pub fn run_tui(app: &mut TriageApp) -> Result<TuiExitOutcome> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let _guard = TerminalCleanupGuard;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let outcome = event_loop(&mut terminal, app)?;

    terminal.show_cursor()?;
    Ok(outcome)
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut TriageApp,
) -> Result<TuiExitOutcome> {
    loop {
        terminal.draw(|f| draw_frame(f, app))?;

        if !event::poll(Duration::from_millis(150))? {
            continue;
        }

        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    if app.dirty {
                        app.modal = ModalState::ConfirmQuitUnsaved {
                            assigned_count: app.assigned_count(),
                        };
                    } else {
                        return Ok(TuiExitOutcome::QuitOnly);
                    }
                    continue;
                }

                if app.modal != ModalState::None {
                    if let Some(exit) = handle_modal_key(app, key) {
                        return Ok(exit);
                    }
                    continue;
                }

                if let Some(exit) = handle_normal_key(app, key) {
                    return Ok(exit);
                }
            }
            Event::Mouse(mouse) if app.modal == ModalState::None => match mouse.kind {
                MouseEventKind::ScrollUp => app.move_cursor_up(3),
                MouseEventKind::ScrollDown => app.move_cursor_down(3),
                MouseEventKind::Down(_) => {
                    if let Ok((cols, _)) = crossterm::terminal::size() {
                        app.active_pane = if mouse.column < (cols * 54 / 100) {
                            ActivePane::UnmatchedLeft
                        } else {
                            ActivePane::AnchorsRight
                        };
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
}

fn handle_normal_key(app: &mut TriageApp, key: KeyEvent) -> Option<TuiExitOutcome> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // When focus is on the Right (Anchor Search) Pane, printable letters edit `anchor_search`
    // while still honoring navigation, `Enter` (assign), `Tab` (switch pane), and `1-5` if search is empty!
    if app.active_pane == ActivePane::AnchorsRight && !ctrl {
        match key.code {
            KeyCode::Backspace => {
                app.anchor_search.pop();
                app.rebuild_filtered_anchors();
                return None;
            }
            KeyCode::Esc => {
                if !app.anchor_search.is_empty() {
                    app.anchor_search.clear();
                    app.rebuild_filtered_anchors();
                } else {
                    app.active_pane = ActivePane::UnmatchedLeft;
                }
                return None;
            }
            KeyCode::Char(c)
                if !matches!(c, '1'..='5') || !app.anchor_search.is_empty() =>
            {
                app.anchor_search.push(c);
                app.rebuild_filtered_anchors();
                return None;
            }
            _ => {}
        }
    }

    if ctrl {
        match key.code {
            KeyCode::Char('u') => {
                app.move_cursor_up(15);
                return None;
            }
            KeyCode::Char('d') => {
                app.move_cursor_down(15);
                return None;
            }
            KeyCode::Char('a') => {
                app.open_add_alias_modal();
                return None;
            }
            KeyCode::Char('n') => {
                app.open_new_anchor_modal();
                return None;
            }
            KeyCode::Char('s') | KeyCode::Char('w') => {
                if let Err(e) = app.save_session() {
                    app.status_message = format!("Save error: {}", e);
                }
                return None;
            }
            KeyCode::Char('x') => {
                let count = app.assigned_count();
                if count > 0 {
                    app.modal = ModalState::ConfirmExecute {
                        assigned_count: count,
                    };
                }
                return None;
            }
            KeyCode::Char('q') => {
                if app.dirty {
                    app.modal = ModalState::ConfirmQuitUnsaved {
                        assigned_count: app.assigned_count(),
                    };
                } else {
                    return Some(TuiExitOutcome::QuitOnly);
                }
                return None;
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Tab => {
            app.active_pane = match app.active_pane {
                ActivePane::UnmatchedLeft => ActivePane::AnchorsRight,
                ActivePane::AnchorsRight => ActivePane::UnmatchedLeft,
            };
        }
        KeyCode::Up | KeyCode::Char('k') => app.move_cursor_up(1),
        KeyCode::Down | KeyCode::Char('j') => app.move_cursor_down(1),
        KeyCode::PageUp => app.move_cursor_up(15),
        KeyCode::PageDown => app.move_cursor_down(15),
        KeyCode::Left | KeyCode::Char('h') if app.active_pane == ActivePane::UnmatchedLeft => {
            app.set_expand_current_dir(false);
        }
        KeyCode::Right | KeyCode::Char('l') if app.active_pane == ActivePane::UnmatchedLeft => {
            app.set_expand_current_dir(true);
        }
        KeyCode::Char(' ') if app.active_pane == ActivePane::UnmatchedLeft => {
            app.toggle_multi_select_current();
        }
        KeyCode::Char('t') if app.active_pane == ActivePane::UnmatchedLeft => {
            app.toggle_left_view_mode();
        }
        KeyCode::Char('e') if app.active_pane == ActivePane::UnmatchedLeft => {
            app.toggle_expand_current_dir();
        }
        KeyCode::Char('/') => {
            app.modal = ModalState::LeftFilterInput {
                buf: app.left_filter.clone(),
            };
        }
        KeyCode::Char('f') => {
            app.active_pane = ActivePane::AnchorsRight;
        }
        KeyCode::Esc => {
            if !app.left_filter.is_empty() {
                app.left_filter.clear();
                app.rebuild_visible_left();
                app.refresh_suggestions();
                app.status_message = "Cleared left filter.".to_string();
            } else if !app.multi_selected.is_empty() {
                app.multi_selected.clear();
                app.rebuild_visible_left();
                app.status_message = "Cleared multi-selection.".to_string();
            }
        }
        KeyCode::Enter => {
            if let Some(anc_idx) = app.highlighted_anchor_idx() {
                app.assign_current_to_anchor(anc_idx);
            } else {
                app.status_message = "No matching anchor selected in the right pane.".to_string();
            }
        }
        KeyCode::Char('D') => {
            if let Some(anc_idx) = app.highlighted_anchor_idx() {
                app.request_assign_whole_parent_dir(anc_idx);
            } else {
                app.status_message =
                    "Highlight an anchor in the right pane before pressing D.".to_string();
            }
        }
        KeyCode::Char(c @ '1'..='5') => {
            let slot = (c as usize) - ('0' as usize);
            app.assign_to_suggestion_slot(slot);
        }
        KeyCode::Char('a') => {
            app.open_add_alias_modal();
        }
        KeyCode::Char('n') => {
            app.open_new_anchor_modal();
        }
        KeyCode::Char('H') => {
            app.toggle_hide_assigned();
        }
        KeyCode::Char('u') => {
            app.undo_last();
        }
        KeyCode::Backspace | KeyCode::Delete if app.active_pane == ActivePane::UnmatchedLeft => {
            app.unassign_current();
        }
        KeyCode::Char('w') => {
            if let Err(e) = app.save_session() {
                app.status_message = format!("Save error: {}", e);
            }
        }
        KeyCode::Char('X') => {
            let count = app.assigned_count();
            if count == 0 {
                app.status_message =
                    "0 files assigned yet — assign items first before pressing X.".to_string();
            } else {
                app.modal = ModalState::ConfirmExecute {
                    assigned_count: count,
                };
            }
        }
        KeyCode::Char('q') => {
            if app.dirty {
                app.modal = ModalState::ConfirmQuitUnsaved {
                    assigned_count: app.assigned_count(),
                };
            } else {
                return Some(TuiExitOutcome::QuitOnly);
            }
        }
        _ => {}
    }

    None
}

fn handle_modal_key(app: &mut TriageApp, key: KeyEvent) -> Option<TuiExitOutcome> {
    let current_modal = std::mem::replace(&mut app.modal, ModalState::None);

    match current_modal {
        ModalState::None => {}
        ModalState::LeftFilterInput { mut buf } => match key.code {
            KeyCode::Enter => {
                app.left_filter = buf;
                app.rebuild_visible_left();
                app.refresh_suggestions();
            }
            KeyCode::Esc => {
                app.rebuild_visible_left();
                app.refresh_suggestions();
            }
            KeyCode::Backspace => {
                buf.pop();
                app.left_filter = buf.clone();
                app.rebuild_visible_left();
                app.refresh_suggestions();
                app.modal = ModalState::LeftFilterInput { buf };
            }
            KeyCode::Char(c) => {
                buf.push(c);
                app.left_filter = buf.clone();
                app.rebuild_visible_left();
                app.refresh_suggestions();
                app.modal = ModalState::LeftFilterInput { buf };
            }
            _ => {
                app.modal = ModalState::LeftFilterInput { buf };
            }
        },
        ModalState::AddAlias {
            anchor_idx,
            mut alias_buf,
        } => match key.code {
            KeyCode::Esc => {
                app.status_message = "Cancelled Add Alias.".to_string();
            }
            KeyCode::Enter => {
                if let Err(e) = app.commit_add_alias(anchor_idx, &alias_buf) {
                    app.status_message = format!("Add Alias error: {}", e);
                }
            }
            KeyCode::Backspace => {
                alias_buf.pop();
                app.modal = ModalState::AddAlias {
                    anchor_idx,
                    alias_buf,
                };
            }
            KeyCode::Char(c) => {
                alias_buf.push(c);
                app.modal = ModalState::AddAlias {
                    anchor_idx,
                    alias_buf,
                };
            }
            _ => {
                app.modal = ModalState::AddAlias {
                    anchor_idx,
                    alias_buf,
                };
            }
        },
        ModalState::NewAnchor {
            root_choices,
            mut selected_root_idx,
            mut folder_name_buf,
            mut alias_buf,
            mut active_field,
        } => match key.code {
            KeyCode::Esc => {
                app.status_message = "Cancelled New Anchor.".to_string();
            }
            KeyCode::Left if key.modifiers.contains(KeyModifiers::ALT) || active_field == NewAnchorField::InitialAlias && alias_buf.is_empty() => {
                if !root_choices.is_empty() {
                    selected_root_idx = selected_root_idx.saturating_sub(1);
                }
                app.modal = ModalState::NewAnchor {
                    root_choices,
                    selected_root_idx,
                    folder_name_buf,
                    alias_buf,
                    active_field,
                };
            }
            KeyCode::Up => {
                if !root_choices.is_empty() {
                    selected_root_idx = selected_root_idx.saturating_sub(1);
                }
                app.modal = ModalState::NewAnchor {
                    root_choices,
                    selected_root_idx,
                    folder_name_buf,
                    alias_buf,
                    active_field,
                };
            }
            KeyCode::Down => {
                if !root_choices.is_empty() {
                    selected_root_idx = (selected_root_idx + 1).min(root_choices.len() - 1);
                }
                app.modal = ModalState::NewAnchor {
                    root_choices,
                    selected_root_idx,
                    folder_name_buf,
                    alias_buf,
                    active_field,
                };
            }
            KeyCode::Tab => {
                active_field = match active_field {
                    NewAnchorField::FolderName => NewAnchorField::InitialAlias,
                    NewAnchorField::InitialAlias => NewAnchorField::FolderName,
                };
                app.modal = ModalState::NewAnchor {
                    root_choices,
                    selected_root_idx,
                    folder_name_buf,
                    alias_buf,
                    active_field,
                };
            }
            KeyCode::Backspace => {
                match active_field {
                    NewAnchorField::FolderName => {
                        folder_name_buf.pop();
                    }
                    NewAnchorField::InitialAlias => {
                        alias_buf.pop();
                    }
                }
                app.modal = ModalState::NewAnchor {
                    root_choices,
                    selected_root_idx,
                    folder_name_buf,
                    alias_buf,
                    active_field,
                };
            }
            KeyCode::Char(c) => {
                match active_field {
                    NewAnchorField::FolderName => folder_name_buf.push(c),
                    NewAnchorField::InitialAlias => alias_buf.push(c),
                }
                app.modal = ModalState::NewAnchor {
                    root_choices,
                    selected_root_idx,
                    folder_name_buf,
                    alias_buf,
                    active_field,
                };
            }
            KeyCode::Enter => {
                let root = root_choices
                    .get(selected_root_idx)
                    .cloned()
                    .unwrap_or_else(|| std::path::PathBuf::from("/Volumes"));
                match app.commit_new_anchor(&root, &folder_name_buf, &alias_buf) {
                    Ok(created) => {
                        app.status_message =
                            format!("Created new anchor {:?} and assigned!", created);
                    }
                    Err(e) => {
                        app.status_message = format!("New Anchor error: {}", e);
                    }
                }
            }
            _ => {
                app.modal = ModalState::NewAnchor {
                    root_choices,
                    selected_root_idx,
                    folder_name_buf,
                    alias_buf,
                    active_field,
                };
            }
        },
        ModalState::ConfirmWholeParentDir {
            parent_dir,
            file_indices,
            anchor_idx,
        } => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                app.apply_assignment_batch(
                    &file_indices,
                    Some(anchor_idx),
                    "Assigned whole parent directory",
                );
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                app.status_message =
                    format!("Cancelled whole-directory assignment on {:?}.", parent_dir);
            }
            _ => {
                app.modal = ModalState::ConfirmWholeParentDir {
                    parent_dir,
                    file_indices,
                    anchor_idx,
                };
            }
        },
        ModalState::ConfirmExecute { assigned_count } => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                if let Err(e) = app.save_session() {
                    app.status_message = format!("Failed to save CSV before execution: {}", e);
                    return None;
                }
                return Some(TuiExitOutcome::ExecuteNow);
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                app.status_message = "Execution cancelled (assignments kept in TUI).".to_string();
            }
            _ => {
                app.modal = ModalState::ConfirmExecute { assigned_count };
            }
        },
        ModalState::ConfirmQuitUnsaved { assigned_count } => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                if let Err(e) = app.save_session() {
                    app.status_message = format!("Failed to save CSV: {}", e);
                    return None;
                }
                return Some(TuiExitOutcome::QuitOnly);
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                return Some(TuiExitOutcome::QuitOnly);
            }
            KeyCode::Esc => {
                app.status_message = "Quit cancelled.".to_string();
            }
            _ => {
                app.modal = ModalState::ConfirmQuitUnsaved { assigned_count };
            }
        },
    }

    None
}

pub fn draw_frame(f: &mut Frame, app: &TriageApp) {
    let root_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(10),
            Constraint::Length(4),
        ])
        .split(f.area());

    // -------------------------------------------------------------------------
    // Top Header Banner
    // -------------------------------------------------------------------------
    let header_line = Line::from(vec![
        Span::styled(
            " rusty_triage ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("Unassigned: {}", app.unassigned_count()),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" │ "),
        Span::styled(
            format!("Assigned: {}/{}", app.assigned_count(), app.total_files()),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" │ "),
        Span::styled(
            format!("Anchors: {}", app.anchor_cfg.anchors.len()),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw(" │ "),
        Span::styled(
            format!("Undo: {}", app.undo_depth()),
            Style::default().fg(Color::Magenta),
        ),
        Span::raw(if app.dirty { " [UNSAVED *]" } else { "" }),
    ]);
    let sub_header = Line::from(vec![
        Span::styled(" CSV Out: ", Style::default().fg(Color::DarkGray)),
        Span::raw(app.output_csv_path.to_string_lossy().to_string()),
        Span::styled("  │  Filter (/): ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            if app.left_filter.is_empty() {
                "<none>".to_string()
            } else {
                app.left_filter.clone()
            },
            Style::default().fg(Color::Yellow),
        ),
    ]);
    f.render_widget(
        Paragraph::new(vec![header_line, sub_header]),
        root_chunks[0],
    );

    // -------------------------------------------------------------------------
    // Main Two-Pane Split
    // -------------------------------------------------------------------------
    let main_panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(54), Constraint::Percentage(46)])
        .split(root_chunks[1]);

    draw_left_unmatched_pane(f, app, main_panes[0]);
    draw_right_anchors_pane(f, app, main_panes[1]);

    // -------------------------------------------------------------------------
    // Footer Status + Keybinding Legend
    // -------------------------------------------------------------------------
    let cur_path_str = app
        .highlighted_left_path()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "<no item>".to_string());

    let footer_lines = vec![
        Line::from(vec![
            Span::styled(" Path: ", Style::default().fg(Color::Cyan)),
            Span::raw(cur_path_str),
        ]),
        Line::from(vec![
            Span::styled(" Status: ", Style::default().fg(Color::Green)),
            Span::raw(app.status_message.clone()),
        ]),
        Line::from(vec![
            Span::styled(" [Tab]", Style::default().fg(Color::Yellow)),
            Span::raw("Switch Pane "),
            Span::styled("[1-5]", Style::default().fg(Color::Yellow)),
            Span::raw("Suggest "),
            Span::styled("[Enter]", Style::default().fg(Color::Yellow)),
            Span::raw("Assign "),
            Span::styled("[D]", Style::default().fg(Color::Yellow)),
            Span::raw("Whole Dir "),
            Span::styled("[Space]", Style::default().fg(Color::Yellow)),
            Span::raw("Select "),
            Span::styled("[f]", Style::default().fg(Color::Yellow)),
            Span::raw("Search Anchor "),
            Span::styled("[a]", Style::default().fg(Color::Yellow)),
            Span::raw("Add Alias "),
            Span::styled("[n]", Style::default().fg(Color::Yellow)),
            Span::raw("New Anchor "),
            Span::styled("[t]", Style::default().fg(Color::Yellow)),
            Span::raw("Tree/Flat "),
            Span::styled("[H]", Style::default().fg(Color::Yellow)),
            Span::raw("Hide Done "),
            Span::styled("[u]", Style::default().fg(Color::Yellow)),
            Span::raw("Undo "),
            Span::styled("[w]", Style::default().fg(Color::Yellow)),
            Span::raw("Save "),
            Span::styled("[X]", Style::default().fg(Color::LightRed)),
            Span::raw("Execute "),
            Span::styled("[q]", Style::default().fg(Color::Yellow)),
            Span::raw("Quit"),
        ]),
    ];
    let footer_block = Block::default().borders(Borders::TOP);
    f.render_widget(
        Paragraph::new(footer_lines).block(footer_block),
        root_chunks[2],
    );

    // -------------------------------------------------------------------------
    // Modal Overlay (if active)
    // -------------------------------------------------------------------------
    if app.modal != ModalState::None {
        draw_modal(f, app);
    }
}

fn draw_left_unmatched_pane(f: &mut Frame, app: &TriageApp, area: Rect) {
    let is_active = app.active_pane == ActivePane::UnmatchedLeft;
    let border_style = if is_active {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let mode_str = match app.left_view_mode {
        LeftViewMode::TreeByParentDir => "Tree by Parent Dir (←/→ fold, t=flat)",
        LeftViewMode::FlatFiles => "Flat Files (t=tree)",
    };
    let title = format!(
        " Unmatched Items ({}) — {} ",
        app.visible_left_rows.len(),
        mode_str
    );

    let items: Vec<ListItem> = app
        .visible_left_rows
        .iter()
        .map(|row| match row {
            VisibleLeftRow::DirHeader {
                display_label,
                expanded,
                total_files,
                assigned_files,
                selected_files,
                ..
            } => {
                let arrow = if *expanded { "▼" } else { "▶" };
                let sel_tag = if *selected_files > 0 {
                    format!("[*{}] ", selected_files)
                } else {
                    String::new()
                };
                let status_style = if *assigned_files == *total_files && *total_files > 0 {
                    Style::default().fg(Color::Green)
                } else if *assigned_files > 0 {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{} ", arrow), Style::default().fg(Color::Cyan)),
                    Span::styled(sel_tag, Style::default().fg(Color::Yellow)),
                    Span::styled(display_label.clone(), status_style),
                    Span::styled(
                        format!("  [{}/{}]", assigned_files, total_files),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
            }
            VisibleLeftRow::FileItem {
                indented,
                display_label,
                assigned_anchor,
                selected,
                ..
            } => {
                let pad = if *indented { "    " } else { "" };
                let sel_mark = if *selected { "[*] " } else { "    " };
                let mut spans = vec![
                    Span::raw(pad),
                    Span::styled(sel_mark, Style::default().fg(Color::Yellow)),
                ];
                if let Some(a_idx) = assigned_anchor {
                    let aname = app
                        .anchor_cfg
                        .anchors
                        .get(*a_idx)
                        .and_then(|a| a.anchor_path.file_name())
                        .and_then(|s| s.to_str())
                        .unwrap_or("anchor");
                    spans.push(Span::styled(
                        format!("✓ {} ", display_label),
                        Style::default().fg(Color::Green),
                    ));
                    spans.push(Span::styled(
                        format!("──► {}", aname),
                        Style::default()
                            .fg(Color::LightGreen)
                            .add_modifier(Modifier::BOLD),
                    ));
                } else {
                    spans.push(Span::raw(display_label.clone()));
                }
                ListItem::new(Line::from(spans))
            }
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(title),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(35, 55, 85))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    let mut state = ListState::default();
    if !app.visible_left_rows.is_empty() {
        state.select(Some(app.left_cursor));
    }
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_right_anchors_pane(f: &mut Frame, app: &TriageApp, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(7), Constraint::Min(6)])
        .split(area);

    // 1. Top Box: Smart Suggestions `[1]`..`[5]`
    let mut sugg_lines: Vec<Line> = Vec::new();
    if app.current_suggestions.is_empty() {
        sugg_lines.push(Line::from(Span::styled(
            "  No close lexical suggestions — press [f] or [Tab] to search anchors, or [n] to create a new anchor.",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for (i, s) in app.current_suggestions.iter().enumerate() {
            sugg_lines.push(Line::from(vec![
                Span::styled(
                    format!(" [{}] ", i + 1),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    s.display_name.clone(),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  ({} • {})", s.reason, s.volume_hint),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
    }
    let sugg_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta))
        .title(" ★ Smart Suggestions (press 1-5 to assign) ");
    f.render_widget(Paragraph::new(sugg_lines).block(sugg_block), chunks[0]);

    // 2. Bottom Box: Live Searchable Anchor Browser
    let is_active = app.active_pane == ActivePane::AnchorsRight;
    let border_style = if is_active {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let cursor_char = if is_active { "█" } else { "" };
    let title = format!(
        " Anchors ({}/{}) — Search: {}{} ",
        app.filtered_anchor_indices.len(),
        app.anchor_cfg.anchors.len(),
        app.anchor_search,
        cursor_char
    );

    let items: Vec<ListItem> = app
        .filtered_anchor_indices
        .iter()
        .map(|&anc_idx| {
            let anc = &app.anchor_cfg.anchors[anc_idx];
            let base = anc
                .anchor_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&anc.raw_key);
            let vol = anc
                .anchor_path
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let alias_suffix = if anc.aliases.is_empty() {
                String::new()
            } else {
                format!("  aliases: [{}]", anc.aliases.join(", "))
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    base.to_string(),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  ({})", vol), Style::default().fg(Color::DarkGray)),
                Span::styled(alias_suffix, Style::default().fg(Color::Cyan)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(title),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(35, 65, 55))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    let mut state = ListState::default();
    if !app.filtered_anchor_indices.is_empty() {
        state.select(Some(app.right_cursor));
    }
    f.render_stateful_widget(list, chunks[1], &mut state);
}

fn draw_modal(f: &mut Frame, app: &TriageApp) {
    let area = centered_rect(64, 34, f.area());
    f.render_widget(Clear, area);

    match &app.modal {
        ModalState::None => {}
        ModalState::LeftFilterInput { buf } => {
            let lines = vec![
                Line::from("Filter unmatched folders & video files by substring:"),
                Line::from(""),
                Line::from(vec![
                    Span::styled(" > ", Style::default().fg(Color::Cyan)),
                    Span::styled(
                        format!("{}█", buf),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    "[Enter] Keep Filter   [Esc] Close",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Filter Unmatched List (/) ");
            f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), area);
        }
        ModalState::AddAlias {
            anchor_idx,
            alias_buf,
        } => {
            let anc_path = app.anchor_cfg.anchors[*anchor_idx]
                .anchor_path
                .to_string_lossy()
                .to_string();
            let lines = vec![
                Line::from(vec![
                    Span::raw("Target Anchor: "),
                    Span::styled(anc_path, Style::default().fg(Color::Green)),
                ]),
                Line::from(""),
                Line::from("New Alias (saved to anchors.yaml & auto-sweeps remaining unmatched items):"),
                Line::from(vec![
                    Span::styled(" > ", Style::default().fg(Color::Cyan)),
                    Span::styled(
                        format!("{}█", alias_buf),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    "[Enter] Save & Auto-Sweep   [Esc] Cancel",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Magenta))
                .title(" Add Alias to Anchor (a) ");
            f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), area);
        }
        ModalState::NewAnchor {
            root_choices,
            selected_root_idx,
            folder_name_buf,
            alias_buf,
            active_field,
        } => {
            let root_str = root_choices
                .get(*selected_root_idx)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let f_cur = if *active_field == NewAnchorField::FolderName {
                "█"
            } else {
                ""
            };
            let a_cur = if *active_field == NewAnchorField::InitialAlias {
                "█"
            } else {
                ""
            };
            let lines = vec![
                Line::from(vec![
                    Span::raw("Parent Root Volume (↑/↓ to switch): "),
                    Span::styled(
                        format!(
                            "{}  ({}/{})",
                            root_str,
                            selected_root_idx + 1,
                            root_choices.len()
                        ),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::raw("Folder Name   : "),
                    Span::styled(
                        format!("{}{}", folder_name_buf, f_cur),
                        Style::default().fg(Color::Yellow),
                    ),
                ]),
                Line::from(vec![
                    Span::raw("Aliases (CSV) : "),
                    Span::styled(
                        format!("{}{}", alias_buf, a_cur),
                        Style::default().fg(Color::Green),
                    ),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    "[↑/↓] Root Volume   [Tab] Switch Field   [Enter] Create Dir & Assign   [Esc] Cancel",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Green))
                .title(" Create New Anchor Directory (n) ");
            f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), area);
        }
        ModalState::ConfirmWholeParentDir {
            parent_dir,
            file_indices,
            anchor_idx,
        } => {
            let anc = app.anchor_cfg.anchors[*anchor_idx]
                .anchor_path
                .to_string_lossy()
                .to_string();
            let lines = vec![
                Line::from(Span::styled(
                    "Large / Shared Parent Directory Warning!",
                    Style::default()
                        .fg(Color::LightRed)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(format!("Parent Dir : {:?}", parent_dir)),
                Line::from(format!("Files      : {} video file(s)", file_indices.len())),
                Line::from(format!("Target     : {}", anc)),
                Line::from(""),
                Line::from("[y / Enter] Confirm Assign All   [n / Esc] Cancel"),
            ];
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::LightRed))
                .title(" Confirm Whole Parent Directory Assignment ");
            f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), area);
        }
        ModalState::ConfirmExecute { assigned_count } => {
            let lines = vec![
                Line::from(format!(
                    "Save {:?} and execute {} staged file assignment(s) now?",
                    app.output_csv_path, assigned_count
                )),
                Line::from(""),
                Line::from(
                    "This exits the TUI and runs rusty_dedup -> Temp-Hold -> rusty_clean / direct move.",
                ),
                Line::from(""),
                Line::from("[y / Enter] Save & Execute Now   [n / Esc] Back to TUI"),
            ];
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::LightRed))
                .title(" Execute Staged Assignments (X) ");
            f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), area);
        }
        ModalState::ConfirmQuitUnsaved { assigned_count } => {
            let lines = vec![
                Line::from(format!(
                    "You have unsaved changes ({} total assigned file(s)).",
                    assigned_count
                )),
                Line::from(""),
                Line::from(format!(
                    "[y / Enter] Save to {:?} & Quit",
                    app.output_csv_path
                )),
                Line::from("[n] Discard Unsaved Changes & Quit"),
                Line::from("[Esc] Cancel & Stay in TUI"),
            ];
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(" Unsaved Assignments ");
            f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), area);
        }
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
