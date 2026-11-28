use rusty_scanner::config::{AnchorDirsConfig, MatchingConfig, ResolvedTools, ScannerConfig};
use rusty_scanner::csv_report::write_matches_csv;
use rusty_scanner::matcher::{AnchorMatcher, MatchTier, PlannedAction};
use rusty_scanner::orchestrator::{recover_stranded_temp_holds, run_live_loop};
use rusty_scanner::scanner::{MiscVideoCandidate, discover_misc_videos};
use std::collections::HashSet;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static TEST_SEQ: AtomicUsize = AtomicUsize::new(1);

fn make_temp_test_dir(label: &str) -> PathBuf {
    let seq = TEST_SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "rusty_scanner_test_{}_{}_{}",
        label,
        std::process::id(),
        seq
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn parses_map_style_anchor_dirs_yaml_with_aliases_and_destinations() {
    let yaml = r#"
version: 1.0
anchors:
  "/Volumes/Media/Creator #1_h265": # trailing comment after quoted '#' path
    aliases:
      - CreatorOne
      - C1
    destination: incoming/2026

  /Volumes/Media/StudioBeta:
    aliases: [SB, "Studio Beta Official"]
    destination: /Volumes/External/StudioBetaDrop

  /Volumes/Media/MinimalAnchor: {}
"#;
    let cfg = AnchorDirsConfig::from_yaml_str(yaml, Path::new("anchor_dirs.yaml")).unwrap();
    assert_eq!(cfg.anchors.len(), 3);

    assert_eq!(
        cfg.anchors[0].anchor_path,
        PathBuf::from("/Volumes/Media/Creator #1_h265")
    );
    assert_eq!(cfg.anchors[0].aliases, vec!["CreatorOne", "C1"]);
    assert_eq!(
        cfg.anchors[0].destination_path,
        PathBuf::from("/Volumes/Media/Creator #1_h265/incoming/2026")
    );
    assert!(cfg.anchors[0].has_custom_destination());

    assert_eq!(
        cfg.anchors[1].anchor_path,
        PathBuf::from("/Volumes/Media/StudioBeta")
    );
    assert_eq!(
        cfg.anchors[1].aliases,
        vec!["SB", "Studio Beta Official"]
    );
    assert_eq!(
        cfg.anchors[1].destination_path,
        PathBuf::from("/Volumes/External/StudioBetaDrop")
    );

    assert_eq!(
        cfg.anchors[2].anchor_path,
        PathBuf::from("/Volumes/Media/MinimalAnchor")
    );
    assert!(cfg.anchors[2].aliases.is_empty());
    assert_eq!(
        cfg.anchors[2].destination_path,
        PathBuf::from("/Volumes/Media/MinimalAnchor")
    );
    assert!(!cfg.anchors[2].has_custom_destination());

    // Relative anchor path MUST be rejected
    let bad_yaml = "anchors:\n  relative/anchor/path:\n    aliases: [Rel]\n";
    assert!(AnchorDirsConfig::from_yaml_str(bad_yaml, Path::new("bad.yaml")).is_err());
}

#[test]
fn parses_repository_scanner_config_yaml() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cfg_path = manifest_dir.join("scanner_config.yaml");
    let cfg = ScannerConfig::load(&cfg_path).expect("scanner_config.yaml should parse cleanly");

    assert!((cfg.matching.exact_clause_score - 1.00).abs() < 1e-6);
    assert!((cfg.matching.whole_token_score - 0.95).abs() < 1e-6);
    assert!((cfg.matching.compact_affix_score - 0.90).abs() < 1e-6);
    assert_eq!(cfg.matching.max_secondary_symlinks, 3);
    assert!(cfg.matching.fuzzy_enabled);
    assert!(cfg.video_extensions.contains("mp4"));
    assert!(cfg.video_extensions.contains("mkv"));
    assert!(cfg.tools.rusty_dedup_bin.is_none()); // `auto`
    assert_eq!(cfg.tools.states_dir, PathBuf::from(".states"));
    assert!(cfg.reports.enabled);
    assert_eq!(cfg.reports.output_dir, PathBuf::from("./reports"));
    assert!(cfg.report_server.enabled);
    assert_eq!(cfg.report_server.listen_addr, "0.0.0.0:8787");
    assert!(cfg.report_server.serve_during_run);
}

#[test]
fn matching_and_multi_anchor_ranking_covers_all_tiers_and_guards() {
    let yaml = r#"
anchors:
  /Anchors/Candy Glitter_h265:
    aliases:
      - CG
      - Goon For Life
    destination: sorted
  /Anchors/Studio Alpha:
    aliases: [SA]
  /Anchors/Artist Beta:
    aliases: [AB]
  /Anchors/Director Gamma:
    aliases: [DG]
  /Anchors/Extra Fifth:
    aliases: [EF]
  /Anchors/Episode 1: {}
  /Anchors/G I R L_h265: {}
"#;
    let anchor_cfg = AnchorDirsConfig::from_yaml_str(yaml, Path::new("test.yaml")).unwrap();
    let matcher = AnchorMatcher::new(&anchor_cfg.anchors, MatchingConfig::default());

    let mk_cand = |p: &str, root: Option<&str>| MiscVideoCandidate {
        path: PathBuf::from(p),
        dev_inode: (1, 1),
        size_bytes: 1024,
        misc_root: root.map(PathBuf::from),
    };

    // 1. Short acronym `CG` matches `[CG] Sunset.mp4` via exact clause, NEVER `cgi_animation.mp4`
    let r_cg = matcher.match_single(&mk_cand("/misc/[CG] Sunset_1080p_h265.mp4", Some("/misc")));
    assert_eq!(r_cg.matches.len(), 1);
    assert_eq!(r_cg.matches[0].anchor_idx, 0);
    assert_eq!(r_cg.matches[0].match_tier, MatchTier::ExactClause);

    let r_cgi = matcher.match_single(&mk_cand("/misc/cgi_animation_clip.mp4", Some("/misc")));
    assert!(
        r_cgi.matches.is_empty(),
        "Short alias 'CG' must never match 'cgi_animation'"
    );

    // 2. Fuzzy match catches genuine typo (`Candy Gliter`), rejects false cousin (`Candy Girl`),
    //    and blocks sequel number mismatch (`Episode 2` vs `Episode 1`)
    let r_typo = matcher.match_single(&mk_cand("/misc/Candy Gliter - Clip.mp4", Some("/misc")));
    assert_eq!(r_typo.matches.len(), 1);
    assert_eq!(r_typo.matches[0].anchor_idx, 0);
    assert_eq!(r_typo.matches[0].match_tier, MatchTier::FuzzyClause);

    let r_girl = matcher.match_single(&mk_cand("/misc/Candy Girl - Clip.mp4", Some("/misc")));
    assert!(
        r_girl.matches.is_empty(),
        "'Candy Girl' must not fuzzily false-match 'Candy Glitter'"
    );

    let r_ep2 = matcher.match_single(&mk_cand("/misc/Episode 2 - Intro.mp4", Some("/misc")));
    assert!(
        r_ep2.matches.is_empty(),
        "Sequel number guard must block 'Episode 2' matching 'Episode 1'"
    );

    // 3. Enclosing directory matching: BOTH subfolders beneath `misc_root` AND `misc_root`'s own
    //    folder basename (`Abbie Cat H265` / `Candy Glitter H265`) map every file inside to that anchor as Rank #1!
    let r_sub = matcher.match_single(&mk_cand(
        "/misc/Candy_Glitter/random_001.mp4",
        Some("/misc"),
    ));
    assert_eq!(r_sub.matches.len(), 1);
    assert_eq!(r_sub.matches[0].anchor_idx, 0);
    assert_eq!(r_sub.matches[0].match_tier, MatchTier::ExactSubdir);

    let r_passed_dir = matcher.match_single(&mk_cand(
        "/Volumes/f1m/f/z_fdc_h265/downloads/Candy Glitter H265/Studio Alpha - GuestScene.mp4",
        Some("/Volumes/f1m/f/z_fdc_h265/downloads/Candy Glitter H265"),
    ));
    assert_eq!(r_passed_dir.matches.len(), 2);
    assert_eq!(r_passed_dir.matches[0].anchor_idx, 0); // Enclosing dir `Candy Glitter H265` -> Rank #1 `PRIMARY_MOVE`
    assert_eq!(
        r_passed_dir.matches[0].planned_action,
        PlannedAction::PrimaryMove
    );
    assert_eq!(r_passed_dir.matches[1].anchor_idx, 1); // Co-star `Studio Alpha` -> Rank #2 `SYMLINK`
    assert_eq!(
        r_passed_dir.matches[1].planned_action,
        PlannedAction::Symlink
    );

    // 3b. Stylized letter-spaced `G I R L_h265` matches `G.I.R.L.`, NEVER the ordinary word `Girl`
    let r_girl_word = matcher.match_single(&mk_cand(
        "/misc/New Girl Alexa Smoking_h265.mp4",
        Some("/misc"),
    ));
    assert!(
        r_girl_word.matches.is_empty(),
        "'G I R L_h265' must not collapse to match ordinary English word 'Girl'"
    );
    let r_girl_acronym = matcher.match_single(&mk_cand(
        "/misc/G.I.R.L. - Episode Scene.mp4",
        Some("/misc"),
    ));
    assert_eq!(r_girl_acronym.matches.len(), 1);
    assert_eq!(r_girl_acronym.matches[0].anchor_idx, 6);

    // 4. 5-way collaboration: Rank #1 gets `PRIMARY_MOVE`, Ranks #2-#4 get `SYMLINK`, Rank #5 gets `RANK_EXCEEDED_NO_OP`
    let r_collab = matcher.match_single(&mk_cand(
        "/misc/Studio Alpha & Candy Glitter & Artist Beta & Director Gamma & Extra Fifth - MegaCollab.mp4",
        Some("/misc"),
    ));
    assert_eq!(r_collab.matches.len(), 5);
    assert_eq!(r_collab.matches[0].anchor_idx, 1); // Studio Alpha (clause_pos 0 -> Rank #1)
    assert_eq!(r_collab.matches[0].planned_action, PlannedAction::PrimaryMove);
    assert_eq!(r_collab.matches[1].anchor_idx, 0); // Candy Glitter (Rank #2)
    assert_eq!(r_collab.matches[1].planned_action, PlannedAction::Symlink);
    assert_eq!(r_collab.matches[2].anchor_idx, 2); // Artist Beta (Rank #3)
    assert_eq!(r_collab.matches[2].planned_action, PlannedAction::Symlink);
    assert_eq!(r_collab.matches[3].anchor_idx, 3); // Director Gamma (Rank #4)
    assert_eq!(r_collab.matches[3].planned_action, PlannedAction::Symlink);
    assert_eq!(r_collab.matches[4].anchor_idx, 4); // Extra Fifth (Rank #5)
    assert_eq!(
        r_collab.matches[4].planned_action,
        PlannedAction::RankExceededNoOp
    );
}

#[test]
fn dry_run_csv_generation_leaves_files_untouched() {
    let base = make_temp_test_dir("dry_run");
    let misc_dir = base.join("misc");
    let anc_a = base.join("AnchorAlpha");
    let anc_b = base.join("AnchorBeta");
    fs::create_dir_all(&misc_dir).unwrap();
    fs::create_dir_all(&anc_a).unwrap();
    fs::create_dir_all(&anc_b).unwrap();

    let f1 = misc_dir.join("AnchorAlpha & AnchorBeta - Collab, Special.mp4");
    let f2 = misc_dir.join("completely_unknown.mkv");
    fs::write(&f1, b"video_bytes_1").unwrap();
    fs::write(&f2, b"video_bytes_2").unwrap();

    let anchors_yaml = format!(
        "anchors:\n  {}:\n    destination: incoming\n  {}: {{}}\n",
        anc_a.display(),
        anc_b.display()
    );
    let anchor_cfg =
        AnchorDirsConfig::from_yaml_str(&anchors_yaml, Path::new("anchors.yaml")).unwrap();
    let mut scanner_cfg = ScannerConfig::default();
    scanner_cfg.reports.output_dir = base.join("reports");
    let matcher = AnchorMatcher::new(&anchor_cfg.anchors, scanner_cfg.matching.clone());

    let cands = discover_misc_videos(
        std::slice::from_ref(&misc_dir),
        &anchor_cfg.anchors,
        &scanner_cfg.video_extensions,
        &HashSet::new(),
        true,
        false,
    );
    assert_eq!(cands.len(), 2);

    let outcome = matcher.match_batch(&cands);
    let csv_path = base.join("matches.csv");
    let rows = write_matches_csv(&outcome, &csv_path).unwrap();
    assert_eq!(rows, 2);

    let csv_text = fs::read_to_string(&csv_path).unwrap();
    assert!(csv_text.contains("PRIMARY_MOVE"));
    assert!(csv_text.contains("SYMLINK"));
    assert!(csv_text.contains("incoming"));
    // Both original misc files MUST still exist untouched
    assert!(f1.is_file());
    assert!(f2.is_file());

    let _ = fs::remove_dir_all(&base);
}

#[cfg(unix)]
#[test]
fn live_orchestrator_handles_direct_moves_temp_parking_clean_symlinks_and_unmatched() {
    let base = make_temp_test_dir("live_e2e");
    let misc_dir = base.join("misc");
    let anc1 = base.join("CreatorOne");
    let anc2 = base.join("CreatorTwo");
    let anc3 = base.join("CreatorThree");
    let anc4 = base.join("CreatorFour");
    let anc5 = base.join("CreatorFive");
    let mock_bin_dir = base.join("mock_bins");

    for d in [&misc_dir, &anc1, &anc2, &anc3, &anc4, &anc5, &mock_bin_dir] {
        fs::create_dir_all(d).unwrap();
    }

    // Existing files inside `CreatorOne` root:
    // - `existing_keep.mp4`: unrelated non-duplicate in `CreatorOne` root (must NOT be swept into `CreatorOne/vault`!)
    // - `dup_scene_hq.mkv`: existing higher-quality duplicate in `CreatorOne` root
    let anc1_unrelated = anc1.join("existing_keep.mp4");
    let anc1_hq_dup = anc1.join("dup_scene_hq.mkv");
    fs::write(&anc1_unrelated, b"keep_in_anchor_root").unwrap();
    fs::write(&anc1_hq_dup, b"high_quality_anchor_copy_1080p").unwrap();

    // Misc files:
    // 1. `CreatorOne & CreatorTwo & CreatorThree & CreatorFour & CreatorFive - DupScene.mp4`
    //    -> Matches all 5 anchors! Rank #1 = CreatorOne (`destination: vault`).
    //    -> Duplicate of `anc1_hq_dup` -> `rusty_clean` retains winner in `CreatorOne/vault` and deletes loser,
    //       while `anc1_unrelated` and `unmatched_clip.avi` stay untouched via Temp-Hold parking!
    //    -> Ranks #2, #3, #4 (`CreatorTwo`, `CreatorThree`, `CreatorFour`) get symlinks; `CreatorFive` (Rank #5) gets NO symlink!
    let m_dup = misc_dir.join(
        "CreatorOne & CreatorTwo & CreatorThree & CreatorFour & CreatorFive - DupScene.mp4",
    );
    fs::write(&m_dup, b"low_quality_misc_dup").unwrap();

    // 2. `CreatorTwo - BrandNewClip.mp4` -> Matches `CreatorTwo` (no duplicates -> moved directly, NOT copied!)
    let m_unique = misc_dir.join("CreatorTwo - BrandNewClip.mp4");
    fs::write(&m_unique, b"unique_video_payload").unwrap();

    // 3. `mystery_clip_999.avi` -> Unmatched -> must stay in `misc_dir` and be listed in `misc_files.txt`!
    let m_unmatched = misc_dir.join("mystery_clip_999.avi");
    fs::write(&m_unmatched, b"unmatched_payload").unwrap();

    // Create mock `rusty_dedup` script that inspects passed file args:
    // - Verifies individual files (not `misc_dir`) were passed!
    // - Emits a duplicate group when `DupScene.mp4` is among args; otherwise emits empty `duplicates: []`.
    let mock_dedup = mock_bin_dir.join("rusty_dedup");
    let dedup_script = r#"#!/usr/bin/env python3
import json, sys, os

out_path = "duplicates.json"
states_dir = ".states"
record_moves_path = None
inputs = []
i = 1
while i < len(sys.argv):
    a = sys.argv[i]
    if a in ("-o", "--output"):
        out_path = sys.argv[i + 1]
        i += 2
    elif a == "--states-dir":
        states_dir = sys.argv[i + 1]
        i += 2
    elif a == "--inputs-from":
        with open(sys.argv[i + 1], "r", encoding="utf-8") as lf:
            for line in lf:
                s = line.strip()
                if s:
                    inputs.append(s)
        i += 2
    elif a == "--record-moves":
        record_moves_path = sys.argv[i + 1]
        i += 2
    elif a in ("-c", "--dedup-config", "--config", "--categories-log"):
        i += 2
    elif a.startswith("-"):
        i += 1
    else:
        inputs.append(a)
        i += 1

if record_moves_path is not None:
    os.makedirs(states_dir, exist_ok=True)
    with open(record_moves_path, "r", encoding="utf-8") as mf:
        moves = json.load(mf)
    assert isinstance(moves, list) and len(moves) >= 1
    with open(os.path.join(states_dir, "recorded_moves.jsonl"), "a", encoding="utf-8") as out_f:
        for m in moves:
            out_f.write(json.dumps(m) + "\n")
    sys.exit(0)

# Verify misc directory itself was NOT passed — only anchor dir + individual matched files via --inputs-from!
# AND verify rusty_dedup was launched UNDER rusty_dedup_metrics_harness.py!
assert os.environ.get("RUSTY_DEDUP_HARNESS_ACTIVE") == "1", "Expected RUSTY_DEDUP_HARNESS_ACTIVE=1 from Python metrics harness!"
assert os.environ.get("RUSTY_DEDUP_TELEMETRY_SNAPSHOT"), "Expected RUSTY_DEDUP_TELEMETRY_SNAPSHOT from Python metrics harness!"
for inp in inputs[1:]:
    assert os.path.isfile(inp), f"Expected individual file arg to rusty_dedup, got: {inp}"

anchor_dir = os.path.realpath(inputs[0])
dup_misc = [os.path.realpath(p) for p in inputs[1:] if "DupScene" in os.path.basename(p)]
hq_cand = os.path.realpath(os.path.join(anchor_dir, "dup_scene_hq.mkv"))

dups = []
if dup_misc and os.path.exists(hq_cand):
    dups.append({
        "dup_group_name": "dup_scene",
        "files": [hq_cand, dup_misc[0]],
        "attributes": {}
    })

with open(out_path, "w") as f:
    json.dump({"input_directories": inputs, "duplicates": dups}, f)
"#;
    fs::write(&mock_dedup, dedup_script).unwrap();
    fs::set_permissions(&mock_dedup, fs::Permissions::from_mode(0o755)).unwrap();

    // Create mock `rusty_clean` script that faithfully simulates `rusty_clean -t <target>`:
    // - Crucially walks `parent(f)` for every duplicate file (just like real `rusty_clean` Phase 2!) and moves
    //   ANY unparked non-duplicate video it finds there into `--target` — proving our Temp-Hold parking
    //   actually protected `mystery_clip_999.avi` and `existing_keep.mp4`!
    let mock_clean = mock_bin_dir.join("rusty_clean");
    let clean_script = r#"#!/usr/bin/env python3
import json, os, shutil, sys

dup_json = sys.argv[1]
target_dir = None
report_out = None
i = 2
while i < len(sys.argv):
    a = sys.argv[i]
    if a in ("-t", "--target"):
        target_dir = sys.argv[i + 1]
        i += 2
    elif a in ("-o", "--report-out"):
        report_out = sys.argv[i + 1]
        i += 2
    elif a in ("-c", "--decision-config", "--states-dir"):
        i += 2
    else:
        i += 1

os.makedirs(target_dir, exist_ok=True)
with open(dup_json) as f:
    data = json.load(f)

dup_files = set()
parent_dirs = set()
reports = []

for grp in data.get("duplicates", []):
    files = grp["files"]
    for p in files:
        dup_files.add(os.path.abspath(p))
        parent_dirs.add(os.path.dirname(os.path.abspath(p)))
    # Retain first (HQ) file, delete second (misc dup), move winner into target_dir
    winner = files[0]
    loser = files[1]
    if os.path.exists(loser):
        os.remove(loser)
    dest_winner = os.path.join(target_dir, os.path.basename(winner))
    if os.path.abspath(winner) != os.path.abspath(dest_winner):
        shutil.move(winner, dest_winner)
    reports.append({
        "dup_group_name": grp["dup_group_name"],
        "retained_winner": dest_winner,
        "scores": [{"file_path": p} for p in files]
    })

# Simulate rusty_clean Phase 2: walk every parent_dir != target_dir and sweep any unparked videos!
VIDEO_EXTS = {".mp4", ".mkv", ".avi", ".mov", ".wmv"}
for pdir in parent_dirs:
    if os.path.abspath(pdir) == os.path.abspath(target_dir):
        continue
    for root, dirs, fnames in os.walk(pdir):
        if os.path.abspath(root) == os.path.abspath(target_dir):
            continue
        for fn in fnames:
            full = os.path.abspath(os.path.join(root, fn))
            if full in dup_files:
                continue
            if os.path.splitext(fn)[1].lower() in VIDEO_EXTS:
                shutil.move(full, os.path.join(target_dir, fn))

if report_out:
    with open(report_out, "w") as f:
        json.dump(reports, f)
"#;
    fs::write(&mock_clean, clean_script).unwrap();
    fs::set_permissions(&mock_clean, fs::Permissions::from_mode(0o755)).unwrap();

    let anchors_yaml = format!(
        r#"
anchors:
  {}:
    destination: vault
  {}:
    destination: links
  {}: {{}}
  {}: {{}}
  {}: {{}}
"#,
        anc1.display(),
        anc2.display(),
        anc3.display(),
        anc4.display(),
        anc5.display()
    );
    let anchor_cfg =
        AnchorDirsConfig::from_yaml_str(&anchors_yaml, Path::new("anchors.yaml")).unwrap();
    let mut scanner_cfg = ScannerConfig::default();
    scanner_cfg.reports.output_dir = base.join("reports");
    let matcher = AnchorMatcher::new(&anchor_cfg.anchors, scanner_cfg.matching.clone());
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

    let unmatched_out = base.join("misc_files.txt");
    let stats = run_live_loop(
        std::slice::from_ref(&misc_dir),
        &anchor_cfg,
        &scanner_cfg,
        &matcher,
        &tools,
        None,
        &unmatched_out,
        true,
    )
    .unwrap();

    assert_eq!(stats.dedup_runs, 2);
    assert_eq!(stats.clean_runs, 1);
    assert_eq!(stats.files_moved_directly, 1);
    assert_eq!(stats.files_resolved_via_clean, 1);
    assert_eq!(stats.secondary_symlinks_created, 3); // Ranks #2, #3, #4 only!
    assert_eq!(stats.unmatched_written, 1);

    // 1. Both matched misc files are GONE from `misc_dir` (moved/deleted, never copied!)
    assert!(!m_dup.exists());
    assert!(!m_unique.exists());

    // 2. Unmatched misc file AND unrelated `CreatorOne` root file stayed untouched in their original places!
    assert!(
        m_unmatched.is_file(),
        "Unmatched misc file must remain in misc_dir (protected by Temp-Hold during rusty_clean)"
    );
    assert!(
        anc1_unrelated.is_file(),
        "Unrelated file in CreatorOne root must not be swept into CreatorOne/vault"
    );

    // 3. Surviving HQ winner is in `CreatorOne/vault/dup_scene_hq.mkv`
    let expected_winner = anc1.join("vault").join("dup_scene_hq.mkv");
    assert!(expected_winner.is_file());

    // 4. Unique file moved to `CreatorTwo/links/CreatorTwo - BrandNewClip.mp4`
    assert!(
        anc2.join("links")
            .join("CreatorTwo - BrandNewClip.mp4")
            .is_file()
    );

    // 5. Ranks #2 (`CreatorTwo/links`), #3 (`CreatorThree`), #4 (`CreatorFour`) have symlinks -> `expected_winner`
    let expected_link_name =
        "CreatorOne & CreatorTwo & CreatorThree & CreatorFour & CreatorFive - DupScene.mkv";
    for sec_dir in [anc2.join("links"), anc3.clone(), anc4.clone()] {
        let lp = sec_dir.join(expected_link_name);
        assert!(
            lp.symlink_metadata().unwrap().file_type().is_symlink(),
            "Expected symlink in {:?}",
            sec_dir
        );
        assert_eq!(fs::read_link(&lp).unwrap(), expected_winner);
    }

    // 6. Rank #5 (`CreatorFive`) received NO symlink
    assert!(fs::read_dir(&anc5).unwrap().next().is_none());

    // 7. `misc_files.txt` lists ONLY `mystery_clip_999.avi`
    let unmatched_txt = fs::read_to_string(&unmatched_out).unwrap();
    assert_eq!(unmatched_txt.trim(), m_unmatched.to_string_lossy());

    // 8. `--record-moves` recorded the direct move into `.states/recorded_moves.jsonl`
    let rec_moves_txt = fs::read_to_string(base.join(".states").join("recorded_moves.jsonl")).unwrap();
    assert!(rec_moves_txt.contains("CreatorTwo - BrandNewClip.mp4"));

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn stranded_temp_hold_recovers_on_startup() {
    let base = make_temp_test_dir("hold_recovery");
    let misc_dir = base.join("misc");
    let hold_dir = base.join(".rusty_scanner_hold_99999999_1_deadbeef");
    fs::create_dir_all(&misc_dir).unwrap();
    fs::create_dir_all(&hold_dir).unwrap();

    let orig_file = misc_dir.join("stranded.mp4");
    let parked_file = hold_dir.join("000001_stranded.mp4");
    fs::write(&parked_file, b"stranded_content").unwrap();

    let manifest_json = serde_json::json!({
        "pid": 99999999u32,
        "entries": [{
            "original_path": orig_file,
            "parked_path": parked_file,
        }]
    });
    fs::write(
        hold_dir.join("hold_manifest.json"),
        serde_json::to_string_pretty(&manifest_json).unwrap(),
    )
    .unwrap();

    recover_stranded_temp_holds(std::slice::from_ref(&misc_dir), &[], true);

    assert!(orig_file.is_file());
    assert!(!hold_dir.exists());

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn non_short_circuiting_dir_and_title_fusion_with_nlp_noun_weighting() {
    let yaml = r#"
anchors:
  /Anchors/abbie cat_h265: {}
  /Anchors/candid_h265: {}
  /Anchors/kendra_h265: {}
  /Anchors/Mistress Mira_h265: {}
  /Anchors/Goddess Alexandra Snow_h265: {}
"#;
    let anchor_cfg = AnchorDirsConfig::from_yaml_str(yaml, Path::new("nlp_test.yaml")).unwrap();
    let matcher = AnchorMatcher::new(&anchor_cfg.anchors, MatchingConfig::default());

    let mk_cand = |p: &str, root: Option<&str>| MiscVideoCandidate {
        path: PathBuf::from(p),
        dev_inode: (1, 1),
        size_bytes: 1024,
        misc_root: root.map(PathBuf::from),
    };

    // 1. No short-circuiting: when BOTH directory (`Abbie Cat H265`) AND filename (`Abbie Cat - My Day!`)
    //    match `abbie cat_h265`, it fuses into `DIR_AND_TITLE` with higher confidence than dir-only (`1.0000 > 0.9700`)!
    let r_both = matcher.match_single(&mk_cand(
        "/Volumes/f1m/f/z_fdc_h265/downloads/Abbie Cat H265/Abbie Cat - My Day!_h265.mp4",
        Some("/Volumes/f1m/f/z_fdc_h265/downloads/Abbie Cat H265"),
    ));
    let r_dir_only = matcher.match_single(&mk_cand(
        "/Volumes/f1m/f/z_fdc_h265/downloads/Abbie Cat H265/Candid Smokingk_h265.mp4",
        Some("/Volumes/f1m/f/z_fdc_h265/downloads/Abbie Cat H265"),
    ));
    assert_eq!(r_both.matches.len(), 1);
    assert_eq!(r_both.matches[0].match_tier, MatchTier::DirAndTitle);
    assert_eq!(r_dir_only.matches.len(), 1);
    assert_eq!(r_dir_only.matches[0].match_tier, MatchTier::ExactSubdir);
    assert!(
        r_both.matches[0].confidence_score > r_dir_only.matches[0].confidence_score,
        "Corroborated dir+title match ({}) must score higher than dir-only match ({})",
        r_both.matches[0].confidence_score,
        r_dir_only.matches[0].confidence_score
    );

    // 2. Notice in `r_dir_only` (`Candid Smokingk_h265.mp4`), Adjective `'Candid'` was down-weighted
    //    below `min_effective_score` so `candid_h265` did NOT false-match! Same for loose `/downloads`:
    let r_adj = matcher.match_single(&mk_cand(
        "/downloads/Candid Smokingk_h265.mp4",
        Some("/downloads"),
    ));
    assert!(
        r_adj.matches.is_empty(),
        "Adjective 'Candid' in 'Candid Smokingk' must be down-weighted below threshold"
    );

    // 3. Proper Noun `'Kendra'` in `'Kendra Handsfree Smoking_h265.mp4'` inside `'Abbie Cat H265'`:
    //    Because title matching was NOT short-circuited, BOTH `abbie cat_h265` (`0.9700`, Rank #1 `PRIMARY_MOVE`)
    //    AND `kendra_h265` (`0.9025`, Rank #2 `SYMLINK`) are discovered and ranked!
    let r_abbie_kendra = matcher.match_single(&mk_cand(
        "/Volumes/f1m/f/z_fdc_h265/downloads/Abbie Cat H265/Kendra Handsfree Smoking_h265.mp4",
        Some("/Volumes/f1m/f/z_fdc_h265/downloads/Abbie Cat H265"),
    ));
    assert_eq!(r_abbie_kendra.matches.len(), 2);
    assert_eq!(r_abbie_kendra.matches[0].anchor_idx, 0); // Abbie Cat -> Rank #1
    assert_eq!(
        r_abbie_kendra.matches[0].planned_action,
        PlannedAction::PrimaryMove
    );
    assert_eq!(r_abbie_kendra.matches[1].anchor_idx, 2); // Kendra (ProperNoun) -> Rank #2 Symlink
    assert_eq!(
        r_abbie_kendra.matches[1].planned_action,
        PlannedAction::Symlink
    );

    // 4. Honorific + ProperNoun fuzzy guard: `"Mistress Mia"` does NOT false-match `"Mistress Mira"`,
    //    whereas `"Goddess Allexandra Snow"` DOES fuzzy-match `"Goddess Alexandra Snow"`!
    let r_mia = matcher.match_single(&mk_cand(
        "/downloads/Mistress Mia - Boots.mp4",
        Some("/downloads"),
    ));
    assert!(
        r_mia.matches.is_empty(),
        "Shared low-weight honorific 'Mistress' must not cause 'Mia' to fuzzy-match 'Mira'"
    );

    let r_alex = matcher.match_single(&mk_cand(
        "/downloads/Goddess Allexandra Snow - Scene.mp4",
        Some("/downloads"),
    ));
    assert_eq!(r_alex.matches.len(), 1);
    assert_eq!(r_alex.matches[0].anchor_idx, 4);
    assert_eq!(r_alex.matches[0].match_tier, MatchTier::FuzzyClause);
}
