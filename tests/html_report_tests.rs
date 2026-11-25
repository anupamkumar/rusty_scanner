use rusty_scanner::config::{AnchorDirsConfig, ResolvedTools, ScannerConfig};
use rusty_scanner::html_report::{RunManifest, rebuild_html_from_run_dir};
use rusty_scanner::matcher::AnchorMatcher;
use rusty_scanner::orchestrator::run_live_loop;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static HTML_TEST_SEQ: AtomicUsize = AtomicUsize::new(1);

fn make_report_test_dir(label: &str) -> PathBuf {
    let seq = HTML_TEST_SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "rusty_scanner_html_test_{}_{}_{}",
        label,
        std::process::id(),
        seq
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(unix)]
#[test]
fn persists_all_dedup_and_clean_reports_and_builds_drilldown_html_and_rebuilds_offline() {
    let base = make_report_test_dir("persistent_reports");
    let misc_dir = base.join("misc");
    let anc_upgrade = base.join("Abbie Cat_h265");
    let anc_direct = base.join("Kendra_h265");
    let mock_bins = base.join("bins");
    let reports_root = base.join("reports");

    for d in [&misc_dir, &anc_upgrade, &anc_direct, &mock_bins] {
        fs::create_dir_all(d).unwrap();
    }

    // Existing lower-quality WMV copy in `Abbie Cat_h265` (size 2,000,000 simulated in JSON)
    let old_anchor_copy = anc_upgrade.join("Spicy_Interview_old.wmv");
    fs::write(&old_anchor_copy, b"old_wmv_bytes").unwrap();

    // Incoming higher-quality H265 copy in `misc` that also co-stars `Kendra` (Rank #2 symlink!)
    let incoming_hq = misc_dir.join("Abbie Cat & Kendra - Spicy Interview_h265.mp4");
    fs::write(&incoming_hq, b"new_h265_hq_bytes").unwrap();

    // Incoming brand-new unique video for `Kendra_h265` (0 duplicates -> direct move, still saves `dups.json`!)
    let incoming_unique = misc_dir.join("Kendra - Solo Clip_h265.mp4");
    fs::write(&incoming_unique, b"kendra_unique_bytes").unwrap();

    // Unmatched file with `</script>` substring in path (`mystery_</script>_clip.mp4`) to verify HTML JSON-escape safety!
    let unmatched_tricky = misc_dir.join("mystery_<").join("script>_clip.mp4");
    fs::create_dir_all(unmatched_tricky.parent().unwrap()).unwrap();
    fs::write(&unmatched_tricky, b"unmatched_bytes").unwrap();

    // Mock `rusty_dedup` emitting full `VideoAttributes` schema
    let mock_dedup = mock_bins.join("rusty_dedup");
    let dedup_py = r#"#!/usr/bin/env python3
import json, os, sys

out_path = "duplicates.json"
cat_log = None
inputs = []
i = 1
while i < len(sys.argv):
    a = sys.argv[i]
    if a in ("-o", "--output"):
        out_path = sys.argv[i + 1]
        i += 2
    elif a == "--categories-log":
        cat_log = sys.argv[i + 1]
        i += 2
    elif a in ("-c", "--dedup-config", "--config", "--states-dir"):
        i += 2
    elif a.startswith("-"):
        i += 1
    else:
        inputs.append(a)
        i += 1

anchor_dir = os.path.realpath(inputs[0])
misc_files = [os.path.realpath(p) for p in inputs[1:]]
old_wmv = os.path.realpath(os.path.join(anchor_dir, "Spicy_Interview_old.wmv"))
hq_cand = [p for p in misc_files if "Spicy Interview" in os.path.basename(p)]

dups = []
pairs = []
if hq_cand and os.path.exists(old_wmv):
    m_path = hq_cand[0]
    dups.append({
        "dup_group_name": "Spicy_Interview_old==Abbie Cat & Kendra - Spicy Interview_h265",
        "files": [old_wmv, m_path],
        "attributes": {
            old_wmv: {
                "size_in_bytes": 1200000000,
                "runtime_in_seconds": 841,
                "video_height": 720,
                "video_width": 1280,
                "video_codec": "WMV",
                "audio_codec": "MP3",
                "video_bitrate_bps": 1950000,
                "audio_bitrate_bps": 128000
            },
            m_path: {
                "size_in_bytes": 520000000,
                "runtime_in_seconds": 842,
                "video_height": 1080,
                "video_width": 1920,
                "video_codec": "H265",
                "audio_codec": "AAC",
                "video_bitrate_bps": 4820000,
                "audio_bitrate_bps": 192000
            }
        }
    })
    pairs.append({
        "file1": old_wmv,
        "file2": m_path,
        "initial_category": "Strong chance of duplicate pair",
        "final_outcome": "Confirmed duplicate",
        "confidence": 0.994,
        "matched_frames": 837,
        "compared_runtime_secs": 841,
        "best_offset_secs": 1,
        "detection_method": "dHash 1-fps sliding window"
    })

with open(out_path, "w") as f:
    json.dump({"input_directories": [anchor_dir] + misc_files, "duplicates": dups}, f, indent=2)

if cat_log:
    with open(cat_log, "w") as f:
        json.dump(pairs, f, indent=2)
"#;
    fs::write(&mock_dedup, dedup_py).unwrap();
    fs::set_permissions(&mock_dedup, fs::Permissions::from_mode(0o755)).unwrap();

    // Mock `rusty_clean` emitting full 8-attribute `GroupDecisionReport` schema AND mutating its input `dup_json`
    // (verifying that our persistent `dedup/*_dups.json` stays untouched because we pass a scratch copy!)
    let mock_clean = mock_bins.join("rusty_clean");
    let clean_py = r#"#!/usr/bin/env python3
import json, os, shutil, sys

dup_json = sys.argv[1]
target_dir = sys.argv[sys.argv.index("-t") + 1]
report_out = sys.argv[sys.argv.index("-o") + 1]

with open(dup_json) as f:
    data = json.load(f)

reports = []
for grp in data.get("duplicates", []):
    old_wmv, m_path = grp["files"][0], grp["files"][1]
    if os.path.exists(old_wmv):
        os.remove(old_wmv)
    dest_winner = os.path.join(target_dir, os.path.basename(m_path))
    shutil.move(m_path, dest_winner)

    # Mutate input dup_json just like real `rusty_clean -t` does
    grp["files"] = ["MUTATED_BY_CLEAN"]

    reports.append({
        "dup_group_name": grp["dup_group_name"],
        "group_size": 2,
        "retained_winner": dest_winner,
        "winner_score": 8.8182,
        "tie_broken_by_shortest_path": False,
        "scores": [
            {
                "file_path": old_wmv,
                "size_raw": 1200000000, "size_points": 1, "size_normalized": 0.5,
                "runtime_raw": 841, "runtime_points": 1, "runtime_normalized": 0.5,
                "height_raw": 720, "height_points": 1, "height_normalized": 0.5,
                "width_raw": 1280, "width_points": 1, "width_normalized": 0.5,
                "video_bitrate_raw": 1950000, "video_bitrate_points": 1, "video_bitrate_normalized": 1.0,
                "audio_bitrate_raw": 128000, "audio_bitrate_points": 1, "audio_bitrate_normalized": 0.5,
                "video_codec_raw": "WMV", "video_codec_points": 6.0, "video_codec_normalized": 0.5455,
                "audio_codec_raw": "MP3", "audio_codec_points": 7.0, "audio_codec_normalized": 0.6364,
                "total_normalized_score": 4.6819
            },
            {
                "file_path": m_path,
                "size_raw": 520000000, "size_points": 2, "size_normalized": 1.0,
                "runtime_raw": 842, "runtime_points": 2, "runtime_normalized": 1.0,
                "height_raw": 1080, "height_points": 2, "height_normalized": 1.0,
                "width_raw": 1920, "width_points": 2, "width_normalized": 1.0,
                "video_bitrate_raw": 4820000, "video_bitrate_points": 2, "video_bitrate_normalized": 2.0,
                "audio_bitrate_raw": 192000, "audio_bitrate_points": 2, "audio_bitrate_normalized": 1.0,
                "video_codec_raw": "H265", "video_codec_points": 10.0, "video_codec_normalized": 0.9091,
                "audio_codec_raw": "AAC", "audio_codec_points": 10.0, "audio_codec_normalized": 0.9091,
                "total_normalized_score": 8.8182
            }
        ],
        "actions": [
            {
                "file_path": m_path,
                "decision": "RETAIN & MOVE TO TARGET",
                "action_taken": f"Moved retained winner to {dest_winner}",
                "moved_to": dest_winner,
                "symlink_created": None,
                "symlink_target": None
            },
            {
                "file_path": old_wmv,
                "decision": "DELETE",
                "action_taken": "Deleted duplicate file",
                "moved_to": None,
                "symlink_created": None,
                "symlink_target": None
            }
        ]
    })

with open(dup_json, "w") as f:
    json.dump(data, f)
with open(report_out, "w") as f:
    json.dump(reports, f, indent=2)
"#;
    fs::write(&mock_clean, clean_py).unwrap();
    fs::set_permissions(&mock_clean, fs::Permissions::from_mode(0o755)).unwrap();

    let anchors_yaml = format!(
        "anchors:\n  \"{}\": {{}}\n  \"{}\": {{}}\n",
        anc_upgrade.display(),
        anc_direct.display()
    );
    let anchor_cfg =
        AnchorDirsConfig::from_yaml_str(&anchors_yaml, Path::new("anchors.yaml")).unwrap();
    let mut scanner_cfg = ScannerConfig::default();
    scanner_cfg.reports.enabled = true;
    scanner_cfg.reports.output_dir = reports_root.clone();
    scanner_cfg.reports.capture_pair_categories_log = true;

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

    let unmatched_out = base.join("un.txt");
    let stats = run_live_loop(
        std::slice::from_ref(&misc_dir),
        &anchor_cfg,
        &scanner_cfg,
        &matcher,
        &tools,
        None,
        &unmatched_out,
        false,
    )
    .unwrap();

    // 1. Verify persistent `dedup/` and `clean/` job reports were saved and NEVER deleted!
    let run_dir = &stats.report_run_dir;
    assert!(run_dir.is_dir());
    let dedup_saved: Vec<PathBuf> = fs::read_dir(run_dir.join("dedup"))
        .unwrap()
        .filter_map(|e| e.ok().map(|x| x.path()))
        .collect();
    let clean_saved: Vec<PathBuf> = fs::read_dir(run_dir.join("clean"))
        .unwrap()
        .filter_map(|e| e.ok().map(|x| x.path()))
        .collect();

    // 2 dedup runs (`_dups.json`) + 2 `--categories-log` (`_pairs.json`) = 4 files in `dedup/`
    assert_eq!(
        dedup_saved
            .iter()
            .filter(|p| p.to_string_lossy().ends_with("_dups.json"))
            .count(),
        2,
        "Both rusty_dedup JSON reports must be preserved on disk"
    );
    assert_eq!(
        dedup_saved
            .iter()
            .filter(|p| p.to_string_lossy().ends_with("_pairs.json"))
            .count(),
        2,
        "Both --categories-log pair JSON reports must be preserved on disk"
    );
    // 1 clean run (`_clean.json`), zero leftover `.scratch_input_*` files!
    assert_eq!(clean_saved.len(), 1);
    assert!(clean_saved[0].to_string_lossy().ends_with("_clean.json"));

    // Verify saved `dedup/*_dups.json` stayed 100% pristine (NOT mutated by `rusty_clean`)
    for dp in dedup_saved
        .iter()
        .filter(|p| p.to_string_lossy().ends_with("_dups.json"))
    {
        let txt = fs::read_to_string(dp).unwrap();
        assert!(
            !txt.contains("MUTATED_BY_CLEAN"),
            "Saved dedup report must remain pristine raw output of rusty_dedup"
        );
    }

    // 2. Verify `run_manifest.json` outcome classification (`ANCHOR_UPGRADED`), reclaimed bytes, & symlinks
    let manifest_raw = fs::read_to_string(run_dir.join("run_manifest.json")).unwrap();
    let manifest: RunManifest = serde_json::from_str(&manifest_raw).unwrap();
    assert_eq!(manifest.status, "COMPLETED");
    assert_eq!(manifest.totals.anchor_batches, 2);
    assert_eq!(manifest.totals.dedup_runs, 2);
    assert_eq!(manifest.totals.clean_runs, 1);
    assert_eq!(manifest.totals.anchor_upgrades, 1);
    assert_eq!(manifest.totals.duplicates_deleted, 1);
    assert_eq!(manifest.totals.bytes_reclaimed, 1_200_000_000);
    assert_eq!(manifest.totals.symlinks_created, 1);
    assert_eq!(manifest.totals.unmatched_count, 1);

    // 3. Verify `execution_report.html` and `reports/latest_report.html` exist and safely escape `</script>`
    let html_path = stats.html_report_path.as_ref().unwrap();
    assert!(html_path.is_file());
    assert!(reports_root.join("latest_report.html").is_file());
    let html_text = fs::read_to_string(html_path).unwrap();
    assert!(html_text.contains("ANCHOR_UPGRADED"));
    assert!(html_text.contains("Spicy_Interview_old.wmv"));
    assert!(html_text.contains("dHash 1-fps sliding window"));
    assert!(
        !html_text.contains("mystery_</script>_clip.mp4"),
        "Raw unescaped `</script>` must never appear inside embedded HTML JSON!"
    );
    assert!(html_text.contains("mystery_<\\/script>_clip.mp4"));

    // 4. Delete `run_manifest.json` and `execution_report.html` to test standalone `--build-report <RUN_DIR>`
    //    reconstructing directly from raw `dedup/*.json` + `clean/*.json` files!
    fs::remove_file(run_dir.join("run_manifest.json")).unwrap();
    fs::remove_file(html_path).unwrap();

    let rebuilt_html = rebuild_html_from_run_dir(run_dir).unwrap();
    assert!(rebuilt_html.is_file());
    assert!(run_dir.join("run_manifest.json").is_file());
    let rebuilt_text = fs::read_to_string(&rebuilt_html).unwrap();
    assert!(rebuilt_text.contains("REBUILT_FROM_JSON"));
    assert!(rebuilt_text.contains("ANCHOR_UPGRADED"));
    assert!(rebuilt_text.contains("1200000000"));

    // 5. Test lightweight HTTP report server (`BackgroundReportServer` lifecycle + HTTP router):
    //    - `GET /` -> 200 OK Multi-Run Hub HTML with auto-refresh
    //    - `GET /api/runs` -> 200 OK JSON listing our run
    //    - `GET /latest` -> 302 Redirect to `/<run_id>/execution_report.html`
    //    - `GET /<run_id>/execution_report.html` -> 200 OK interactive dashboard with auto-refresh
    //    - `GET /..%2F..%2Fetc%2Fpasswd` -> 403 Forbidden (path traversal blocked!)
    let srv_cfg = rusty_scanner::config::ReportServerConfig {
        enabled: true,
        listen_addr: "127.0.0.1:0".to_string(),
        serve_during_run: true,
        linger_after_run_secs: 0,
        live_refresh_secs: 5,
    };
    if let Some(mut srv) =
        rusty_scanner::report_server::BackgroundReportServer::try_start(&reports_root, &srv_cfg)
    {
        assert!(srv.local_addr.port() > 0);
        srv.stop();
    }

    let http_get = |path: &str| -> String {
        let req = format!("GET {} HTTP/1.1\r\nHost: localhost\r\n\r\n", path);
        let bytes = rusty_scanner::report_server::handle_http_request_in_memory(
            req.as_bytes(),
            &reports_root,
            5,
        )
        .unwrap();
        String::from_utf8(bytes).unwrap()
    };

    let r_hub = http_get("/");
    assert!(r_hub.starts_with("HTTP/1.1 200 OK"));
    assert!(r_hub.contains("Execution Reports Hub"));
    assert!(r_hub.contains("HUB_POLL_SECS = 5;"));

    let r_api = http_get("/api/runs");
    assert!(r_api.starts_with("HTTP/1.1 200 OK"));
    assert!(r_api.contains(&manifest.run_id));

    let r_latest = http_get("/latest");
    assert!(r_latest.starts_with("HTTP/1.1 302 Found"));
    assert!(r_latest.contains(&format!(
        "Location: /{}/execution_report.html",
        manifest.run_id
    )));

    let r_dash = http_get(&format!("/{}/execution_report.html", manifest.run_id));
    assert!(r_dash.starts_with("HTTP/1.1 200 OK"));
    assert!(r_dash.contains("ANCHOR_UPGRADED"));
    assert!(r_dash.contains("refreshReportTick"));

    let r_traversal = http_get("/..%2F..%2Fetc%2Fpasswd");
    assert!(r_traversal.starts_with("HTTP/1.1 403 Forbidden"));

    let _ = fs::remove_dir_all(&base);
}
