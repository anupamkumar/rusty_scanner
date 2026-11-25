use anyhow::Result;
use clap::Parser;
use rusty_scanner::config::{AnchorDirsConfig, ScannerConfig};
use rusty_scanner::csv_report::{
    print_match_summary, read_matches_csv, write_matches_csv, write_unmatched_files,
};
use rusty_scanner::html_report::rebuild_html_from_run_dir;
use rusty_scanner::matcher::{AnchorMatcher, BatchMatchOutcome};
use rusty_scanner::orchestrator::{execute_matched_files, run_live_loop};
use rusty_scanner::report_server::{run_standalone_report_server, validate_listen_addr};
use rusty_scanner::scanner::discover_misc_videos;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(
    name = "rusty_scanner",
    version = "0.1.0",
    about = "Rusty Intelli Scanner: matches miscellaneous video files to anchor directories (with aliases & custom destinations), outputs CSV previews in --dry-run mode, and orchestrates rusty_dedup + rusty_clean consolidation with Rank #2-#4 symlinks."
)]
struct Args {
    /// One or more miscellaneous video directories (scanned recursively) and/or individual video files
    #[arg(
        value_name = "INPUTS",
        required_unless_present_any = ["from_csv", "build_report", "serve_reports"]
    )]
    inputs: Vec<PathBuf>,

    /// Path to the map-style anchor directories YAML file
    #[arg(short = 'a', long = "anchors", default_value = "anchor_dirs.yaml")]
    anchors: PathBuf,

    /// Optional path to scanner_config.yaml (defaults to ./scanner_config.yaml or <exe_dir>/scanner_config.yaml)
    #[arg(short = 'c', long = "config")]
    config: Option<PathBuf>,

    /// Execute curated matches directly from a CSV file (such as `manual_matches.csv` exported by `rusty_triage`)
    #[arg(long = "from-csv", value_name = "CSV_PATH")]
    from_csv: Option<PathBuf>,

    /// Override output directory where per-run dedup/clean JSON reports and `execution_report.html` are saved
    #[arg(long = "report-dir", value_name = "REPORTS_DIR")]
    report_dir: Option<PathBuf>,

    /// Standalone HTML generator: rebuild `execution_report.html` from an existing saved `reports/run_<timestamp>` directory and exit
    #[arg(long = "build-report", value_name = "RUN_DIR")]
    build_report: Option<PathBuf>,

    /// Start the lightweight HTTP report server (`report_server` in scanner_config.yaml) so dashboards can be viewed from any browser
    #[arg(long = "serve-reports", default_value_t = false)]
    serve_reports: bool,

    /// Override `report_server.listen_addr` (e.g. `0.0.0.0:8787`)
    #[arg(long = "listen", value_name = "HOST:PORT")]
    listen: Option<String>,

    /// Preview mode: produce the CSV table of anchor matches without moving files or invoking rusty_dedup/rusty_clean
    #[arg(long = "dry-run", default_value_t = false)]
    dry_run: bool,

    /// Output CSV file path (defaults to `anchor_matches.csv` in --dry-run mode; pass `-` for stdout; optional audit CSV in live mode)
    #[arg(short = 'o', long = "csv-out")]
    csv_out: Option<PathBuf>,

    /// Output text file path listing remaining unmatched miscellaneous video files
    #[arg(short = 'u', long = "unmatched-out", default_value = "misc_files.txt")]
    unmatched_out: PathBuf,

    /// Enable verbose diagnostics (-v / --verbose)
    #[arg(short = 'v', long = "verbose", default_value_t = false)]
    verbose: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Standalone Mode 1: Rebuild interactive HTML dashboard from a saved run directory
    if let Some(ref run_dir) = args.build_report {
        let html_path = rebuild_html_from_run_dir(run_dir)?;
        println!(
            "Rebuilt interactive HTML audit report: {:?}",
            html_path
        );
        return Ok(());
    }

    let (mut scanner_cfg, loaded_cfg_path) =
        ScannerConfig::load_or_default(args.config.as_deref(), args.verbose)?;
    if let Some(ref custom_rep_dir) = args.report_dir {
        scanner_cfg.reports.output_dir = custom_rep_dir.clone();
    }
    if let Some(ref custom_listen) = args.listen {
        validate_listen_addr(custom_listen)?;
        scanner_cfg.report_server.listen_addr = custom_listen.clone();
    }

    // Standalone Mode 2: Host `<reports.output_dir>` over HTTP (`--serve-reports`)
    if args.serve_reports {
        return run_standalone_report_server(
            &scanner_cfg.reports.output_dir,
            &scanner_cfg.report_server,
        );
    }

    let anchor_cfg = AnchorDirsConfig::load(&args.anchors)?;

    if args.verbose {
        eprintln!(
            "[Verbose] Loaded {} anchor(s) from {:?} (version {})",
            anchor_cfg.anchors.len(),
            args.anchors,
            anchor_cfg.version
        );
        eprintln!(
            "[Verbose] Scanner config source: {:?}",
            loaded_cfg_path
                .as_deref()
                .unwrap_or_else(|| Path::new("<built-in defaults>"))
        );
    }

    for anc in &anchor_cfg.anchors {
        if !anc.anchor_path.exists() {
            eprintln!(
                "Warning: Configured anchor directory {:?} does not currently exist on disk.",
                anc.anchor_path
            );
        }
    }

    // Mode 1: Execute curated assignments from `--from-csv <CSV_PATH>`
    if let Some(ref csv_input_path) = args.from_csv {
        let curated_matches = read_matches_csv(csv_input_path, &anchor_cfg)?;
        let outcome = BatchMatchOutcome {
            matched_files: curated_matches,
            unmatched_files: Vec::new(),
        };
        print_match_summary(&outcome, &anchor_cfg.anchors, "FROM-CSV CURATED BATCH");
        if args.dry_run {
            eprintln!(
                "[Dry-Run] Validated {} curated file assignment(s) from {:?}; no disk changes made.",
                outcome.matched_files.len(),
                csv_input_path
            );
            return Ok(());
        }
        let tools = scanner_cfg.resolve_tools()?;
        execute_matched_files(
            &outcome.matched_files,
            &anchor_cfg,
            &scanner_cfg,
            &tools,
            args.verbose,
        )?;
        return Ok(());
    }

    let matcher = AnchorMatcher::new(&anchor_cfg.anchors, scanner_cfg.matching.clone());

    if args.dry_run {
        let csv_target = args
            .csv_out
            .clone()
            .unwrap_or_else(|| PathBuf::from("anchor_matches.csv"));

        let candidates = discover_misc_videos(
            &args.inputs,
            &anchor_cfg.anchors,
            &scanner_cfg.video_extensions,
            &HashSet::new(),
            true,
            args.verbose,
        );
        let outcome = matcher.match_batch(&candidates);
        let rows = write_matches_csv(&outcome, &csv_target)?;
        let unmatched_count =
            write_unmatched_files(&outcome.unmatched_files, &args.unmatched_out)?;
        print_match_summary(&outcome, &anchor_cfg.anchors, "DRY-RUN PREVIEW");

        if csv_target != Path::new("-") {
            eprintln!(
                "[Dry-Run] Wrote {} match row(s) across {} matched file(s) to {:?}.",
                rows,
                outcome.matched_files.len(),
                csv_target
            );
        }
        eprintln!(
            "[Dry-Run] Wrote {} unmatched file path(s) to {:?}.",
            unmatched_count, args.unmatched_out
        );
        return Ok(());
    }

    let tools = scanner_cfg.resolve_tools()?;
    if args.verbose {
        eprintln!("[Verbose] Resolved tools:");
        eprintln!("  • python_bin      : {:?}", tools.python_bin);
        eprintln!("  • metrics_harness : {:?}", tools.dedup_metrics_harness);
        eprintln!("  • harness_linger  : {:?}", tools.harness_linger_secs);
        eprintln!("  • rusty_dedup     : {:?}", tools.rusty_dedup_bin);
        eprintln!("  • rusty_clean     : {:?}", tools.rusty_clean_bin);
        eprintln!("  • dedup_config    : {:?}", tools.dedup_config);
        eprintln!("  • decision_config : {:?}", tools.decision_config);
        eprintln!("  • states_dir      : {:?}", tools.states_dir);
    }

    run_live_loop(
        &args.inputs,
        &anchor_cfg,
        &scanner_cfg,
        &matcher,
        &tools,
        args.csv_out.as_deref(),
        &args.unmatched_out,
        args.verbose,
    )?;

    Ok(())
}
