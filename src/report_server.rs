use crate::config::{ReportServerConfig, normalize_path_lexical};
use crate::html_report::{
    RunManifest, RunTotals, reconcile_and_persist_manifest, refresh_latest_report_shortcuts,
};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const MAX_HTTP_HEADER_BYTES: usize = 8192;

/// Summary metadata for a single execution run exposed by `/api/runs` and rendered on `/`.
#[derive(Debug, Clone, Serialize)]
pub struct RunHubEntry {
    pub run_id: String,
    pub mode: String,
    pub status: String,
    pub started_at_utc: String,
    pub started_at_unix_ms: u64,
    pub updated_at_utc: String,
    pub elapsed_secs: f64,
    pub report_url: String,
    pub manifest_url: String,
    pub totals: RunTotals,
}

/// RAII handle for an in-process background HTTP report server spawned during a live run.
pub struct BackgroundReportServer {
    pub local_addr: SocketAddr,
    stop_flag: Arc<AtomicBool>,
    join_handle: Option<JoinHandle<()>>,
}

impl BackgroundReportServer {
    /// Attempts to start the HTTP report server in a background thread.
    /// If `listen_addr` is already bound (e.g. a persistent `rusty_scanner --serve-reports`
    /// daemon is already running) or restricted, logs a friendly message and returns `None`
    /// without interrupting video consolidation.
    pub fn try_start(reports_root: &Path, cfg: &ReportServerConfig) -> Option<Self> {
        if !cfg.enabled || !cfg.serve_during_run {
            return None;
        }
        let root_norm = normalize_path_lexical(reports_root);
        let _ = fs::create_dir_all(&root_norm);

        match TcpListener::bind(&cfg.listen_addr) {
            Ok(listener) => {
                let local_addr = listener.local_addr().ok()?;
                let _ = listener.set_nonblocking(true);
                let stop_flag = Arc::new(AtomicBool::new(false));
                let flag_clone = Arc::clone(&stop_flag);
                let live_refresh = cfg.live_refresh_secs;
                let root_for_thread = root_norm.clone();

                print_server_urls("Background Report Server", local_addr, &root_norm);

                let handle = thread::Builder::new()
                    .name("rusty-report-http".to_string())
                    .spawn(move || {
                        accept_loop(listener, &root_for_thread, live_refresh, Some(flag_clone));
                    })
                    .ok()?;

                Some(Self {
                    local_addr,
                    stop_flag,
                    join_handle: Some(handle),
                })
            }
            Err(e) if e.kind() == ErrorKind::AddrInUse => {
                eprintln!(
                    "[Report Server] {} is already active (persistent --serve-reports server running); dashboard is live at {}",
                    cfg.listen_addr,
                    format_preferred_url_from_str(&cfg.listen_addr)
                );
                None
            }
            Err(e) => {
                eprintln!(
                    "[Report Server] Could not bind {} ({}); HTML files will still be written to {:?}",
                    cfg.listen_addr, e, root_norm
                );
                None
            }
        }
    }

    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        // Nudge loopback so `accept` wakes immediately if blocking
        let wakeup_target = if self.local_addr.ip().is_unspecified() {
            SocketAddr::from(([127, 0, 0, 1], self.local_addr.port()))
        } else {
            self.local_addr
        };
        let _ = TcpStream::connect_timeout(&wakeup_target, Duration::from_millis(50));
        if let Some(h) = self.join_handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for BackgroundReportServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Runs the standalone foreground HTTP report server (`rusty_scanner --serve-reports`) indefinitely.
pub fn run_standalone_report_server(reports_root: &Path, cfg: &ReportServerConfig) -> Result<()> {
    let root_norm = normalize_path_lexical(reports_root);
    fs::create_dir_all(&root_norm).with_context(|| {
        format!(
            "Failed to create reports directory {:?} for HTTP server",
            root_norm
        )
    })?;

    let listener = TcpListener::bind(&cfg.listen_addr).with_context(|| {
        format!(
            "Failed to bind HTTP report server on '{}'. Change report_server.listen_addr in scanner_config.yaml or pass --listen <HOST:PORT>.",
            cfg.listen_addr
        )
    })?;
    let local_addr = listener.local_addr()?;
    print_server_urls("Rusty Intelli Scanner — HTTP Report Server", local_addr, &root_norm);
    println!("  Press Ctrl-C to stop.\n");

    accept_loop(listener, &root_norm, cfg.live_refresh_secs, None);
    Ok(())
}

fn accept_loop(
    listener: TcpListener,
    reports_root: &Path,
    live_refresh_secs: u64,
    stop_flag: Option<Arc<AtomicBool>>,
) {
    loop {
        if let Some(ref flag) = stop_flag {
            if flag.load(Ordering::Relaxed) {
                break;
            }
        }
        match listener.accept() {
            Ok((stream, _peer)) => {
                if let Some(ref flag) = stop_flag {
                    if flag.load(Ordering::Relaxed) {
                        break;
                    }
                }
                let root = reports_root.to_path_buf();
                thread::spawn(move || {
                    let _ = handle_http_connection(stream, &root, live_refresh_secs);
                });
            }
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(120));
            }
            Err(_) => {
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn handle_http_connection(
    mut stream: TcpStream,
    reports_root: &Path,
    live_refresh_secs: u64,
) -> Result<()> {
    // Crucial on macOS/BSD: accepted sockets inherit `O_NONBLOCK` from a nonblocking listener!
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(15)));

    let mut buf = [0u8; MAX_HTTP_HEADER_BYTES];
    let mut read_total = 0usize;
    loop {
        if read_total >= MAX_HTTP_HEADER_BYTES {
            break;
        }
        match stream.read(&mut buf[read_total..]) {
            Ok(0) => break,
            Ok(n) => {
                read_total += n;
                if buf[..read_total].windows(4).any(|w| w == b"\r\n\r\n")
                    || buf[..read_total].windows(2).any(|w| w == b"\n\n")
                {
                    break;
                }
            }
            Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    if read_total == 0 {
        return Ok(());
    }

    route_and_write_http_response(&buf[..read_total], &mut stream, reports_root, live_refresh_secs)
}

/// Processes a raw HTTP/1.1 request against `reports_root` and returns the complete HTTP response bytes.
pub fn handle_http_request_in_memory(
    raw_request: &[u8],
    reports_root: &Path,
    live_refresh_secs: u64,
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    route_and_write_http_response(raw_request, &mut out, reports_root, live_refresh_secs)?;
    Ok(out)
}

fn route_and_write_http_response<W: Write>(
    raw_request: &[u8],
    writer: &mut W,
    reports_root: &Path,
    live_refresh_secs: u64,
) -> Result<()> {
    let req_str = String::from_utf8_lossy(raw_request);
    let first_line = req_str.lines().next().unwrap_or("");
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let raw_target = parts.next().unwrap_or("/");

    if method != "GET" && method != "HEAD" {
        return send_response(
            writer,
            "405 Method Not Allowed",
            "text/plain; charset=utf-8",
            b"Method Not Allowed",
            method == "HEAD",
        );
    }
    let head_only = method == "HEAD";

    // Strip query string (`?t=...`) and fragment
    let path_part = raw_target
        .split('?')
        .next()
        .unwrap_or("/")
        .split('#')
        .next()
        .unwrap_or("/");
    let decoded_path = percent_decode(path_part);

    match decoded_path.as_str() {
        "/" | "/index.html" => {
            let runs = collect_run_hub_entries(reports_root);
            let html = render_runs_hub_html(&runs, live_refresh_secs);
            return send_response(
                writer,
                "200 OK",
                "text/html; charset=utf-8",
                html.as_bytes(),
                head_only,
            );
        }
        "/api/runs" => {
            let runs = collect_run_hub_entries(reports_root);
            let body = serde_json::to_vec(&runs).unwrap_or_else(|_| b"[]".to_vec());
            return send_response(
                writer,
                "200 OK",
                "application/json; charset=utf-8",
                &body,
                head_only,
            );
        }
        "/healthz" => {
            return send_response(
                writer,
                "200 OK",
                "application/json; charset=utf-8",
                b"{\"status\":\"ok\"}",
                head_only,
            );
        }
        "/latest" | "/latest/" | "/latest_report.html" => {
            let runs = collect_run_hub_entries(reports_root);
            if let Some(newest) = runs.first() {
                return send_redirect(writer, &newest.report_url);
            }
            let fallback_latest = reports_root.join("latest_report.html");
            if fallback_latest.is_file() {
                return serve_static_file(writer, &fallback_latest, head_only);
            }
            return send_redirect(writer, "/");
        }
        _ => {}
    }

    // Safely resolve static file inside `reports_root` (strict path-traversal rejection)
    let Some(rel_clean) = sanitize_relative_url_path(&decoded_path) else {
        return send_response(
            writer,
            "403 Forbidden",
            "text/plain; charset=utf-8",
            b"403 Forbidden: Invalid path",
            head_only,
        );
    };

    // Trigger run liveness reconciliation when requesting a run's HTML or manifest directly
    if let Some(first_comp) = rel_clean.components().next() {
        let comp_str = first_comp.as_os_str().to_string_lossy();
        if comp_str.starts_with("run_") {
            let run_dir = reports_root.join(first_comp.as_os_str());
            let mpath = run_dir.join("run_manifest.json");
            if let Ok(raw) = fs::read_to_string(&mpath) {
                if let Ok(mut m) = serde_json::from_str::<RunManifest>(&raw) {
                    let _ = reconcile_and_persist_manifest(&run_dir, &mut m);
                }
            }
        }
    }

    let candidate = if reports_root.join(&rel_clean).is_dir() {
        reports_root.join(&rel_clean).join("execution_report.html")
    } else {
        reports_root.join(&rel_clean)
    };

    if !candidate.is_file() {
        return send_response(
            writer,
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"404 Not Found",
            head_only,
        );
    }

    // Canonical containment verification
    let Ok(can_root) = fs::canonicalize(reports_root) else {
        return send_response(
            writer,
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"404 Not Found",
            head_only,
        );
    };
    let Ok(can_file) = fs::canonicalize(&candidate) else {
        return send_response(
            writer,
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"404 Not Found",
            head_only,
        );
    };
    if !can_file.starts_with(&can_root) {
        return send_response(
            writer,
            "403 Forbidden",
            "text/plain; charset=utf-8",
            b"403 Forbidden",
            head_only,
        );
    }

    serve_static_file(writer, &can_file, head_only)
}

fn sanitize_relative_url_path(decoded_path: &str) -> Option<PathBuf> {
    if decoded_path.contains('\0') || decoded_path.contains('\\') {
        return None;
    }
    let trimmed = decoded_path.trim_start_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let mut clean = PathBuf::new();
    for comp in Path::new(trimmed).components() {
        match comp {
            Component::Normal(seg) => clean.push(seg),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if clean.as_os_str().is_empty() {
        None
    } else {
        Some(clean)
    }
}

fn serve_static_file<W: Write>(writer: &mut W, file_path: &Path, head_only: bool) -> Result<()> {
    let mut file = File::open(file_path)?;
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let content_type = guess_content_type(file_path);

    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store, max-age=0\r\nConnection: close\r\n\r\n",
        content_type, len
    );
    writer.write_all(header.as_bytes())?;
    if !head_only {
        std::io::copy(&mut file, writer)?;
    }
    writer.flush()?;
    Ok(())
}

fn send_response<W: Write>(
    writer: &mut W,
    status: &str,
    content_type: &str,
    body: &[u8],
    head_only: bool,
) -> Result<()> {
    let header = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store, max-age=0\r\nConnection: close\r\n\r\n",
        status,
        content_type,
        body.len()
    );
    writer.write_all(header.as_bytes())?;
    if !head_only {
        writer.write_all(body)?;
    }
    writer.flush()?;
    Ok(())
}

fn send_redirect<W: Write>(writer: &mut W, location: &str) -> Result<()> {
    let header = format!(
        "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\nCache-Control: no-store, max-age=0\r\nConnection: close\r\n\r\n",
        location
    );
    writer.write_all(header.as_bytes())?;
    writer.flush()?;
    Ok(())
}

fn guess_content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "txt" | "log" | "yaml" | "yml" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Discovers all `run_*` directories inside `reports_root`, automatically reconciles dead-PID
/// `"IN_PROGRESS"` runs to `"INTERRUPTED"`, and returns their summaries sorted newest-first.
pub fn collect_run_hub_entries(reports_root: &Path) -> Vec<RunHubEntry> {
    let Ok(rd) = fs::read_dir(reports_root) else {
        return Vec::new();
    };
    let mut runs: Vec<(RunHubEntry, Option<(PathBuf, RunManifest, bool)>)> = Vec::new();

    for entry in rd.filter_map(|e| e.ok()) {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        let folder_name = entry.file_name().to_string_lossy().to_string();
        if !folder_name.starts_with("run_") {
            continue;
        }
        let manifest_path = p.join("run_manifest.json");
        let html_path = p.join("execution_report.html");
        if !manifest_path.is_file() && !html_path.is_file() {
            continue;
        }

        if let Ok(raw) = fs::read_to_string(&manifest_path) {
            if let Ok(mut m) = serde_json::from_str::<RunManifest>(&raw) {
                let changed = reconcile_and_persist_manifest(&p, &mut m);
                let hub_entry = RunHubEntry {
                    report_url: format!("/{}/execution_report.html", folder_name),
                    manifest_url: format!("/{}/run_manifest.json", folder_name),
                    run_id: m.run_id.clone(),
                    mode: m.mode.clone(),
                    status: m.status.clone(),
                    started_at_utc: m.started_at_utc.clone(),
                    started_at_unix_ms: m.started_at_unix_ms,
                    updated_at_utc: m.updated_at_utc.clone(),
                    elapsed_secs: m.elapsed_secs,
                    totals: m.totals.clone(),
                };
                runs.push((hub_entry, Some((p, m, changed))));
                continue;
            }
        }

        runs.push((
            RunHubEntry {
                report_url: format!("/{}/execution_report.html", folder_name),
                manifest_url: format!("/{}/run_manifest.json", folder_name),
                run_id: folder_name,
                mode: "UNKNOWN".to_string(),
                status: "COMPLETED".to_string(),
                started_at_utc: String::new(),
                started_at_unix_ms: 0,
                updated_at_utc: String::new(),
                elapsed_secs: 0.0,
                totals: RunTotals::default(),
            },
            None,
        ));
    }

    runs.sort_by(|(a, _), (b, _)| {
        b.started_at_unix_ms
            .cmp(&a.started_at_unix_ms)
            .then_with(|| b.run_id.cmp(&a.run_id))
    });

    if let Some((_, Some((_, newest_manifest, true)))) = runs.first() {
        refresh_latest_report_shortcuts(reports_root, newest_manifest);
    }

    runs.into_iter().map(|(e, _)| e).collect()
}

fn detect_lan_ip() -> Option<String> {
    // Non-transmitting UDP routing table lookup (zero packets sent on the wire)
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("10.255.255.255:80").ok()?;
    let addr = sock.local_addr().ok()?;
    if addr.ip().is_loopback() || addr.ip().is_unspecified() {
        None
    } else {
        Some(addr.ip().to_string())
    }
}

fn format_preferred_url_from_str(listen_addr: &str) -> String {
    let port = listen_addr
        .rsplit(':')
        .next()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(8787);
    if let Some(ip) = detect_lan_ip() {
        format!("http://{}:{}/", ip, port)
    } else {
        format!("http://127.0.0.1:{}/", port)
    }
}

fn print_server_urls(banner: &str, local_addr: SocketAddr, reports_root: &Path) {
    let port = local_addr.port();
    eprintln!("================================================================================");
    eprintln!("=== {} ===", banner);
    eprintln!("  Serving Directory : {:?}", reports_root);
    eprintln!("  Listening Socket  : {}", local_addr);
    eprintln!("  Local Browser URL : http://127.0.0.1:{}/", port);
    if local_addr.ip().is_unspecified() {
        if let Some(lan_ip) = detect_lan_ip() {
            eprintln!("  Network / LAN URL : http://{}:{}/  (Latest: http://{}:{}/latest)", lan_ip, port, lan_ip, port);
        }
    }
    eprintln!("================================================================================");
}

pub fn validate_listen_addr(addr: &str) -> Result<()> {
    if addr.trim().is_empty() || !addr.contains(':') {
        bail!(
            "Invalid listen address {:?} (expected HOST:PORT such as 0.0.0.0:8787)",
            addr
        );
    }
    Ok(())
}

fn render_runs_hub_html(runs: &[RunHubEntry], live_refresh_secs: u64) -> String {
    let runs_json = serde_json::to_string(runs)
        .unwrap_or_else(|_| "[]".to_string())
        .replace("</", "<\\/")
        .replace("<!--", "<\\!--");

    let mut html = String::with_capacity(runs_json.len() + 18 * 1024);
    html.push_str(HUB_HTML_PREFIX);
    html.push_str(&runs_json);
    html.push_str("</script>\n<script>\nconst HUB_POLL_SECS = ");
    html.push_str(&live_refresh_secs.to_string());
    html.push_str(";\n");
    html.push_str(HUB_HTML_SUFFIX);
    html
}

const HUB_HTML_PREFIX: &str = r#"<!DOCTYPE html>
<html lang="en" data-theme="dark">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Rusty Intelli Scanner — Execution Reports Hub</title>
<style>
:root, [data-theme="dark"] {
  --bg: #0b0f17; --bg-card: #121826; --bg-elevated: #1a2234; --bg-hover: #212c42;
  --border: #28354f; --text: #e6edf7; --text-muted: #93a4bf;
  --accent: #4f9cf9; --accent-soft: rgba(79, 156, 249, 0.15);
  --emerald: #34d399; --emerald-soft: rgba(52, 211, 153, 0.16);
  --amber: #fbbf24; --amber-soft: rgba(251, 191, 36, 0.16);
  --purple: #a78bfa; --purple-soft: rgba(167, 139, 250, 0.15);
  --red: #f87171; --red-soft: rgba(248, 113, 113, 0.16);
  --mono: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  --sans: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
}
[data-theme="light"] {
  --bg: #f4f7fb; --bg-card: #ffffff; --bg-elevated: #eef3f9; --bg-hover: #e3ebf6;
  --border: #d0dbe8; --text: #0f172a; --text-muted: #475569;
  --accent: #2563eb; --accent-soft: rgba(37, 99, 235, 0.11);
  --emerald: #059669; --emerald-soft: rgba(5, 150, 105, 0.12);
  --amber: #d97706; --amber-soft: rgba(217, 119, 6, 0.12);
  --purple: #7c3aed; --purple-soft: rgba(124, 58, 237, 0.11);
  --red: #dc2626; --red-soft: rgba(220, 38, 38, 0.12);
}
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--text); font-family: var(--sans); font-size: 14px; }
.container { max-width: 1420px; margin: 0 auto; padding: 24px 24px 64px; }
.topbar {
  display: flex; flex-wrap: wrap; justify-content: space-between; align-items: center;
  gap: 14px; padding-bottom: 16px; border-bottom: 1px solid var(--border); margin-bottom: 20px;
}
h1 { margin: 0; font-size: 21px; font-weight: 700; }
.sub { margin-top: 4px; font-size: 12.5px; color: var(--text-muted); font-family: var(--mono); }
.btn {
  display: inline-block; text-decoration: none; background: var(--bg-elevated); color: var(--text);
  border: 1px solid var(--border); border-radius: 8px; padding: 8px 14px; font-size: 13px;
  font-weight: 600; cursor: pointer;
}
.btn:hover { background: var(--bg-hover); border-color: var(--accent); }
.btn-primary { background: var(--accent); color: #fff; border-color: var(--accent); }
.btn-primary:hover { opacity: 0.92; color: #fff; }
.kpi-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(210px, 1fr)); gap: 12px; margin-bottom: 20px; }
.kpi { background: var(--bg-card); border: 1px solid var(--border); border-radius: 10px; padding: 14px 16px; }
.kpi-lbl { font-size: 11.5px; text-transform: uppercase; letter-spacing: 0.04em; color: var(--text-muted); font-weight: 600; }
.kpi-val { font-size: 25px; font-weight: 800; font-family: var(--mono); margin-top: 4px; }
.table-card { background: var(--bg-card); border: 1px solid var(--border); border-radius: 10px; overflow-x: auto; }
table { width: 100%; border-collapse: collapse; font-size: 13px; }
th {
  text-align: left; padding: 10px 14px; background: var(--bg-elevated); color: var(--text-muted);
  font-size: 11px; text-transform: uppercase; letter-spacing: 0.04em; border-bottom: 1px solid var(--border);
}
td { padding: 12px 14px; border-bottom: 1px solid var(--border); vertical-align: middle; }
tr:last-child td { border-bottom: none; }
tr:hover td { background: var(--bg-hover); }
.pill {
  display: inline-block; font-size: 11px; font-weight: 700; padding: 3px 9px; border-radius: 999px;
  font-family: var(--mono);
}
.p-done { background: var(--emerald-soft); color: var(--emerald); }
.p-live { background: var(--amber-soft); color: var(--amber); border: 1px solid var(--amber); }
.p-stop { background: var(--red-soft); color: var(--red); border: 1px solid rgba(248, 113, 113, 0.45); }
.mono { font-family: var(--mono); }
.empty { text-align: center; padding: 48px 20px; color: var(--text-muted); }
</style>
</head>
<body>
<div class="container">
  <div class="topbar">
    <div>
      <h1>Rusty Intelli Scanner — Execution Reports Hub</h1>
      <div class="sub" id="hub-sub"></div>
    </div>
    <div style="display:flex; gap:10px; align-items:center;">
      <a class="btn btn-primary" id="btn-open-latest" href="/latest">Open Latest Report →</a>
      <button class="btn" id="btn-theme" type="button">Light / Dark</button>
    </div>
  </div>
  <div class="kpi-grid" id="hub-kpis"></div>
  <div class="table-card" id="hub-table"></div>
</div>
<script id="hub-runs-data" type="application/json">"#;

const HUB_HTML_SUFFIX: &str = r#"
(function() {
  let RUNS = JSON.parse(document.getElementById('hub-runs-data').textContent || '[]');

  function esc(v) {
    return String(v == null ? '' : v)
      .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;').replace(/'/g, '&#39;');
  }
  function fmtBytes(b) {
    const n = Number(b || 0);
    if (!n) return '0 B';
    const u = ['B','KB','MB','GB','TB'];
    let i = 0, v = n;
    while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
    return (i >= 2 ? v.toFixed(2) : v.toFixed(0)) + ' ' + u[i];
  }
  function fmtStart(r) {
    if (r.started_at_unix_ms) {
      try { return new Date(r.started_at_unix_ms).toLocaleString(); } catch (_) {}
    }
    return r.started_at_utc || '—';
  }

  function render() {
    const liveCount = RUNS.filter(r => r.status === 'IN_PROGRESS').length;
    const stoppedCount = RUNS.filter(r => r.status !== 'COMPLETED' && r.status !== 'IN_PROGRESS').length;
    const statusSummary = liveCount > 0
      ? (liveCount + ' LIVE IN PROGRESS (auto-refreshing every ' + HUB_POLL_SECS + 's)')
      : (stoppedCount > 0 ? ('No active runs (' + stoppedCount + ' stopped)') : 'All runs completed');
    document.getElementById('hub-sub').textContent =
      RUNS.length + ' saved execution run(s)  •  ' + statusSummary;

    const latestBtn = document.getElementById('btn-open-latest');
    latestBtn.style.display = RUNS.length ? 'inline-block' : 'none';

    let totVideos = 0, totMoves = 0, totDups = 0, totUpgrades = 0, totReclaimed = 0;
    RUNS.forEach(r => {
      const t = r.totals || {};
      totVideos += (t.misc_files_evaluated || 0);
      totMoves += (t.direct_moves || 0) + (t.phase2_nondup_moves || 0);
      totDups += (t.duplicate_groups || 0);
      totUpgrades += (t.anchor_upgrades || 0);
      totReclaimed += (t.bytes_reclaimed || 0);
    });

    document.getElementById('hub-kpis').innerHTML = [
      ['Saved Runs', RUNS.length],
      ['Matched Videos Processed', totVideos],
      ['New Videos Consolidated', totMoves],
      ['Duplicate Groups / Upgrades ★', totDups + ' / ' + totUpgrades + ' ★'],
      ['Total Disk Space Reclaimed', fmtBytes(totReclaimed)]
    ].map(([l, v]) =>
      '<div class="kpi"><div class="kpi-lbl">' + esc(l) + '</div><div class="kpi-val">' + esc(v) + '</div></div>'
    ).join('');

    const box = document.getElementById('hub-table');
    if (!RUNS.length) {
      box.innerHTML = '<div class="empty">No execution runs saved yet in <code>reports/</code>. Start a <code>rusty_scanner</code> or <code>rusty_triage</code> live run and this page will update automatically!</div>';
      return;
    }

    box.innerHTML = '<table><thead><tr>' +
      '<th>Status</th><th>Run ID &amp; Started</th><th>Mode</th><th>Batches</th>' +
      '<th>Moved</th><th>Dup Groups</th><th>Upgrades ★</th><th>Space Reclaimed</th><th>Unmatched</th><th>Actions</th>' +
      '</tr></thead><tbody>' +
      RUNS.map(r => {
        const t = r.totals || {};
        const st = r.status || 'COMPLETED';
        const pillCls = st === 'COMPLETED' ? 'p-done' : (st === 'IN_PROGRESS' ? 'p-live' : 'p-stop');
        const pillText = st === 'COMPLETED' ? 'COMPLETED' : (st === 'IN_PROGRESS' ? 'LIVE' : 'STOPPED');
        const movedCnt = (t.direct_moves || 0) + (t.phase2_nondup_moves || 0);
        return '<tr>' +
          '<td><span class="pill ' + pillCls + '">' + esc(pillText) + '</span></td>' +
          '<td><div class="mono" style="font-weight:700;">' + esc(r.run_id) + '</div><div style="font-size:12px; color:var(--text-muted);">' + esc(fmtStart(r)) + ' • ' + Number(r.elapsed_secs || 0).toFixed(1) + 's</div></td>' +
          '<td class="mono">' + esc(r.mode) + '</td>' +
          '<td class="mono">' + (t.anchor_batches || 0) + '</td>' +
          '<td class="mono">+' + movedCnt + ' (' + esc(fmtBytes(t.bytes_moved || 0)) + ')</td>' +
          '<td class="mono">' + (t.duplicate_groups || 0) + '</td>' +
          '<td class="mono" style="color:var(--emerald); font-weight:700;">' + (t.anchor_upgrades || 0) + ' ★</td>' +
          '<td class="mono" style="color:var(--amber); font-weight:700;">' + esc(fmtBytes(t.bytes_reclaimed || 0)) + '</td>' +
          '<td class="mono">' + (t.unmatched_count || 0) + '</td>' +
          '<td style="white-space:nowrap; display:flex; gap:6px;">' +
            '<a class="btn btn-primary" style="padding:5px 10px; font-size:12px;" href="' + esc(r.report_url) + '">Open Dashboard</a>' +
            '<a class="btn" style="padding:5px 9px; font-size:12px;" href="' + esc(r.manifest_url) + '" target="_blank">JSON</a>' +
          '</td>' +
        '</tr>';
      }).join('') + '</tbody></table>';
  }

  document.getElementById('btn-theme').addEventListener('click', () => {
    const h = document.documentElement;
    h.setAttribute('data-theme', h.getAttribute('data-theme') === 'light' ? 'dark' : 'light');
  });

  render();

  if (HUB_POLL_SECS > 0) {
    setInterval(async () => {
      try {
        const res = await fetch('/api/runs?t=' + Date.now(), { cache: 'no-store' });
        if (res.ok) { RUNS = await res.json(); render(); }
      } catch (_) {}
    }, HUB_POLL_SECS * 1000);
  }
})();
</script>
</body>
</html>
"#;
