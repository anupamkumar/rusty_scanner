use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rusty_scanner::config::{AnchorDirsConfig, ResolvedTools, ScannerConfig};
use rusty_scanner::csv_report::read_matches_csv;
use rusty_scanner::orchestrator::execute_matched_files;
use rusty_scanner::suggestions::SuggestionEngine;
use rusty_scanner::tui::state::{ModalState, TriageApp};
use rusty_scanner::tui::ui::draw_frame;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static TUI_TEST_SEQ: AtomicUsize = AtomicUsize::new(1);

fn make_tui_temp_dir(label: &str) -> PathBuf {
    let seq = TUI_TEST_SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "rusty_triage_test_{}_{}_{}",
        label,
        std::process::id(),
        seq
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn smart_suggestions_handle_acronyms_date_prefixes_and_honorific_variants() {
    let yaml = r#"
anchors:
  /Volumes/f0m/f/the english mansion_h265: {}
  /Volumes/f0m/f/Young Goddess Kim_h265: {}
  /Volumes/f0m/f/Miss Courtney_h265: {}
  /Volumes/f0m/f/club stilletto_h265: {}
  /Volumes/f0m/f/House of Sinn_h265: {}
"#;
    let cfg = AnchorDirsConfig::from_yaml_str(yaml, Path::new("anchors.yaml")).unwrap();
    let engine = SuggestionEngine::new(&cfg.anchors, &[]);

    // 1. `TEM` acronym + `MsCourtney` in `0101-1-TEM-FootDollBoxBound-MsCourtneySophieDoll`
    let s1 = engine.top_suggestions(
        Path::new("/downloads/0101-1-TEM-FootDollBoxBound-MsCourtneySophieDoll/0101-1-TEM-FootDollBoxBound-MsCourtneySophieDoll_h265.mp4"),
        5,
    );
    assert!(
        s1.iter().any(|s| s.anchor_idx == 0),
        "Expected 'the english mansion_h265' via TEM acronym, got: {:?}",
        s1
    );

    // 2. `YGK` acronym in `1303_YGK-CruelIntentions`
    let s2 = engine.top_suggestions(
        Path::new("/downloads/1303_YGK-CruelIntentions/1303_YGK-CruelIntentions_h265.mp4"),
        5,
    );
    assert_eq!(s2[0].anchor_idx, 1);

    // 3. `1704_Ms Courtneys Fet Lair` -> suggests `Miss Courtney_h265` (honorific + possessive stripped!)
    let s3 = engine.top_suggestions(
        Path::new("/downloads/1704_Ms Courtneys Fet Lair - All He Wants/1704_Ms Courtneys Fet Lair_h265.mp4"),
        5,
    );
    assert_eq!(s3[0].anchor_idx, 2);

    // 4. `1303_Club Stiletto-AshesandSpit` -> suggests misspelled `club stilletto_h265`
    let s4 = engine.top_suggestions(
        Path::new("/downloads/1303_Club Stiletto-AshesandSpit/1303_Club Stiletto_h265.mp4"),
        5,
    );
    assert_eq!(s4[0].anchor_idx, 3);
}

#[test]
fn tui_tree_grouping_whole_dir_guard_alias_autosweep_new_anchor_undo_and_render() {
    let base = make_tui_temp_dir("state_and_render");
    let vol_f0 = base.join("f0m");
    let vol_f1 = base.join("f1m");
    let anc_tem = vol_f0.join("the english mansion_h265");
    let anc_courtney = vol_f0.join("Miss Courtney_h265");
    let anc_other = vol_f1.join("candy glitter_h265");
    for d in [&anc_tem, &anc_courtney, &anc_other] {
        fs::create_dir_all(d).unwrap();
    }

    let anchors_yaml_path = base.join("anchors.yaml");
    let yaml_content = format!(
        r#"version: 1.0
# Preserve this comment!
anchors:
  "{}":
    aliases: []
  "{}":
    aliases: ["Ms Courtney"]
  "{}": {{}}
"#,
        anc_tem.display(),
        anc_courtney.display(),
        anc_other.display()
    );
    fs::write(&anchors_yaml_path, yaml_content).unwrap();

    // Unmatched structure:
    // - Shared root `/downloads` has a loose file AND two creator pack subfolders (`PackA` with 2 files, `TEM_1`, `TEM_2`, `BrookPack`)
    let dl = base.join("downloads");
    let loose = dl.join("loose_root_clip.mp4");
    let pack_a_1 = dl.join("CourtneyPack").join("clip1.mp4");
    let pack_a_2 = dl.join("CourtneyPack").join("clip2.mp4");
    let tem_1 = dl.join("0511_TEM - Scene One").join("0511_TEM - Scene One_h265.mp4");
    let tem_2 = dl.join("1403_TEM - Scene Two").join("1403_TEM - Scene Two_h265.mp4");
    let new_creator = dl.join("2012_Brook Logan - Clip").join("2012_Brook Logan - Clip_h265.mp4");

    let unmatched_files = vec![
        loose.clone(),
        pack_a_1.clone(),
        pack_a_2.clone(),
        tem_1.clone(),
        tem_2.clone(),
        new_creator.clone(),
    ];
    for f in &unmatched_files {
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(f, b"dummy_video").unwrap();
    }

    let anchor_cfg = AnchorDirsConfig::load(&anchors_yaml_path).unwrap();
    let scanner_cfg = ScannerConfig::default();
    let out_csv = base.join("manual_matches.csv");
    let rem_un = base.join("un_remaining.txt");

    let mut app = TriageApp::new(
        anchors_yaml_path.clone(),
        out_csv.clone(),
        Some(rem_un.clone()),
        anchor_cfg,
        scanner_cfg,
        unmatched_files,
    )
    .unwrap();

    assert_eq!(app.total_files(), 6);
    assert_eq!(app.assigned_count(), 0);

    // 1. Pressing `D` on the loose file directly inside shared `/downloads` triggers safety confirmation modal!
    let loose_row_pos = app
        .visible_left_rows
        .iter()
        .position(|r| matches!(r, rusty_scanner::tui::state::VisibleLeftRow::DirHeader { parent_path, .. } if parent_path == &dl))
        .unwrap();
    app.left_cursor = loose_row_pos;
    app.request_assign_whole_parent_dir(1);
    assert!(
        matches!(app.modal, ModalState::ConfirmWholeParentDir { .. }),
        "Expected safety confirmation modal when pressing D on shared root /downloads"
    );
    app.modal = ModalState::None;
    assert_eq!(app.assigned_count(), 0);

    // 2. Pressing `D` on a file inside `CourtneyPack` (leaf creator folder with 2 videos) assigns BOTH immediately!
    let pack_a_row = app
        .visible_left_rows
        .iter()
        .position(|r| matches!(r, rusty_scanner::tui::state::VisibleLeftRow::FileItem { display_label, .. } if display_label == "clip1.mp4"))
        .unwrap();
    app.left_cursor = pack_a_row;
    app.request_assign_whole_parent_dir(1); // `Miss Courtney_h265`
    assert_eq!(app.modal, ModalState::None);
    assert_eq!(app.assigned_count(), 2);

    // 3. Test Undo (`u`) and re-apply
    app.undo_last();
    assert_eq!(app.assigned_count(), 0);
    app.left_cursor = pack_a_row;
    app.request_assign_whole_parent_dir(1);
    assert_eq!(app.assigned_count(), 2);

    // 4. Test Live Alias Learning (`a`): adding alias `"TEM"` to `the english mansion_h265` updates
    //    `anchors.yaml` AND automatically sweeps BOTH `0511_TEM` and `1403_TEM`!
    let swept = app.commit_add_alias(0, "TEM").unwrap();
    assert_eq!(swept, 2);
    assert_eq!(app.assigned_count(), 4);
    let updated_yaml = fs::read_to_string(&anchors_yaml_path).unwrap();
    assert!(updated_yaml.contains("# Preserve this comment!"));
    assert!(updated_yaml.contains("\"TEM\""));

    // 5. Test New Anchor Creation (`n`): creates `/.../f0m/Brook Logan_h265`, updates `anchors.yaml`,
    //    and assigns `2012_Brook Logan - Clip_h265.mp4`!
    let brook_row = app
        .visible_left_rows
        .iter()
        .position(|r| matches!(r, rusty_scanner::tui::state::VisibleLeftRow::FileItem { display_label, .. } if display_label.contains("Brook Logan")))
        .unwrap();
    app.left_cursor = brook_row;
    app.open_new_anchor_modal();
    assert!(matches!(app.modal, ModalState::NewAnchor { .. }));
    app.modal = ModalState::None;

    let created_dir = app
        .commit_new_anchor(&vol_f0, "Brook Logan_h265", "BrookLogan")
        .unwrap();
    assert!(created_dir.is_dir());
    assert_eq!(app.anchor_cfg.anchors.len(), 4);
    assert_eq!(app.assigned_count(), 5); // Only `loose_root_clip.mp4` remains unassigned!

    // 6. Render a full `ratatui` frame via `TestBackend` to verify headless UI rendering works cleanly!
    let backend = TestBackend::new(130, 36);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw_frame(f, &app)).unwrap();
    let buffer = terminal.backend().buffer();
    let mut screen_text = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            screen_text.push_str(buffer[(x, y)].symbol());
        }
        screen_text.push('\n');
    }
    assert!(screen_text.contains("rusty_triage"));
    assert!(screen_text.contains("Assigned: 5/6"));
    assert!(screen_text.contains("Brook Logan_h265"));

    // 7. Save session (`w`) -> verify `manual_matches.csv` (5 rows) + `un_remaining.txt` (1 file)
    let (csv_rows, rem_count) = app.save_session().unwrap();
    assert_eq!(csv_rows, 5);
    assert_eq!(rem_count, Some(1));

    let reloaded = read_matches_csv(&out_csv, &app.anchor_cfg).unwrap();
    assert_eq!(reloaded.len(), 5);

    let _ = fs::remove_dir_all(&base);
}

#[cfg(unix)]
#[test]
fn execute_from_csv_runs_end_to_end_pipeline() {
    let base = make_tui_temp_dir("from_csv_exec");
    let misc = base.join("misc");
    let anc = base.join("TargetAnchor_h265");
    let mock_bins = base.join("bins");
    for d in [&misc, &anc, &mock_bins] {
        fs::create_dir_all(d).unwrap();
    }

    let vid = misc.join("manual_pick.mp4");
    fs::write(&vid, b"video_payload").unwrap();

    let mock_dedup = mock_bins.join("rusty_dedup");
    fs::write(
        &mock_dedup,
        "#!/usr/bin/env python3\nimport json, sys\nif '--record-moves' in sys.argv:\n    sys.exit(0)\nout = sys.argv[sys.argv.index('-o') + 1]\nwith open(out, 'w') as f:\n    json.dump({'duplicates': []}, f)\n",
    )
    .unwrap();
    fs::set_permissions(&mock_dedup, fs::Permissions::from_mode(0o755)).unwrap();

    let mock_clean = mock_bins.join("rusty_clean");
    fs::write(&mock_clean, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&mock_clean, fs::Permissions::from_mode(0o755)).unwrap();

    let anchors_yaml = base.join("anchors.yaml");
    fs::write(
        &anchors_yaml,
        format!("anchors:\n  \"{}\":\n    aliases: []\n", anc.display()),
    )
    .unwrap();

    let anchor_cfg = AnchorDirsConfig::load(&anchors_yaml).unwrap();
    let mut scanner_cfg = ScannerConfig::default();
    scanner_cfg.reports.output_dir = base.join("reports");
    let csv_path = base.join("manual_matches.csv");

    let mut app = TriageApp::new(
        anchors_yaml,
        csv_path.clone(),
        None,
        anchor_cfg.clone(),
        scanner_cfg.clone(),
        vec![vid.clone()],
    )
    .unwrap();
    app.assign_current_to_anchor(0);
    app.save_session().unwrap();

    let curated = read_matches_csv(&csv_path, &anchor_cfg).unwrap();
    let tools = ResolvedTools {
        python_bin: PathBuf::from("python3"),
        dedup_metrics_harness: Some(PathBuf::from(
            "/Users/maildev/coding/rusty_dedup/scripts/rusty_dedup_metrics_harness.py",
        )),
        harness_linger_secs: Some(0.0),
        rusty_dedup_bin: mock_dedup,
        rusty_clean_bin: mock_clean,
        dedup_config: None,
        decision_config: None,
        states_dir: base.join(".states"),
    };
    let stats =
        execute_matched_files(&curated, &anchor_cfg, &scanner_cfg, &tools, false).unwrap();
    assert_eq!(stats.files_moved_directly, 1);
    assert!(!vid.exists());
    assert!(anc.join("manual_pick.mp4").is_file());

    let _ = fs::remove_dir_all(&base);
}
