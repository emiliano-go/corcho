use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::ai::{self, AIRequest};
use crate::core::{merge_settings_value, Core};
use crate::integrations;
use crate::model::{self, Settings};
use crate::theme;
use crate::updates;

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn serve() -> Result<()> {
    let mut core = Core::boot()?;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.len() > 8 * 1024 * 1024 {
            continue;
        }
        let Ok(request) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(response) = handle(&mut core, request) {
            serde_json::to_writer(&mut stdout, &response)?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

/// Invoked after a tool call so an in-process host (the GUI) can repaint from
/// the shared core.
pub type ChangeHook = Arc<dyn Fn() + Send + Sync + 'static>;

#[derive(Clone, Debug)]
pub struct ProgressEvent {
    pub token: Value,
    pub progress: f64,
    pub total: Option<f64>,
    pub message: Option<String>,
}

const SUPPORTED_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];
const OUR_VERSION: &str = "2025-06-18";

pub fn serve_http(addr: &str) -> Result<()> {
    let core = Arc::new(Mutex::new(Core::boot()?));
    serve_http_shared(addr, core, None)
}

/// Serve the Streamable HTTP transport against an existing core. The GUI uses
/// this so MCP clients share the live process instead of a second copy.
pub fn serve_http_shared(
    addr: &str,
    core: Arc<Mutex<Core>>,
    on_change: Option<ChangeHook>,
) -> Result<()> {
    let token = {
        let core = core.lock().map_err(|_| anyhow!("core lock"))?;
        http_token(&core.data_dir)?
    };
    let listener = TcpListener::bind(addr)?;
    eprintln!("corcho mcp http listening on http://{addr}");
    serve_listener(listener, core, token, on_change)
}

fn serve_listener(
    listener: TcpListener,
    core: Arc<Mutex<Core>>,
    token: String,
    on_change: Option<ChangeHook>,
) -> Result<()> {
    for connection in listener.incoming() {
        let Ok(stream) = connection else { continue };
        let core = core.clone();
        let token = token.clone();
        let on_change = on_change.clone();
        std::thread::spawn(move || {
            let _ = handle_http(stream, core, &token, on_change);
        });
    }
    Ok(())
}

pub(crate) fn http_token(data_dir: &Path) -> Result<String> {
    let path = data_dir.join("mcp.token");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim().to_string();
        if !existing.is_empty() {
            return Ok(existing);
        }
    }
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    std::fs::write(&path, &token)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(token)
}

struct HttpRequest {
    method: String,
    path: String,
    headers: std::collections::HashMap<String, String>,
    body: Vec<u8>,
}

fn read_http(stream: &mut TcpStream) -> Result<Option<HttpRequest>> {
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    let mut data = Vec::new();
    let mut buffer = [0u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Ok(None);
        }
        data.extend_from_slice(&buffer[..read]);
        if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if data.len() > 1_000_000 {
            return Ok(None);
        }
    };

    let head = String::from_utf8_lossy(&data[..header_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let mut headers = std::collections::HashMap::new();
    for line in lines {
        let mut pair = line.splitn(2, ':');
        let key = pair.next().unwrap_or("").trim().to_ascii_lowercase();
        let value = pair.next().unwrap_or("").trim().to_string();
        if !key.is_empty() {
            headers.insert(key, value);
        }
    }

    let content_length = headers
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    if content_length > 8 * 1024 * 1024 {
        return Err(anyhow!("request body exceeds 8 MiB"));
    }
    let body_start = header_end + 4;
    while data.len() < body_start + content_length {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        data.extend_from_slice(&buffer[..read]);
    }
    let end = std::cmp::min(data.len(), body_start + content_length);
    let body = data[body_start..end].to_vec();
    Ok(Some(HttpRequest {
        method,
        path,
        headers,
        body,
    }))
}

fn handle_http(
    mut stream: TcpStream,
    core: Arc<Mutex<Core>>,
    token: &str,
    on_change: Option<ChangeHook>,
) -> Result<()> {
    let Some(req) = read_http(&mut stream)? else {
        return Ok(());
    };
    if req.path != "/mcp" && req.path != "/" {
        return respond(&mut stream, "404 Not Found", &json!({"error": "not found"}));
    }
    if let Some(origin) = req.headers.get("origin") {
        if !origin.is_empty()
            && !origin.starts_with("http://localhost")
            && !origin.starts_with("http://127.0.0.1")
        {
            return respond(
                &mut stream,
                "403 Forbidden",
                &json!({"error": "origin not allowed"}),
            );
        }
    }
    if req.headers.get("authorization").map(String::as_str)
        != Some(format!("Bearer {token}").as_str())
    {
        return respond(
            &mut stream,
            "401 Unauthorized",
            &json!({"error": "missing bearer token"}),
        );
    }
    if req.method != "POST" {
        return respond(
            &mut stream,
            "405 Method Not Allowed",
            &json!({"error": "POST only; no server-initiated stream"}),
        );
    }
    if let Some(version) = req.headers.get("mcp-protocol-version") {
        if !SUPPORTED_VERSIONS.contains(&version.as_str()) {
            return respond(
                &mut stream,
                "400 Bad Request",
                &json!({"error": "unsupported MCP-Protocol-Version"}),
            );
        }
    }

    let request: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
    if !(request.get("id").is_some() && request.get("method").is_some()) {
        // Notifications and JSON-RPC responses are accepted with an empty 202.
        return respond_empty(&mut stream, "202 Accepted");
    }

    let wants_sse = req
        .headers
        .get("accept")
        .map(|v| v.contains("text/event-stream"))
        .unwrap_or(false);
    if wants_sse {
        return stream_sse(stream, core, request, on_change);
    }

    let is_tool_call = request.get("method").and_then(|m| m.as_str()) == Some("tools/call");
    let response = {
        let mut core = core.lock().map_err(|_| anyhow!("core lock"))?;
        handle(&mut core, request)
    };
    if is_tool_call {
        if let Some(hook) = &on_change {
            hook();
        }
    }
    match response {
        Some(response) => respond(&mut stream, "200 OK", &response),
        None => respond_empty(&mut stream, "202 Accepted"),
    }
}

enum SseMessage {
    Progress(ProgressEvent),
    Response(Option<Value>),
}

/// Stream a request response as Server-Sent Events, forwarding progress
/// notifications and keeping the connection alive while a long tool runs.
fn stream_sse(
    mut stream: TcpStream,
    core: Arc<Mutex<Core>>,
    request: Value,
    on_change: Option<ChangeHook>,
) -> Result<()> {
    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\nX-Accel-Buffering: no\r\nAccess-Control-Allow-Origin: *\r\n\r\n",
    )?;
    stream.flush()?;

    let (tx, rx) = std::sync::mpsc::channel::<SseMessage>();
    let worker_tx = tx.clone();
    let is_tool_call = request.get("method").and_then(|m| m.as_str()) == Some("tools/call");
    let worker = std::thread::spawn(move || {
        let response = match core.lock() {
            Ok(mut core) => {
                let sink = |event: ProgressEvent| {
                    let _ = tx.send(SseMessage::Progress(event));
                };
                handle_with_progress(&mut core, request, Some(&sink))
            }
            Err(_) => None,
        };
        let _ = worker_tx.send(SseMessage::Response(response));
        if is_tool_call {
            if let Some(hook) = on_change {
                hook();
            }
        }
    });

    loop {
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(SseMessage::Progress(event)) => {
                let mut params = json!({
                    "progressToken": event.token,
                    "progress": event.progress,
                });
                if let Some(total) = event.total {
                    params["total"] = json!(total);
                }
                if let Some(message) = event.message {
                    params["message"] = json!(message);
                }
                write_sse(
                    &mut stream,
                    &json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/progress",
                        "params": params,
                    }),
                )?;
            }
            Ok(SseMessage::Response(Some(response))) => {
                write_sse(&mut stream, &response)?;
                break;
            }
            Ok(SseMessage::Response(None)) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let _ = stream.write_all(b": keep-alive\n\n");
                let _ = stream.flush();
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = worker.join();
    Ok(())
}

fn write_sse(stream: &mut TcpStream, message: &Value) -> Result<()> {
    let data = serde_json::to_string(message)?;
    stream.write_all(b"event: message\ndata: ")?;
    stream.write_all(data.as_bytes())?;
    stream.write_all(b"\n\n")?;
    stream.flush()?;
    Ok(())
}

fn respond(stream: &mut TcpStream, status: &str, body: &Value) -> Result<()> {
    let body = body.to_string();
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()?;
    Ok(())
}

fn respond_empty(stream: &mut TcpStream, status: &str) -> Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: 0\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(head.as_bytes())?;
    stream.flush()?;
    Ok(())
}

pub fn handle(core: &mut Core, request: Value) -> Option<Value> {
    handle_with_progress(core, request, None)
}

fn negotiate_protocol(requested: &str) -> String {
    if SUPPORTED_VERSIONS.contains(&requested) {
        requested.to_string()
    } else {
        OUR_VERSION.to_string()
    }
}

pub fn handle_with_progress(
    core: &mut Core,
    request: Value,
    sink: Option<&dyn Fn(ProgressEvent)>,
) -> Option<Value> {
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");
    match method {
        "initialize" => Some(ok(
            id,
            json!({
                "protocolVersion": negotiate_protocol(
                    request.get("params")
                        .and_then(|p| p.get("protocolVersion"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(OUR_VERSION)
                ),
                "capabilities": {
                    "tools": {},
                    "resources": {},
                    "prompts": {}
                },
                "serverInfo": { "name": "corcho", "version": VERSION }
            }),
        )),
        "ping" => Some(ok(id, json!({}))),
        "tools/list" => Some(ok(id, json!({ "tools": tools() }))),
        "tools/call" => {
            let name = request
                .get("params")
                .and_then(|p| p.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let args = request
                .get("params")
                .and_then(|p| p.get("arguments"))
                .cloned()
                .unwrap_or_else(|| json!({}));
            let token = request
                .get("params")
                .and_then(|p| p.get("_meta"))
                .and_then(|m| m.get("progressToken"))
                .cloned();
            let emit = |progress: f64, total: Option<f64>, message: &str| {
                if let (Some(sink), Some(token)) = (sink, token.as_ref()) {
                    sink(ProgressEvent {
                        token: token.clone(),
                        progress,
                        total,
                        message: Some(message.to_string()),
                    });
                }
            };
            let progress: ProgressFn = if sink.is_some() && token.is_some() {
                Some(&emit)
            } else {
                None
            };
            if let Some(report) = progress {
                report(0.0, None, &format!("running {name}"));
            }
            let result = call_tool(core, name, &args, progress);
            if let Some(report) = progress {
                report(1.0, None, "done");
            }
            match result {
                Ok(text) => {
                    if name == "web.screenshot" {
                        if let Ok(value) = serde_json::from_str::<Value>(&text) {
                            if let Some(data) = value.get("base64").and_then(|v| v.as_str()) {
                                return Some(ok(
                                    id,
                                    json!({
                                        "content": [
                                            {
                                                "type": "text",
                                                "text": format!(
                                                    "{} {}x{}",
                                                    value.get("url").and_then(|v| v.as_str()).unwrap_or(""),
                                                    value.get("width").and_then(|v| v.as_u64()).unwrap_or(0),
                                                    value.get("height").and_then(|v| v.as_u64()).unwrap_or(0)
                                                )
                                            },
                                            { "type": "image", "data": data, "mimeType": "image/png" }
                                        ]
                                    }),
                                ));
                            }
                        }
                    }
                    Some(ok(
                        id,
                        json!({ "content": [{ "type": "text", "text": text }] }),
                    ))
                }
                Err(err) => Some(ok(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": err.to_string() }],
                        "isError": true
                    }),
                )),
            }
        }
        "resources/list" => Some(ok(id, json!({ "resources": resources() }))),
        "resources/read" => {
            let uri = request
                .get("params")
                .and_then(|p| p.get("uri"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            match read_resource(core, uri) {
                Ok((mime, text)) => Some(ok(
                    id,
                    json!({
                        "contents": [{ "uri": uri, "mimeType": mime, "text": text }]
                    }),
                )),
                Err(err) => Some(err_result(id, -32602, &err.to_string())),
            }
        }
        "prompts/list" => Some(ok(id, json!({ "prompts": prompts() }))),
        "prompts/get" => {
            let name = request
                .get("params")
                .and_then(|p| p.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            Some(ok(id, get_prompt(name)))
        }
        _ => id.map(|id| err_result(Some(id), -32601, "method not found")),
    }
}

fn ok(id: Option<Value>, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn err_result(id: Option<Value>, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn text_arg(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn required(args: &Value, key: &str) -> Result<String> {
    let value = text_arg(args, key);
    if value.is_empty() {
        Err(anyhow!("missing argument: {key}"))
    } else {
        Ok(value)
    }
}

fn num_arg(args: &Value, key: &str, fallback: f64) -> f64 {
    args.get(key).and_then(|v| v.as_f64()).unwrap_or(fallback)
}

fn bool_arg(args: &Value, key: &str, fallback: bool) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(fallback)
}

fn settings_public(settings: &Settings) -> Value {
    let mut value = serde_json::to_value(settings).unwrap_or_else(|_| json!({}));
    if let Some(obj) = value.as_object_mut() {
        obj.insert("aiApiKeys".into(), json!({}));
        obj.insert("gdrive_refresh_token".into(), json!(null));
        obj.insert("gdrive_client_secret".into(), json!(null));
    }
    value
}

fn workspace_public(ws: &model::Workspace) -> Value {
    let mut value = serde_json::to_value(ws).unwrap_or_else(|_| json!({}));
    if let Some(settings) = value.get_mut("settings") {
        *settings = settings_public(&ws.settings);
    }
    value
}

fn grid_arg(core: &Core, args: &Value) -> Option<String> {
    let id = text_arg(args, "gridId");
    if !id.is_empty() {
        Some(id)
    } else {
        core.ws.active_grid_id.clone()
    }
}

fn card_by_id(core: &Core, id: &str) -> Result<model::Card> {
    core.ws
        .card(id)
        .map(|(_, c)| c.clone())
        .ok_or_else(|| anyhow!("card not found"))
}

fn frame_from_args(core: &Core, args: &Value) -> Result<(String, f64, f64, f64, f64)> {
    if let Some(frame) = args.get("frame").and_then(|v| v.as_object()) {
        let x = frame.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let y = frame.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let width = frame
            .get("width")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| anyhow!("frame.width is required"))?;
        let height = frame
            .get("height")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| anyhow!("frame.height is required"))?;
        validate_coordinate(x, "frame.x")?;
        validate_coordinate(y, "frame.y")?;
        validate_size(width, 1.0, 1_000_000.0, "frame.width")?;
        validate_size(height, 1.0, 1_000_000.0, "frame.height")?;
        return Ok((
            grid_arg(core, args).unwrap_or_default(),
            x,
            y,
            width,
            height,
        ));
    }
    let width = args.get("viewportWidth").and_then(|v| v.as_f64());
    let height = args.get("viewportHeight").and_then(|v| v.as_f64());
    let grid = grid_arg(core, args);
    let frame = core
        .world_frame(grid.as_deref(), width, height)
        .ok_or_else(|| {
            anyhow!("viewport size unknown: pass frame or viewportWidth/viewportHeight")
        })?;
    Ok(frame)
}

fn visibility_report(core: &Core, args: &Value) -> Result<Value> {
    let (grid_id, fx, fy, fw, fh) = frame_from_args(core, args)?;
    let grid = core
        .ws
        .grid(&grid_id)
        .ok_or_else(|| anyhow!("grid not found"))?;
    let mut cards = Vec::new();
    for card in &grid.cards {
        let left = card.x.max(fx);
        let top = card.y.max(fy);
        let right = (card.x + card.width).min(fx + fw);
        let bottom = (card.y + card.height).min(fy + fh);
        let overlap_w = (right - left).max(0.0);
        let overlap_h = (bottom - top).max(0.0);
        let area = (card.width * card.height).max(1.0);
        let visible = (overlap_w * overlap_h / area).clamp(0.0, 1.0);
        cards.push(json!({
            "id": card.id,
            "kind": card.kind,
            "title": card.title,
            "rect": {"x": card.x, "y": card.y, "width": card.width, "height": card.height},
            "inside": card.x >= fx && card.y >= fy
                && card.x + card.width <= fx + fw
                && card.y + card.height <= fy + fh,
            "intersects": overlap_w > 0.0 && overlap_h > 0.0,
            "visibleFraction": visible
        }));
    }
    Ok(json!({
        "gridId": grid_id,
        "frame": {"x": fx, "y": fy, "width": fw, "height": fh},
        "viewport": {"width": core.ws.viewport_width, "height": core.ws.viewport_height},
        "panX": grid.pan_x,
        "panY": grid.pan_y,
        "zoom": grid.zoom,
        "cards": cards
    }))
}

fn chromium() -> Result<String> {
    for candidate in [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
    ] {
        if std::process::Command::new(candidate)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return Ok(candidate.to_string());
        }
    }
    Err(anyhow!(
        "no Chromium binary found for web capture; install chromium or pass a URL to web.fetch"
    ))
}

fn chrome_run(args: &[String]) -> Result<std::process::Output> {
    let bin = chromium()?;
    let output = std::process::Command::new("timeout")
        .arg("30")
        .arg(&bin)
        .args(args)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "chromium failed ({}): {}",
            output.status,
            stderr.lines().last().unwrap_or("")
        ));
    }
    Ok(output)
}

fn web_url(core: &Core, args: &Value) -> Result<String> {
    let direct = text_arg(args, "url");
    if !direct.is_empty() {
        return validate_url(&direct);
    }
    let id = required(args, "cardId")?;
    let card = card_by_id(core, &id)?;
    let url = card.str_field("url", "");
    if url.is_empty() {
        return Err(anyhow!("card has no url"));
    }
    validate_url(&url)
}

fn normalize_url(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if value.contains("://")
        || ["file:", "data:", "javascript:", "vbscript:", "about:"]
            .iter()
            .any(|scheme| lower.starts_with(scheme))
    {
        value.to_string()
    } else {
        format!("https://{value}")
    }
}

fn validate_kind(kind: &str) -> Result<()> {
    if model::CARD_KINDS.contains(&kind) {
        Ok(())
    } else {
        Err(anyhow!(
            "unknown card kind: {kind}; expected one of {}",
            model::CARD_KINDS.join(", ")
        ))
    }
}

fn validate_color(color: &str) -> Result<()> {
    let Some(hex) = color.strip_prefix('#') else {
        return Err(anyhow!("colour must start with #"));
    };
    if !matches!(hex.len(), 3 | 6 | 8) || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(anyhow!("colour must be #rgb, #rrggbb or #rrggbbaa"));
    }
    Ok(())
}

fn validate_icon(icon: &str) -> Result<()> {
    if icon.is_empty()
        || icon.len() > 64
        || !icon
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(anyhow!(
            "icon must be a lucide name using letters, digits and hyphens"
        ));
    }
    Ok(())
}

fn validate_url(url: &str) -> Result<String> {
    let normalized = normalize_url(url);
    let parsed = url::Url::parse(&normalized).map_err(|e| anyhow!("invalid URL: {e}"))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(anyhow!("only http and https URLs are allowed"));
    }
    if parsed.host_str().is_none() {
        return Err(anyhow!("URL has no host"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(anyhow!("URLs with embedded credentials are not allowed"));
    }
    Ok(normalized)
}

fn validate_coordinate(value: f64, label: &str) -> Result<f64> {
    if !value.is_finite() || value.abs() > 1_000_000.0 {
        return Err(anyhow!("{label} must be a finite coordinate within +/-1e6"));
    }
    Ok(value)
}

fn validate_size(value: f64, min: f64, max: f64, label: &str) -> Result<f64> {
    if !value.is_finite() || value < min || value > max {
        return Err(anyhow!("{label} must be between {min} and {max}"));
    }
    Ok(value)
}

fn validate_length(value: &str, max: usize, label: &str) -> Result<()> {
    if value.len() > max {
        return Err(anyhow!("{label} exceeds {max} bytes"));
    }
    Ok(())
}

fn validate_absolute_path(path: &str) -> Result<std::path::PathBuf> {
    let path = std::path::PathBuf::from(path);
    if !path.is_absolute() {
        return Err(anyhow!("path must be absolute"));
    }
    Ok(path)
}

type ProgressFn<'a> = Option<&'a dyn Fn(f64, Option<f64>, &str)>;

fn report(progress: ProgressFn, value: f64, message: &str) {
    if let Some(cb) = progress {
        cb(value, None, message);
    }
}

fn call_tool(core: &mut Core, name: &str, args: &Value, progress: ProgressFn) -> Result<String> {
    let _ = core.reload_if_changed();
    if let Some(expected) = args.get("expectedRevision").and_then(|v| v.as_u64()) {
        if expected != core.ws.revision {
            return Err(anyhow!(
                "REVISION_CONFLICT: current revision is {}",
                core.ws.revision
            ));
        }
    }
    match name {
        "workspace.get" => Ok(serde_json::to_string_pretty(&workspace_public(&core.ws))?),
        "workspace.export" => {
            let path = validate_absolute_path(&required(args, "path")?)?;
            report(progress, 0.2, "writing backup");
            core.export_backup(&path.to_string_lossy())?;
            Ok(format!("exported to {}", path.display()))
        }
        "workspace.import" => {
            let path = validate_absolute_path(&required(args, "path")?)?;
            report(progress, 0.2, "reading backup");
            if text_arg(args, "format") == "legacy" {
                core.import_legacy(&path.to_string_lossy())?;
            } else {
                core.import_backup(&path.to_string_lossy())?;
            }
            Ok("imported".into())
        }
        "grid.list" => {
            let folder = text_arg(args, "folderId");
            let grids: Vec<Value> = core
                .ws
                .grids
                .iter()
                .filter(|g| folder.is_empty() || g.folder_id.as_deref() == Some(folder.as_str()))
                .map(|g| {
                    json!({
                        "id": g.id,
                        "name": g.name,
                        "folderId": g.folder_id,
                        "cardCount": g.cards.len(),
                        "panX": g.pan_x,
                        "panY": g.pan_y,
                        "zoom": g.zoom
                    })
                })
                .collect();
            Ok(Value::Array(grids).to_string())
        }
        "grid.get" => {
            let id = required(args, "gridId")?;
            Ok(serde_json::to_string_pretty(
                core.ws.grid(&id).ok_or_else(|| anyhow!("grid not found"))?,
            )?)
        }
        "grid.create" => {
            let name = text_arg(args, "name");
            let grid = core.create_grid(if name.is_empty() { "New Grid" } else { &name });
            if let Some(folder) = args.get("folderId").and_then(|v| v.as_str()) {
                core.move_grid_to_folder(&grid.id, Some(folder));
            }
            Ok(serde_json::to_string_pretty(
                &core.ws.grid(&grid.id).cloned(),
            )?)
        }
        "grid.rename" => {
            let id = required(args, "gridId")?;
            let name = required(args, "name")?;
            core.rename_grid(&id, &name);
            Ok("renamed".into())
        }
        "grid.setIcon" => {
            let id = required(args, "gridId")?;
            let icon = required(args, "icon")?;
            let lucide = icon
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
            if !(lucide || icon.to_ascii_lowercase().ends_with(".svg")) {
                return Err(anyhow!("icon must be a lucide name or an .svg path"));
            }
            if core.set_grid_icon(&id, &icon) {
                Ok("ok".into())
            } else {
                Err(anyhow!("grid not found"))
            }
        }
        "grid.delete" => {
            let id = required(args, "gridId")?;
            if core.delete_grid(&id) {
                Ok("deleted".into())
            } else {
                Err(anyhow!("cannot delete the last grid"))
            }
        }
        "grid.switch" => {
            let id = required(args, "gridId")?;
            if core.switch_grid(&id) {
                Ok("switched".into())
            } else {
                Err(anyhow!("grid not found"))
            }
        }
        "grid.move" => {
            let grid = required(args, "gridId")?;
            let folder = text_arg(args, "folderId");
            core.move_grid_to_folder(
                &grid,
                if folder.is_empty() {
                    None
                } else {
                    Some(&folder)
                },
            );
            Ok("moved".into())
        }
        "grid.reorder" => {
            let grid = required(args, "gridId")?;
            let before = text_arg(args, "beforeId");
            if core.reorder_grid(
                &grid,
                if before.is_empty() {
                    None
                } else {
                    Some(&before)
                },
            ) {
                Ok("reordered".into())
            } else {
                Err(anyhow!("grid not found"))
            }
        }
        "workspace.revision" => Ok(json!({"revision": core.ws.revision}).to_string()),
        "workspace.reset" => {
            if text_arg(args, "confirm") != "yes" {
                return Err(anyhow!("pass confirm=\"yes\" to wipe all data"));
            }
            core.ws = model::Workspace::default();
            core.history.reset(&core.ws);
            core.save()?;
            Ok("reset".into())
        }
        "folder.list" => Ok(serde_json::to_string_pretty(&core.ws.folders)?),
        "folder.create" => {
            let name = required(args, "name")?;
            Ok(serde_json::to_string_pretty(&core.create_folder(&name))?)
        }
        "folder.rename" => {
            let id = required(args, "folderId")?;
            let name = required(args, "name")?;
            if core.rename_folder(&id, &name) {
                Ok("renamed".into())
            } else {
                Err(anyhow!("folder not found"))
            }
        }
        "folder.delete" => {
            let id = required(args, "folderId")?;
            let keep = bool_arg(args, "keepContent", true);
            if core.delete_folder(&id, keep) {
                Ok("deleted".into())
            } else {
                Err(anyhow!("folder not found"))
            }
        }
        "folder.toggle" => {
            let id = required(args, "folderId")?;
            if core.ws.folders.iter().any(|f| f.id == id) {
                core.toggle_folder(&id);
                Ok("toggled".into())
            } else {
                Err(anyhow!("folder not found"))
            }
        }
        "view.get" => {
            let grid_id = grid_arg(core, args).ok_or_else(|| anyhow!("no active grid"))?;
            let grid = core
                .ws
                .grid(&grid_id)
                .ok_or_else(|| anyhow!("grid not found"))?;
            let frame = core.world_frame(Some(&grid_id), None, None);
            Ok(json!({
                "gridId": grid_id,
                "panX": grid.pan_x,
                "panY": grid.pan_y,
                "zoom": grid.zoom,
                "viewport": {"width": core.ws.viewport_width, "height": core.ws.viewport_height},
                "frame": frame.map(|(_, x, y, w, h)| json!({"x": x, "y": y, "width": w, "height": h}))
            })
            .to_string())
        }
        "view.set" | "grid.view" => {
            if let Some(grid_id) = args.get("gridId").and_then(|v| v.as_str()) {
                if let Some(grid) = core.ws.grid_mut(grid_id) {
                    if let Some(v) = args.get("panX").and_then(|v| v.as_f64()) {
                        grid.pan_x = v;
                    }
                    if let Some(v) = args.get("panY").and_then(|v| v.as_f64()) {
                        grid.pan_y = v;
                    }
                    if let Some(v) = args.get("zoom").and_then(|v| v.as_f64()) {
                        grid.zoom = v.clamp(0.2, 3.0);
                    }
                } else {
                    return Err(anyhow!("grid not found"));
                }
            } else {
                let (pan_x, pan_y, zoom) = core.active_grid_view();
                core.set_view(
                    args.get("panX").and_then(|v| v.as_f64()).unwrap_or(pan_x),
                    args.get("panY").and_then(|v| v.as_f64()).unwrap_or(pan_y),
                    args.get("zoom").and_then(|v| v.as_f64()).unwrap_or(zoom),
                );
            }
            if let Some(w) = args.get("viewportWidth").and_then(|v| v.as_f64()) {
                if let Some(h) = args.get("viewportHeight").and_then(|v| v.as_f64()) {
                    core.set_viewport(w, h);
                }
            }
            core.save_quiet();
            Ok("ok".into())
        }
        "frame.get" => {
            let (grid_id, x, y, width, height) = frame_from_args(core, args)?;
            Ok(json!({
                "gridId": grid_id,
                "frame": {"x": x, "y": y, "width": width, "height": height},
                "viewport": {"width": core.ws.viewport_width, "height": core.ws.viewport_height}
            })
            .to_string())
        }
        "card.visibility" => Ok(visibility_report(core, args)?.to_string()),
        "card.list" => {
            let grid_id = grid_arg(core, args);
            let kind = text_arg(args, "kind");
            let query = text_arg(args, "query").to_lowercase();
            let cards: Vec<model::Card> = match grid_id {
                Some(id) => core
                    .ws
                    .grid(&id)
                    .map(|g| g.cards.clone())
                    .ok_or_else(|| anyhow!("grid not found"))?,
                None => Vec::new(),
            };
            let cards: Vec<model::Card> = cards
                .into_iter()
                .filter(|c| kind.is_empty() || c.kind == kind)
                .filter(|c| {
                    query.is_empty()
                        || c.title.to_lowercase().contains(&query)
                        || c.text.to_lowercase().contains(&query)
                        || c.search_text().to_lowercase().contains(&query)
                })
                .collect();
            Ok(serde_json::to_string_pretty(&cards)?)
        }
        "card.get" => {
            let id = required(args, "cardId")?;
            let card = core
                .ws
                .card(&id)
                .map(|(_, c)| c.clone())
                .ok_or_else(|| anyhow!("card not found"))?;
            Ok(serde_json::to_string_pretty(&card)?)
        }
        "card.create" => {
            let kind = text_arg(args, "kind");
            let kind = if kind.is_empty() {
                "note".to_string()
            } else {
                kind
            };
            validate_kind(&kind)?;
            let x = validate_coordinate(num_arg(args, "x", 64.0), "x")?;
            let y = validate_coordinate(num_arg(args, "y", 64.0), "y")?;
            if let Some(value) = args.get("title").and_then(|v| v.as_str()) {
                validate_length(value, 4096, "title")?;
            }
            if let Some(value) = args.get("text").and_then(|v| v.as_str()) {
                validate_length(value, 1_000_000, "text")?;
            }
            if let Some(value) = args.get("color").and_then(|v| v.as_str()) {
                validate_color(value)?;
            }
            if let Some(value) = args.get("icon").and_then(|v| v.as_str()) {
                validate_icon(value)?;
            }
            let mut card = core
                .create_card(&kind, x, y)
                .ok_or_else(|| anyhow!("could not create card"))?;
            let mut patch = serde_json::Map::new();
            patch.insert("id".into(), json!(card.id.clone()));
            for key in ["title", "text", "color", "icon", "width", "height"] {
                if let Some(value) = args.get(key) {
                    if key == "width" {
                        validate_size(value.as_f64().unwrap_or(0.0), 80.0, 10_000.0, "width")?;
                    }
                    if key == "height" {
                        validate_size(value.as_f64().unwrap_or(0.0), 64.0, 10_000.0, "height")?;
                    }
                    patch.insert(key.into(), value.clone());
                }
            }
            if let Some(payload) = args.get("payload") {
                if !payload.is_object() {
                    return Err(anyhow!("payload must be an object"));
                }
                patch.insert("payload".into(), payload.clone());
            }
            if let Some(grid_id) = args.get("gridId").and_then(|v| v.as_str()) {
                if core
                    .ws
                    .active_grid_id
                    .as_deref()
                    .map(|a| a != grid_id)
                    .unwrap_or(true)
                {
                    if !core.send_card_to_grid(&card.id, grid_id) {
                        return Err(anyhow!("grid not found"));
                    }
                    card = card_by_id(core, &card.id)?;
                }
            }
            if patch.len() > 1 {
                core.update_card_json(&Value::Object(patch).to_string());
                card = card_by_id(core, &card.id)?;
            }
            Ok(serde_json::to_string_pretty(&card)?)
        }
        "card.update" => {
            let id = required(args, "cardId")?;
            let patch = args.get("patch").cloned().unwrap_or_else(|| args.clone());
            let mut object = patch;
            if let Some(obj) = object.as_object_mut() {
                if let Some(value) = obj.get("color").and_then(|v| v.as_str()) {
                    validate_color(value)?;
                }
                if let Some(value) = obj.get("icon").and_then(|v| v.as_str()) {
                    validate_icon(value)?;
                }
                if let Some(value) = obj.get("title").and_then(|v| v.as_str()) {
                    validate_length(value, 4096, "title")?;
                }
                if let Some(value) = obj.get("text").and_then(|v| v.as_str()) {
                    validate_length(value, 1_000_000, "text")?;
                }
                if let Some(value) = obj.get("x").and_then(|v| v.as_f64()) {
                    validate_coordinate(value, "x")?;
                }
                if let Some(value) = obj.get("y").and_then(|v| v.as_f64()) {
                    validate_coordinate(value, "y")?;
                }
                if let Some(value) = obj.get("width").and_then(|v| v.as_f64()) {
                    validate_size(value, 80.0, 10_000.0, "width")?;
                }
                if let Some(value) = obj.get("height").and_then(|v| v.as_f64()) {
                    validate_size(value, 64.0, 10_000.0, "height")?;
                }
                if let Some(value) = obj.get("payload") {
                    if !value.is_object() {
                        return Err(anyhow!("payload must be an object"));
                    }
                }
                obj.insert("id".into(), json!(id));
            } else {
                return Err(anyhow!("patch must be an object"));
            }
            if core.update_card_json(&object.to_string()) {
                Ok("updated".into())
            } else {
                Err(anyhow!("card not found"))
            }
        }
        "card.delete" => {
            let id = required(args, "cardId")?;
            if core.delete_card(&id) {
                Ok("deleted".into())
            } else {
                Err(anyhow!("card not found"))
            }
        }
        "card.duplicate" => {
            let id = required(args, "cardId")?;
            let mut card = core
                .duplicate_card(&id)
                .ok_or_else(|| anyhow!("card not found"))?;
            if let Some(offset) = args.get("offset").and_then(|v| v.as_f64()) {
                let x = card.x - 32.0 + offset;
                let y = card.y - 32.0 + offset;
                core.update_card_json(&json!({"id": card.id.clone(), "x": x, "y": y}).to_string());
                card = card_by_id(core, &card.id)?;
            }
            Ok(serde_json::to_string_pretty(&card)?)
        }
        "card.move" => {
            let card_id = required(args, "cardId")?;
            let grid_id = text_arg(args, "gridId");
            if !grid_id.is_empty() && !core.send_card_to_grid(&card_id, &grid_id) {
                return Err(anyhow!("card or grid not found"));
            }
            let mut patch = serde_json::Map::new();
            patch.insert("id".into(), json!(card_id));
            let mut changed = false;
            for key in ["x", "y"] {
                if let Some(value) = args.get(key) {
                    patch.insert(key.into(), value.clone());
                    changed = true;
                }
            }
            if changed {
                core.update_card_json(&Value::Object(patch).to_string());
            }
            Ok(serde_json::to_string_pretty(&card_by_id(core, &card_id)?)?)
        }
        "card.resize" => {
            let id = required(args, "cardId")?;
            let mut patch = serde_json::Map::new();
            patch.insert("id".into(), json!(id));
            if let Some(value) = args.get("width") {
                validate_size(value.as_f64().unwrap_or(0.0), 80.0, 10_000.0, "width")?;
                patch.insert("width".into(), value.clone());
            }
            if let Some(value) = args.get("height") {
                validate_size(value.as_f64().unwrap_or(0.0), 64.0, 10_000.0, "height")?;
                patch.insert("height".into(), value.clone());
            }
            if !core.update_card_json(&Value::Object(patch).to_string()) {
                return Err(anyhow!("card not found"));
            }
            Ok(serde_json::to_string_pretty(&card_by_id(core, &id)?)?)
        }
        "card.setColor" => {
            let id = required(args, "cardId")?;
            let color = required(args, "color")?;
            validate_color(&color)?;
            core.update_card_json(&json!({"id": id, "color": color}).to_string());
            Ok("ok".into())
        }
        "card.setIcon" => {
            let id = required(args, "cardId")?;
            let icon = required(args, "icon")?;
            validate_icon(&icon)?;
            core.update_card_json(&json!({"id": id, "icon": icon}).to_string());
            Ok("ok".into())
        }
        "card.lock" => {
            let id = required(args, "cardId")?;
            let locked = bool_arg(args, "locked", true);
            core.update_card_json(&json!({"id": id, "locked": locked}).to_string());
            Ok("ok".into())
        }
        "file.pin" => {
            let path = required(args, "path")?;
            validate_length(&path, 4096, "path")?;
            let x = validate_coordinate(num_arg(args, "x", 64.0), "x")?;
            let y = validate_coordinate(num_arg(args, "y", 64.0), "y")?;
            let grid = text_arg(args, "gridId");
            let card = core.pin_path(
                &path,
                x,
                y,
                if grid.is_empty() { None } else { Some(&grid) },
            )?;
            Ok(serde_json::to_string_pretty(&card)?)
        }
        "card.search" => {
            let query = text_arg(args, "query");
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
            let value: Value = serde_json::from_str(&core.search_json(&query))?;
            let mut results = value.as_array().cloned().unwrap_or_default();
            results.truncate(limit);
            Ok(Value::Array(results).to_string())
        }
        "settings.get" => Ok(serde_json::to_string_pretty(&settings_public(
            &core.ws.settings,
        ))?),
        "settings.set" => {
            let key = required(args, "key")?;
            let value = args.get("value").cloned().unwrap_or(Value::Null);
            if core.set_setting_json(&key, &value.to_string()) {
                Ok("ok".into())
            } else {
                Err(anyhow!("invalid setting"))
            }
        }
        "settings.batch" => {
            let patch = args.get("patch").cloned().unwrap_or_else(|| json!({}));
            let mut value = patch;
            merge_settings_value(&mut core.ws.settings, &mut value);
            core.save()?;
            Ok("ok".into())
        }
        "theme.list" => Ok(core.themes_json()),
        "theme.get" => {
            let mode = core.ws.settings.theme_mode.clone();
            let light = core.ws.settings.theme_light.clone();
            let dark = core.ws.settings.theme_dark.clone();
            let effective = match mode.as_str() {
                "light" => Some(light.clone()),
                "dark" => Some(dark.clone()),
                _ => None,
            };
            Ok(json!({
                "mode": mode,
                "light": light,
                "dark": dark,
                "effective": effective
            })
            .to_string())
        }
        "theme.set" => {
            let id = required(args, "themeId")?;
            if !theme::theme_exists(&core.data_dir, &id) {
                return Err(anyhow!("unknown theme"));
            }
            let mode = text_arg(args, "mode");
            if !mode.is_empty() && !matches!(mode.as_str(), "light" | "dark") {
                return Err(anyhow!("mode must be light or dark"));
            }
            match mode.as_str() {
                "light" => {
                    core.set_setting_json("theme_light", &json!(id).to_string());
                }
                "dark" => {
                    core.set_setting_json("theme_dark", &json!(id).to_string());
                }
                _ => {
                    core.set_setting_json("theme", &json!(id).to_string());
                    core.set_setting_json("theme_light", &json!(id).to_string());
                    core.set_setting_json("theme_dark", &json!(id).to_string());
                }
            };
            Ok("ok".into())
        }
        "theme.mode.set" => {
            let mode = required(args, "mode")?;
            if !matches!(mode.as_str(), "system" | "light" | "dark") {
                return Err(anyhow!("mode must be system, light or dark"));
            }
            core.set_setting_json("theme_mode", &json!(mode).to_string());
            Ok("ok".into())
        }
        "theme.custom.set" => {
            let theme = json!({
                "background": required(args, "background")?,
                "surface": required(args, "surface")?,
                "accent": required(args, "accent")?,
                "text": required(args, "text")?
            });
            core.set_setting_json("customTheme", &theme.to_string());
            core.set_setting_json("theme", &json!("custom").to_string());
            core.set_setting_json("theme_light", &json!("custom").to_string());
            core.set_setting_json("theme_dark", &json!("custom").to_string());
            Ok("ok".into())
        }
        "theme.css.template" => Ok(theme::TEMPLATE.to_string()),
        "theme.css.get" => {
            let id = required(args, "themeId")?;
            if !theme::valid_id(&id) {
                return Err(anyhow!("invalid theme id"));
            }
            let path = theme::themes_dir(&core.data_dir).join(format!("{id}.css"));
            std::fs::read_to_string(&path).map_err(|err| anyhow!("read {}: {err}", path.display()))
        }
        "theme.css.set" => {
            let id = required(args, "themeId")?;
            let css = required(args, "css")?;
            let mut entry = theme::parse_css_theme(&css, &id)?;
            if entry.id != id {
                return Err(anyhow!(
                    "theme id `{}` does not match requested id `{id}`",
                    entry.id
                ));
            }
            entry.source = "css".into();
            let path = theme::write_css_theme(&core.data_dir, &entry)?;
            Ok(path.display().to_string())
        }
        "theme.css.delete" => {
            let id = required(args, "themeId")?;
            if !theme::valid_id(&id) {
                return Err(anyhow!("invalid theme id"));
            }
            if theme::builtin_entries().iter().any(|t| t.id == id) {
                return Err(anyhow!("cannot delete a built-in theme"));
            }
            let path = theme::themes_dir(&core.data_dir).join(format!("{id}.css"));
            std::fs::remove_file(&path)
                .map_err(|err| anyhow!("delete {}: {err}", path.display()))?;
            Ok("ok".into())
        }
        "font.list" => Ok(model::fonts().to_string()),
        "font.set" => {
            let id = required(args, "fontId")?;
            if !model::fonts()
                .as_array()
                .map(|f| f.iter().any(|v| v["id"] == id))
                .unwrap_or(false)
            {
                return Err(anyhow!("unknown font"));
            }
            core.set_setting_json("font", &json!(id).to_string());
            Ok("ok".into())
        }
        "effects.set" => {
            if let Some(v) = args.get("spawn") {
                core.set_setting_json("spawn_animations", &v.to_string());
            }
            if let Some(v) = args.get("parallax") {
                core.set_setting_json("card_parallax", &v.to_string());
            }
            if let Some(v) = args.get("ripple") {
                core.set_setting_json("grid_ripple", &v.to_string());
            }
            if let Some(v) = args.get("reducedMotion") {
                core.set_setting_json("reduced_motion", &v.to_string());
            }
            Ok("ok".into())
        }
        "audio.setVolume" => {
            let volume = args
                .get("volume")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow!("volume is required"))?
                .clamp(0, 100);
            core.set_setting_json("master_volume", &json!(volume).to_string());
            Ok("ok".into())
        }
        "launcher.search" => {
            let query = text_arg(args, "query");
            Ok(core.search_json(&query))
        }
        "launcher.run" => {
            let id = required(args, "commandId")?;
            Ok(core.command(&id).to_string())
        }
        "ai.providers" => Ok(model::models_by_provider().to_string()),
        "ai.complete" => {
            let provider = if text_arg(args, "provider").is_empty() {
                core.ws.settings.ai_provider.clone()
            } else {
                text_arg(args, "provider")
            };
            let key = core
                .ws
                .settings
                .ai_api_keys
                .get(&provider)
                .cloned()
                .unwrap_or_default();
            let prompt = required(args, "prompt")?;
            let req = AIRequest {
                provider: provider.clone(),
                model: {
                    let model = text_arg(args, "model");
                    if model.is_empty() {
                        core.ws
                            .settings
                            .ai_model
                            .get(&provider)
                            .cloned()
                            .unwrap_or_else(|| ai::default_model(&provider).to_string())
                    } else {
                        model
                    }
                },
                api_key: key,
                prompt,
                system_prompt: ai::system_prompt(),
                web_search: core.ws.settings.ai_web_search,
                server_url: core.ws.settings.ai_local_server_url.clone(),
            };
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            report(progress, 0.4, &format!("requesting {provider}"));
            Ok(runtime.block_on(ai::complete(req, |_| {}))?)
        }
        "ai.validateKey" => {
            let provider = required(args, "provider")?;
            let key = required(args, "apiKey")?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            report(progress, 0.4, "validating key");
            runtime.block_on(ai::validate_key(
                &provider,
                &key,
                &core.ws.settings.ai_local_server_url,
            ))?;
            Ok("valid".into())
        }
        "ai.setKey" => {
            let provider = required(args, "provider")?;
            let key = required(args, "apiKey")?;
            let mut keys = core.ws.settings.ai_api_keys.clone();
            keys.insert(provider, key);
            core.set_setting_json("aiApiKeys", &json!(keys).to_string());
            Ok("ok".into())
        }
        "ai.clearKey" => {
            let provider = required(args, "provider")?;
            let mut keys = core.ws.settings.ai_api_keys.clone();
            keys.remove(&provider);
            core.set_setting_json("aiApiKeys", &json!(keys).to_string());
            Ok("ok".into())
        }
        "ai.setModel" => {
            let provider = required(args, "provider")?;
            let model = required(args, "model")?;
            let mut models = core.ws.settings.ai_model.clone();
            models.insert(provider, model);
            core.set_setting_json("aiModel", &json!(models).to_string());
            Ok("ok".into())
        }
        "ai.models.local" => {
            let server_url = text_arg(args, "serverUrl");
            let key = text_arg(args, "apiKey");
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            report(progress, 0.4, "fetching models");
            let models = runtime.block_on(ai::fetch_models(&server_url, &key))?;
            Ok(json!(models).to_string())
        }
        "ai.card.run" => {
            let id = required(args, "cardId")?;
            let card = card_by_id(core, &id)?;
            let provider = card.str_field("provider", &core.ws.settings.ai_provider);
            let model = card.str_field("model", "");
            let prompt = card.str_field("prompt", "");
            if prompt.is_empty() {
                return Err(anyhow!("card has no prompt"));
            }
            let key = core
                .ws
                .settings
                .ai_api_keys
                .get(&provider)
                .cloned()
                .unwrap_or_default();
            let model = if model.is_empty() {
                ai::default_model(&provider).to_string()
            } else {
                model
            };
            let req = AIRequest {
                provider,
                model,
                api_key: key,
                prompt,
                system_prompt: ai::system_prompt(),
                web_search: core.ws.settings.ai_web_search,
                server_url: core.ws.settings.ai_local_server_url.clone(),
            };
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            report(progress, 0.4, "running AI node");
            let text = runtime.block_on(ai::complete(req, |_| {}))?;
            core.update_card_json(&json!({"id": id, "payload": {"lastResult": text}}).to_string());
            Ok("ok".into())
        }
        "ai.stats" => {
            let mut cards = 0;
            let mut prompts = 0;
            for grid in &core.ws.grids {
                for card in &grid.cards {
                    if card.kind == "ai" {
                        cards += 1;
                        prompts += card.str_field("prompt", "").len() as u64;
                    }
                }
            }
            Ok(json!({"aiCards": cards, "promptBytes": prompts}).to_string())
        }
        "web.state" => {
            let id = required(args, "cardId")?;
            let card = card_by_id(core, &id)?;
            Ok(json!({
                "cardId": id,
                "kind": card.kind,
                "url": card.str_field("url", ""),
                "title": card.title,
                "rect": {"x": card.x, "y": card.y, "width": card.width, "height": card.height}
            })
            .to_string())
        }
        "web.navigate" => {
            let id = required(args, "cardId")?;
            let url = validate_url(&required(args, "url")?)?;
            core.update_card_json(&json!({"id": id, "payload": {"url": url}}).to_string());
            Ok(url)
        }
        "web.fetch" => {
            let url = web_url(core, args)?;
            report(progress, 0.2, "requesting page");
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let response = runtime.block_on(
                reqwest::Client::builder()
                    .user_agent("Corcho/0.1")
                    .build()?
                    .get(&url)
                    .send(),
            )?;
            report(progress, 0.7, "reading body");
            let status = response.status().as_u16();
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("text/plain")
                .to_string();
            let body = runtime.block_on(response.text())?;
            let truncated = body.len() > 2_000_000;
            Ok(json!({
                "url": url,
                "status": status,
                "contentType": content_type,
                "truncated": truncated,
                "body": body.get(..2_000_000).unwrap_or(&body)
            })
            .to_string())
        }
        "web.dom" => {
            let url = web_url(core, args)?;
            report(progress, 0.3, "rendering page");
            let output = chrome_run(&[
                "--headless=new".into(),
                "--disable-gpu".into(),
                "--no-sandbox".into(),
                "--disable-dev-shm-usage".into(),
                format!(
                    "--virtual-time-budget={}",
                    num_arg(args, "waitMs", 4000.0) as u64
                ),
                "--dump-dom".into(),
                url.clone(),
            ])?;
            Ok(json!({
                "url": url,
                "html": String::from_utf8_lossy(&output.stdout).to_string()
            })
            .to_string())
        }
        "web.screenshot" => {
            let url = web_url(core, args)?;
            let card = args
                .get("cardId")
                .and_then(|v| v.as_str())
                .and_then(|id| core.ws.card(id).map(|(_, c)| c.clone()));
            let width = args
                .get("width")
                .and_then(|v| v.as_f64())
                .or_else(|| card.as_ref().map(|c| c.width))
                .unwrap_or(1280.0)
                .clamp(200.0, 4096.0);
            let height = args
                .get("height")
                .and_then(|v| v.as_f64())
                .or_else(|| card.as_ref().map(|c| c.height))
                .unwrap_or(900.0)
                .clamp(200.0, 4096.0);
            let path = std::env::temp_dir()
                .join(format!("corcho-mcp-{}.png", uuid::Uuid::new_v4().simple()));
            report(progress, 0.3, "capturing screenshot");
            let result = chrome_run(&[
                "--headless=new".into(),
                "--disable-gpu".into(),
                "--no-sandbox".into(),
                "--hide-scrollbars".into(),
                "--disable-dev-shm-usage".into(),
                format!("--window-size={width},{height}"),
                format!(
                    "--virtual-time-budget={}",
                    num_arg(args, "waitMs", 4000.0) as u64
                ),
                format!("--screenshot={}", path.display()),
                url.clone(),
            ]);
            let bytes = result.and_then(|_| std::fs::read(&path).map_err(Into::into));
            let _ = std::fs::remove_file(&path);
            let bytes = bytes?;
            use base64::Engine;
            Ok(json!({
                "url": url,
                "mimeType": "image/png",
                "width": width as u32,
                "height": height as u32,
                "base64": base64::engine::general_purpose::STANDARD.encode(bytes)
            })
            .to_string())
        }
        "sync.status" => Ok(json!({
            "enabled": core.ws.settings.cloud_sync_enabled,
            "provider": core.ws.settings.cloud_provider,
            "folder": core.ws.settings.cloud_folder,
            "connected": core.ws.settings.gdrive_connected
        })
        .to_string()),
        "sync.configure" => {
            if let Some(enabled) = args.get("enabled").and_then(|v| v.as_bool()) {
                core.set_setting_json("cloud_sync_enabled", &enabled.to_string());
            }
            if let Some(provider) = args.get("provider").and_then(|v| v.as_str()) {
                core.set_setting_json("cloud_provider", &json!(provider).to_string());
            }
            if let Some(folder) = args.get("folder").and_then(|v| v.as_str()) {
                core.set_setting_json("cloud_folder", &json!(folder).to_string());
            }
            Ok("ok".into())
        }
        "sync.now" => {
            let provider = core.ws.settings.cloud_provider.clone();
            if provider == "folder" {
                let folder = core
                    .ws
                    .settings
                    .cloud_folder
                    .clone()
                    .ok_or_else(|| anyhow!("no sync folder configured"))?;
                report(progress, 0.3, "syncing folder");
                let (status, ws) =
                    crate::sync::sync_folder(&core.data_dir, std::path::Path::new(&folder))?;
                if let Some(mut ws) = ws {
                    ws.migrate();
                    core.ws = ws;
                    let snapshot = core.ws.clone();
                    core.history.reset(&snapshot);
                }
                Ok(status)
            } else {
                Err(anyhow!(
                    "configure the folder provider or use iroh.sync for peer sync"
                ))
            }
        }
        "iroh.status" => Ok(core.iroh.status_json()),
        "iroh.create" => Ok(core.iroh.create_workspace()?),
        "iroh.join" => {
            let ticket = required(args, "ticket")?;
            core.iroh.join(&ticket)?;
            core.reload_from_disk()?;
            Ok(core.iroh.status_json())
        }
        "iroh.share" => {
            let read_only = args
                .get("readOnly")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Ok(core.iroh.share(read_only)?)
        }
        "iroh.cache" => Ok(core.iroh.cache_ticket()?),
        "iroh.sync" => {
            report(progress, 0.3, "syncing peers");
            let status = core.iroh.sync_now()?;
            core.reload_from_disk()?;
            Ok(status)
        }
        "iroh.leave" => {
            core.iroh.leave()?;
            Ok("left".into())
        }
        "iroh.peers" => Ok(core.iroh.status_json()),
        "backup.export" => {
            let path = validate_absolute_path(&required(args, "path")?)?;
            report(progress, 0.3, "writing backup");
            core.export_backup(&path.to_string_lossy())?;
            Ok(format!("exported to {}", path.display()))
        }
        "backup.import" => {
            let path = validate_absolute_path(&required(args, "path")?)?;
            report(progress, 0.3, "reading backup");
            core.import_backup(&path.to_string_lossy())?;
            Ok("imported".into())
        }
        "update.check" => {
            let channel = core.ws.settings.update_channel.clone();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            report(progress, 0.3, "checking for updates");
            let state = runtime.block_on(updates::check(&channel))?;
            core.update = state.clone();
            Ok(serde_json::to_string_pretty(&state)?)
        }
        "update.channel.set" => {
            let channel = required(args, "channel")?;
            core.set_setting_json("update_channel", &json!(channel).to_string());
            Ok("ok".into())
        }
        "update.download" => {
            let url = core
                .update
                .url
                .clone()
                .ok_or_else(|| anyhow!("no update available; run update.check first"))?;
            let dest = core
                .data_dir
                .join("updates")
                .join(format!("corcho-{}.AppImage", uuid::Uuid::new_v4().simple()));
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            report(progress, 0.1, "downloading update");
            runtime.block_on(updates::download(&url, &dest, |_| {}))?;
            core.update.downloading = false;
            core.update.progress = 100.0;
            core.update.status = "downloaded".into();
            Ok(json!({"path": dest.to_string_lossy()}).to_string())
        }
        "update.install" => {
            let path = text_arg(args, "path");
            let path = if path.is_empty() {
                let dir = core.data_dir.join("updates");
                std::fs::read_dir(&dir)
                    .ok()
                    .and_then(|rd| {
                        rd.flatten()
                            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
                            .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
                            .map(|e| e.path())
                    })
                    .ok_or_else(|| anyhow!("no downloaded update found"))?
            } else {
                validate_absolute_path(&path)?
            };
            let updates_dir = core.data_dir.join("updates");
            if !path.starts_with(&updates_dir) {
                return Err(anyhow!(
                    "update path must be inside {}",
                    updates_dir.to_string_lossy()
                ));
            }
            updates::install(&path)?;
            Ok(json!({"installing": path.to_string_lossy()}).to_string())
        }
        "app.launchAtStartup" => {
            let enabled = bool_arg(args, "enabled", false);
            if !crate::bridge::write_autostart(enabled) {
                return Err(anyhow!("could not update autostart entry"));
            }
            core.set_setting_json("launch_at_startup", &enabled.to_string());
            Ok("ok".into())
        }
        "app.openExternal" => {
            let url = required(args, "url")?;
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err(anyhow!("only http and https URLs are allowed"));
            }
            open::that_detached(&url)?;
            Ok("opened".into())
        }
        "app.relaunch" => {
            if text_arg(args, "confirm") != "yes" {
                return Err(anyhow!("pass confirm=\"yes\" to relaunch"));
            }
            core.save()?;
            let exe = std::env::current_exe()?;
            std::process::Command::new(exe).spawn()?;
            std::process::exit(0);
        }
        "diagnostics.run" => {
            let mut report = Vec::new();
            report.push(json!({"check": "workspace", "ok": !core.ws.grids.is_empty()}));
            report.push(json!({"check": "dataDir", "ok": core.data_dir.exists()}));
            let duplicate_cards = {
                let mut ids = std::collections::HashSet::new();
                let mut dupes = 0;
                for grid in &core.ws.grids {
                    for card in &grid.cards {
                        if !ids.insert(card.id.clone()) {
                            dupes += 1;
                        }
                    }
                }
                dupes
            };
            report.push(json!({"check": "duplicateCardIds", "ok": duplicate_cards == 0}));
            report.push(json!({"check": "revision", "value": core.ws.revision}));
            Ok(json!({"ok": true, "checks": report}).to_string())
        }
        "tutorial.reset" => {
            core.reset_tutorials();
            Ok("reset".into())
        }
        "onboarding.complete" => {
            core.set_setting_json("onboarding_completed", &json!(true).to_string());
            Ok("ok".into())
        }
        "data.setDir" => {
            let path = validate_absolute_path(&required(args, "path")?)?;
            core.set_data_dir(&path.to_string_lossy())?;
            Ok("ok".into())
        }
        "gdrive.connect" => Err(anyhow!(
            "gdrive.connect needs the GUI OAuth flow; run it from Settings"
        )),
        "gdrive.disconnect" => {
            core.set_setting_json("gdrive_connected", &json!(false).to_string());
            core.set_setting_json("gdrive_refresh_token", &json!(null).to_string());
            Ok("disconnected".into())
        }
        "adblock.hosts" => Ok(core.adblock.hosts_json()),
        "adblock.check" => {
            let url = required(args, "url")?;
            Ok(json!({"url": url, "blocked": core.adblock.blocked(&url)}).to_string())
        }
        "app.about" => Ok(json!({
            "name": "Corcho",
            "version": VERSION,
            "license": "MIT",
            "features": [
                "infinite canvas", "14 card types", "multi-calendar date links",
                "5 AI providers + local", "folder/Drive/git sync",
                "stable/beta/experimental updates", "MCP server"
            ]
        })
        .to_string()),
        "app.changelog" => Ok(CHANGELOG.into()),
        "attachments.list" => Ok(serde_json::to_string_pretty(
            &crate::storage::list_attachments(&core.data_dir),
        )?),
        "attachments.read" => {
            let name = required(args, "name")?;
            let bytes = crate::storage::read_attachment(&core.data_dir, &name)?;
            use base64::Engine;
            Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
        }
        "attachments.write" => {
            let name = required(args, "name")?;
            let data = required(args, "base64")?;
            validate_length(&data, 32_000_000, "base64 payload")?;
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|e| anyhow!("invalid base64: {e}"))?;
            let path = crate::storage::write_attachment(&core.data_dir, &name, &bytes)?;
            Ok(json!({"path": path.to_string_lossy(), "bytes": bytes.len()}).to_string())
        }
        "attachments.delete" => {
            let name = required(args, "name")?;
            crate::storage::delete_attachment(&core.data_dir, &name)?;
            Ok("deleted".into())
        }
        "script.run" => {
            if !core.ws.settings.allow_scripts {
                return Err(anyhow!(
                    "script execution is disabled; set allow_scripts=true first"
                ));
            }
            let script = required(args, "script")?;
            validate_length(&script, 100_000, "script")?;
            let timeout = args
                .get("timeoutSeconds")
                .and_then(|v| v.as_u64())
                .unwrap_or(30)
                .clamp(1, 300);
            report(progress, 0.2, "running script");
            let value = integrations::run_script(&script, std::time::Duration::from_secs(timeout))?;
            Ok(value.to_string())
        }
        "mpris.list" => Ok(integrations::mpris_players().to_string()),
        "mpris.action" => {
            let player = required(args, "player")?;
            let action = required(args, "action")?;
            Ok(integrations::mpris_action(&player, &action).to_string())
        }
        "mpris.metadata" => {
            let player = required(args, "player")?;
            Ok(integrations::mpris_metadata_named(&player).to_string())
        }
        "note.append" => {
            let id = required(args, "cardId")?;
            let extra = text_arg(args, "text");
            let card = core
                .ws
                .card(&id)
                .map(|(_, c)| c.clone())
                .ok_or_else(|| anyhow!("card not found"))?;
            let text = format!("{}\n{}", card.text, extra);
            core.update_card_json(&json!({"id": id, "text": text}).to_string());
            Ok("appended".into())
        }
        "note.replace" => {
            let id = required(args, "cardId")?;
            let text = text_arg(args, "text");
            core.update_card_json(&json!({"id": id, "text": text}).to_string());
            Ok("replaced".into())
        }
        "checklist.add" => {
            let id = required(args, "cardId")?;
            let text = required(args, "text")?;
            let mut card = core
                .ws
                .card(&id)
                .map(|(_, c)| c.clone())
                .ok_or_else(|| anyhow!("card not found"))?;
            let mut items = card
                .payload
                .get("items")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            items
                .push(json!({"id": uuid::Uuid::new_v4().to_string(), "text": text, "done": false}));
            card.set("items", json!(items));
            core.update_card_json(&json!({"id": id, "payload": card.payload}).to_string());
            Ok("added".into())
        }
        "checklist.update" => {
            let id = required(args, "cardId")?;
            let item_id = required(args, "itemId")?;
            let mut card = core
                .ws
                .card(&id)
                .map(|(_, c)| c.clone())
                .ok_or_else(|| anyhow!("card not found"))?;
            let mut items = card
                .payload
                .get("items")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            for item in items.iter_mut() {
                if item.get("id").and_then(|v| v.as_str()) == Some(&item_id) {
                    if let Some(text) = args.get("text").and_then(|v| v.as_str()) {
                        item["text"] = json!(text);
                    }
                    if let Some(done) = args.get("done").and_then(|v| v.as_bool()) {
                        item["done"] = json!(done);
                    }
                }
            }
            card.set("items", json!(items));
            core.update_card_json(&json!({"id": id, "payload": card.payload}).to_string());
            Ok("updated".into())
        }
        "checklist.remove" => {
            let id = required(args, "cardId")?;
            let item_id = required(args, "itemId")?;
            let mut card = core
                .ws
                .card(&id)
                .map(|(_, c)| c.clone())
                .ok_or_else(|| anyhow!("card not found"))?;
            let items: Vec<Value> = card
                .payload
                .get("items")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|i| i.get("id").and_then(|v| v.as_str()) != Some(&item_id))
                .collect();
            card.set("items", json!(items));
            core.update_card_json(&json!({"id": id, "payload": card.payload}).to_string());
            Ok("removed".into())
        }
        "checklist.toggle" => {
            let id = required(args, "cardId")?;
            let item_id = required(args, "itemId")?;
            let mut card = core
                .ws
                .card(&id)
                .map(|(_, c)| c.clone())
                .ok_or_else(|| anyhow!("card not found"))?;
            let mut items = card
                .payload
                .get("items")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            for item in items.iter_mut() {
                if item.get("id").and_then(|v| v.as_str()) == Some(&item_id) {
                    let done = item.get("done").and_then(|v| v.as_bool()).unwrap_or(false);
                    item["done"] = json!(!done);
                }
            }
            card.set("items", json!(items));
            core.update_card_json(&json!({"id": id, "payload": card.payload}).to_string());
            Ok("toggled".into())
        }
        "kanban.addItem" => {
            let id = required(args, "cardId")?;
            let column = required(args, "columnId")?;
            let text = required(args, "text")?;
            let mut card = core
                .ws
                .card(&id)
                .map(|(_, c)| c.clone())
                .ok_or_else(|| anyhow!("card not found"))?;
            let mut columns = card
                .payload
                .get("columns")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            for col in columns.iter_mut() {
                if col.get("id").and_then(|v| v.as_str()) == Some(&column) {
                    let items = col.get_mut("items").and_then(|v| v.as_array_mut());
                    let entry = json!({"id": uuid::Uuid::new_v4().to_string(), "text": text});
                    if let Some(items) = items {
                        items.push(entry);
                    }
                }
            }
            card.set("columns", json!(columns));
            core.update_card_json(&json!({"id": id, "payload": card.payload}).to_string());
            Ok("added".into())
        }
        "kanban.moveItem" => {
            let id = required(args, "cardId")?;
            let from = required(args, "fromColumn")?;
            let to = required(args, "toColumn")?;
            let item = required(args, "itemId")?;
            let mut card = core
                .ws
                .card(&id)
                .map(|(_, c)| c.clone())
                .ok_or_else(|| anyhow!("card not found"))?;
            let mut columns = card
                .payload
                .get("columns")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let mut moved: Option<Value> = None;
            for col in columns.iter_mut() {
                if col.get("id").and_then(|v| v.as_str()) == Some(&from) {
                    if let Some(items) = col.get_mut("items").and_then(|v| v.as_array_mut()) {
                        if let Some(pos) = items
                            .iter()
                            .position(|i| i.get("id").and_then(|v| v.as_str()) == Some(&item))
                        {
                            moved = Some(items.remove(pos));
                        }
                    }
                }
            }
            if let Some(entry) = moved {
                for col in columns.iter_mut() {
                    if col.get("id").and_then(|v| v.as_str()) == Some(&to) {
                        if let Some(items) = col.get_mut("items").and_then(|v| v.as_array_mut()) {
                            items.push(entry.clone());
                        }
                    }
                }
            }
            card.set("columns", json!(columns));
            core.update_card_json(&json!({"id": id, "payload": card.payload}).to_string());
            Ok("moved".into())
        }
        "kanban.removeItem" => {
            let id = required(args, "cardId")?;
            let column = required(args, "columnId")?;
            let item = required(args, "itemId")?;
            let mut card = core
                .ws
                .card(&id)
                .map(|(_, c)| c.clone())
                .ok_or_else(|| anyhow!("card not found"))?;
            let mut columns = card
                .payload
                .get("columns")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            for col in columns.iter_mut() {
                if col.get("id").and_then(|v| v.as_str()) == Some(&column) {
                    if let Some(items) = col.get_mut("items").and_then(|v| v.as_array_mut()) {
                        items.retain(|i| i.get("id").and_then(|v| v.as_str()) != Some(&item));
                    }
                }
            }
            card.set("columns", json!(columns));
            core.update_card_json(&json!({"id": id, "payload": card.payload}).to_string());
            Ok("removed".into())
        }
        "kanban.addColumn" => {
            let id = required(args, "cardId")?;
            let title = required(args, "title")?;
            let mut card = card_by_id(core, &id)?;
            let mut columns = card
                .payload
                .get("columns")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let column_id = text_arg(args, "columnId");
            let column_id = if column_id.is_empty() {
                format!("col-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
            } else {
                column_id
            };
            columns.push(json!({"id": column_id, "title": title, "items": []}));
            card.set("columns", json!(columns));
            core.update_card_json(&json!({"id": id, "payload": card.payload}).to_string());
            Ok("added".into())
        }
        "kanban.renameColumn" => {
            let id = required(args, "cardId")?;
            let column = required(args, "columnId")?;
            let title = required(args, "title")?;
            let mut card = card_by_id(core, &id)?;
            let mut columns = card
                .payload
                .get("columns")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            for col in columns.iter_mut() {
                if col.get("id").and_then(|v| v.as_str()) == Some(&column) {
                    col["title"] = json!(title);
                }
            }
            card.set("columns", json!(columns));
            core.update_card_json(&json!({"id": id, "payload": card.payload}).to_string());
            Ok("renamed".into())
        }
        "kanban.removeColumn" => {
            let id = required(args, "cardId")?;
            let column = required(args, "columnId")?;
            let mut card = card_by_id(core, &id)?;
            let columns: Vec<Value> = card
                .payload
                .get("columns")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|c| c.get("id").and_then(|v| v.as_str()) != Some(&column))
                .collect();
            card.set("columns", json!(columns));
            core.update_card_json(&json!({"id": id, "payload": card.payload}).to_string());
            Ok("removed".into())
        }
        "whiteboard.clear" => {
            let id = required(args, "cardId")?;
            core.update_card_json(&json!({"id": id, "payload": {"strokes": []}}).to_string());
            Ok("cleared".into())
        }
        "whiteboard.stroke" => {
            let id = required(args, "cardId")?;
            let mut card = core
                .ws
                .card(&id)
                .map(|(_, c)| c.clone())
                .ok_or_else(|| anyhow!("card not found"))?;
            let mut strokes = card
                .payload
                .get("strokes")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            strokes.push(json!({
                "color": args.get("color").cloned().unwrap_or(json!("#d98e5c")),
                "width": args.get("width").cloned().unwrap_or(json!(3)),
                "points": args.get("points").cloned().unwrap_or(json!([]))
            }));
            card.set("strokes", json!(strokes));
            core.update_card_json(&json!({"id": id, "payload": card.payload}).to_string());
            Ok("stroked".into())
        }
        "calendar.list" => Ok(core.calendar_json()),
        "calendar.create" => {
            let name = required(args, "name")?;
            validate_length(&name, 80, "calendar name")?;
            let color = text_arg(args, "color");
            let color = if color.is_empty() {
                "#d98e5c".to_string()
            } else {
                color
            };
            validate_color(&color)?;
            Ok(serde_json::to_string_pretty(
                &core.add_calendar(&name, &color),
            )?)
        }
        "calendar.update" => {
            let id = required(args, "calendarId")?;
            let name = text_arg(args, "name");
            let color = text_arg(args, "color");
            if !color.is_empty() {
                validate_color(&color)?;
            }
            if !name.is_empty() {
                validate_length(&name, 80, "calendar name")?;
            }
            if core.update_calendar(
                &id,
                if name.is_empty() { None } else { Some(&name) },
                if color.is_empty() { None } else { Some(&color) },
            ) {
                Ok("updated".into())
            } else {
                Err(anyhow!("calendar not found"))
            }
        }
        "calendar.delete" => {
            let id = required(args, "calendarId")?;
            if text_arg(args, "confirm") != "yes" {
                return Err(anyhow!("pass confirm=\"yes\" to delete a calendar"));
            }
            if core.delete_calendar(&id) {
                Ok("deleted".into())
            } else {
                Err(anyhow!("calendar not found"))
            }
        }
        "calendar.link.add" => {
            let date = required(args, "date")?;
            if !crate::core::valid_date(&date) {
                return Err(anyhow!("date must be YYYY-MM-DD"));
            }
            let calendar_ids: Vec<String> = args
                .get("calendarIds")
                .and_then(|v| v.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if calendar_ids.is_empty() {
                return Err(anyhow!("calendarIds must contain at least one calendar"));
            }
            let title = text_arg(args, "title");
            validate_length(&title, 4096, "title")?;
            let source_card = text_arg(args, "sourceCardId");
            let source_item = text_arg(args, "sourceItemId");
            if source_card.is_empty() && title.is_empty() {
                return Err(anyhow!("title is required for a standalone reminder"));
            }
            let link = core.add_date_link(
                calendar_ids,
                &date,
                &title,
                if source_card.is_empty() {
                    None
                } else {
                    Some(source_card)
                },
                if source_item.is_empty() {
                    None
                } else {
                    Some(source_item)
                },
            )?;
            Ok(serde_json::to_string_pretty(&link)?)
        }
        "calendar.link.update" => {
            let id = required(args, "linkId")?;
            let date = text_arg(args, "date");
            if !date.is_empty() && !crate::core::valid_date(&date) {
                return Err(anyhow!("date must be YYYY-MM-DD"));
            }
            let calendar_ids: Option<Vec<String>> = args
                .get("calendarIds")
                .and_then(|v| v.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                });
            let title = text_arg(args, "title");
            if !title.is_empty() {
                validate_length(&title, 4096, "title")?;
            }
            if core.update_date_link(
                &id,
                calendar_ids,
                if date.is_empty() { None } else { Some(&date) },
                if args.get("title").is_some() {
                    Some(&title)
                } else {
                    None
                },
            )? {
                Ok("updated".into())
            } else {
                Err(anyhow!("date link not found"))
            }
        }
        "calendar.link.delete" => {
            let id = required(args, "linkId")?;
            if core.delete_date_link(&id) {
                Ok("deleted".into())
            } else {
                Err(anyhow!("date link not found"))
            }
        }
        "calendar.link.list" => {
            let calendars: Vec<String> = args
                .get("calendarIds")
                .and_then(|v| v.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let source = text_arg(args, "sourceCardId");
            let value: Value = serde_json::from_str(&core.calendar_json())?;
            let mut links = value["dateLinks"].as_array().cloned().unwrap_or_default();
            if !calendars.is_empty() {
                links.retain(|link| {
                    link["calendarIds"]
                        .as_array()
                        .map(|ids| {
                            ids.iter().any(|id| {
                                id.as_str()
                                    .map(|id| calendars.iter().any(|wanted| wanted == id))
                                    .unwrap_or(false)
                            })
                        })
                        .unwrap_or(false)
                });
            }
            if !source.is_empty() {
                links.retain(|link| link["sourceCardId"].as_str() == Some(source.as_str()));
            }
            if let Some(from) = args.get("from").and_then(|v| v.as_str()) {
                if !crate::core::valid_date(from) {
                    return Err(anyhow!("from must be YYYY-MM-DD"));
                }
                links.retain(|link| link["date"].as_str().unwrap_or("") >= from);
            }
            if let Some(to) = args.get("to").and_then(|v| v.as_str()) {
                if !crate::core::valid_date(to) {
                    return Err(anyhow!("to must be YYYY-MM-DD"));
                }
                links.retain(|link| link["date"].as_str().unwrap_or("") <= to);
            }
            Ok(Value::Array(links).to_string())
        }
        "calendar.agenda" => {
            let from = required(args, "from")?;
            let to = required(args, "to")?;
            if !crate::core::valid_date(&from) || !crate::core::valid_date(&to) {
                return Err(anyhow!("from and to must be YYYY-MM-DD"));
            }
            let calendars: Vec<String> = args
                .get("calendarIds")
                .and_then(|v| v.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            Ok(Value::Array(core.calendar_agenda(&from, &to, &calendars)).to_string())
        }
        "calendar.setMonth" => {
            let id = required(args, "cardId")?;
            let year = args
                .get("year")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow!("year is required"))?;
            let month = args
                .get("month")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow!("month is required"))?;
            if core.set_calendar_month(&id, year, month) {
                Ok("ok".into())
            } else {
                Err(anyhow!("card not found or invalid month"))
            }
        }
        "calendar.today" => {
            let id = required(args, "cardId")?;
            if core.calendar_today(&id) {
                Ok("ok".into())
            } else {
                Err(anyhow!("card not found"))
            }
        }
        "timbre.configure" => {
            let id = required(args, "cardId")?;
            let mut patch = serde_json::Map::new();
            if let Some(volume) = args.get("volume").and_then(|v| v.as_f64()) {
                if !(0.0..=1.0).contains(&volume) {
                    return Err(anyhow!("volume must be between 0 and 1"));
                }
                patch.insert("volume".into(), json!(volume));
            }
            if let Some(freq) = args.get("toneFreq").and_then(|v| v.as_f64()) {
                if !(40.0..=4000.0).contains(&freq) {
                    return Err(anyhow!("toneFreq must be between 40 and 4000"));
                }
                patch.insert("toneFreq".into(), json!(freq));
            }
            if let Some(mix) = args.get("toneMix").and_then(|v| v.as_f64()) {
                if !(0.0..=1.0).contains(&mix) {
                    return Err(anyhow!("toneMix must be between 0 and 1"));
                }
                patch.insert("toneMix".into(), json!(mix));
            }
            core.update_card_json(&json!({"id": id, "payload": Value::Object(patch)}).to_string());
            Ok("configured".into())
        }
        "timbre.state" => {
            let id = required(args, "cardId")?;
            let card = card_by_id(core, &id)?;
            if card.kind != "timbre" {
                return Err(anyhow!("card is not a timbre card"));
            }
            Ok(json!({
                "cardId": id,
                "volume": card.payload.get("volume").cloned().unwrap_or(json!(0.35)),
                "toneFreq": card.payload.get("toneFreq").cloned().unwrap_or(json!(220.0)),
                "toneMix": card.payload.get("toneMix").cloned().unwrap_or(json!(0.4))
            })
            .to_string())
        }
        "portal.set" => {
            let id = required(args, "cardId")?;
            let target = required(args, "targetGridId")?;
            core.update_card_json(
                &json!({"id": id, "payload": {"targetGridId": target}}).to_string(),
            );
            Ok("linked".into())
        }
        "pomodoro.configure" => {
            let id = required(args, "cardId")?;
            let work = args
                .get("workMinutes")
                .and_then(|v| v.as_i64())
                .unwrap_or(25);
            let brk = args
                .get("breakMinutes")
                .and_then(|v| v.as_i64())
                .unwrap_or(5);
            core.update_card_json(
                &json!({"id": id, "payload": {
                    "workMinutes": work, "breakMinutes": brk,
                    "phase": "work", "remaining": work * 60
                }})
                .to_string(),
            );
            Ok("configured".into())
        }
        "pomodoro.reset" => {
            let id = required(args, "cardId")?;
            core.update_card_json(
                &json!({"id": id, "payload": {"phase": "work", "remaining": 1500}}).to_string(),
            );
            Ok("reset".into())
        }
        "api.fetch" => {
            let id = required(args, "cardId")?;
            let card = core
                .ws
                .card(&id)
                .map(|(_, c)| c.clone())
                .ok_or_else(|| anyhow!("card not found"))?;
            let url = if text_arg(args, "url").is_empty() {
                card.str_field("apiUrl", "")
            } else {
                text_arg(args, "url")
            };
            if url.is_empty() {
                return Err(anyhow!("no apiUrl on the card"));
            }
            let url = validate_url(&url)?;
            let method = if text_arg(args, "method").is_empty() {
                card.str_field("method", "GET")
            } else {
                text_arg(args, "method")
            };
            if !matches!(method.as_str(), "GET" | "POST" | "PUT" | "DELETE") {
                return Err(anyhow!("method must be GET, POST, PUT or DELETE"));
            }
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let client = reqwest::Client::new();
            let request = match method.as_str() {
                "POST" => client.post(&url),
                "PUT" => client.put(&url),
                "DELETE" => client.delete(&url),
                _ => client.get(&url),
            };
            report(progress, 0.3, "requesting endpoint");
            let response = runtime.block_on(request.send())?;
            let status = response.status().as_u16();
            let body = runtime.block_on(response.text())?;
            core.update_card_json(
                &json!({"id": id, "payload": {"lastBody": body, "status": status}}).to_string(),
            );
            Ok(body)
        }
        "dialog.open" => Err(anyhow!("dialog.open is GUI-only")),
        "quit" => {
            let confirm = bool_arg(args, "confirm", false);
            if confirm {
                std::process::exit(0);
            }
            Err(anyhow!("pass confirm=true"))
        }
        _ => Err(anyhow!("unknown tool: {name}")),
    }
}

fn title_case(value: &str) -> String {
    value
        .split(['.', '_', '-'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn property_description(key: &str) -> &'static str {
    match key {
        "gridId" => "Target grid id. Omit to use the active grid.",
        "cardId" => "Target card id.",
        "folderId" => "Target folder id.",
        "calendarId" => "Target calendar id.",
        "calendarIds" => "One or more calendar ids.",
        "linkId" => "Date link id.",
        "date" => "Local date in YYYY-MM-DD format.",
        "from" | "to" => "Inclusive date boundary in YYYY-MM-DD format.",
        "year" => "Calendar year.",
        "month" => "Month number from 0 (January) to 11 (December).",
        "toneFreq" => "Tone frequency in hertz, 40-4000.",
        "toneMix" => "Noise/tone mix from 0 (noise) to 1 (tone).",
        "sourceCardId" => "Optional card that owns the date.",
        "sourceItemId" => "Optional checklist or kanban item that owns the date.",
        "kind" => "Card kind: note, website, bookmark, api, ai, script, calendar, timbre, portal, checklist, kanban, whiteboard, pomodoro, mpris or file.",
        "x" | "y" => "Canvas coordinate in world units.",
        "width" | "height" => "Size in canvas units.",
        "title" => "Human-readable title.",
        "text" => "Text content.",
        "color" => "CSS hex colour, for example #d98e5c.",
        "icon" => "Lucide icon name, for example file-text.",
        "locked" => "Whether the card is locked against editing.",
        "payload" => "Kind-specific fields merged into the card payload.",
        "patch" => "Object of card fields to update.",
        "query" => "Search text. Matching is case-insensitive.",
        "limit" => "Maximum number of results.",
        "name" => "Attachment or display name.",
        "path" => "Absolute filesystem path.",
        "url" => "Absolute http or https URL.",
        "viewportWidth" | "viewportHeight" => "Canvas viewport size in device pixels.",
        "frame" => "World-space rectangle {x, y, width, height} to test against.",
        "expectedRevision" => "Reject the call unless the workspace revision matches.",
        "confirm" => "Explicit confirmation value required by destructive tools.",
        "base64" => "Standard base64-encoded bytes.",
        "itemId" => "Checklist or kanban item id.",
        "columnId" => "Kanban column id.",
        "fromColumn" | "toColumn" => "Source or destination kanban column id.",
        "provider" => "AI provider id: claude, openai, google, grok, mistral or local.",
        "model" => "Model id from ai.providers.",
        "apiKey" => "Provider API key. Never returned by reads.",
        "prompt" => "Prompt text.",
        "themeId" => "Theme id from theme.list.",
        "fontId" => "Font id from font.list.",
        "beforeId" => "Insert before this grid id; omit to append.",
        "targetGridId" => "Destination grid id.",
        "mode" => "Calendar or timbre mode value.",
        "accent" => "Accent colour.",
        "speed" => "Animation speed.",
        "workMinutes" | "breakMinutes" => "Timer length in minutes.",
        "volume" => "Master volume from 0 to 100.",
        "enabled" => "Enable or disable the feature.",
        "remote" => "Git remote URL.",
        "branch" => "Git branch name.",
        "script" => "Shell script to run.",
        "waitMs" => "Milliseconds to let the page load before capturing.",
        "timeoutSeconds" => "Execution timeout in seconds (1-300).",
        "format" => "Import format: corcho or legacy.",
        "keepContent" => "Keep grids when deleting a folder.",
        "validate" => "Validate the key without storing it.",
        "player" => "MPRIS player name.",
        "action" => "Player action, for example play, pause, next or previous.",
        _ => "Value for this field.",
    }
}

fn tool_annotations(name: &str) -> Value {
    let read_only = name.starts_with("workspace.get")
        || name == "workspace.revision"
        || name.starts_with("grid.list")
        || name.starts_with("grid.get")
        || name.starts_with("folder.list")
        || name.starts_with("card.list")
        || name.starts_with("card.get")
        || name.starts_with("card.search")
        || name == "card.visibility"
        || name.starts_with("view.get")
        || name.starts_with("frame.get")
        || name.starts_with("settings.get")
        || name.starts_with("theme.list")
        || name.starts_with("theme.get")
        || name == "theme.css.template"
        || name == "theme.css.get"
        || name.starts_with("font.list")
        || name.starts_with("ai.providers")
        || name == "ai.models.local"
        || name == "ai.stats"
        || name.starts_with("web.state")
        || name.starts_with("web.fetch")
        || name.starts_with("web.dom")
        || name.starts_with("web.screenshot")
        || name.starts_with("sync.status")
        || name.starts_with("iroh.status")
        || name.starts_with("iroh.peers")
        || name.starts_with("mpris.list")
        || name.starts_with("mpris.metadata")
        || name.starts_with("app.about")
        || name.starts_with("app.changelog")
        || name.starts_with("attachments.list")
        || name.starts_with("attachments.read")
        || name.starts_with("adblock.hosts")
        || name.starts_with("adblock.check")
        || name.starts_with("diagnostics.run")
        || name.starts_with("launcher.search")
        || name.starts_with("update.check");
    let destructive = matches!(
        name,
        "workspace.reset"
            | "grid.delete"
            | "folder.delete"
            | "card.delete"
            | "data.setDir"
            | "app.relaunch"
            | "quit"
            | "update.install"
            | "workspace.import"
            | "backup.import"
            | "theme.css.delete"
    );
    let network = name.starts_with("ai.")
        || name.starts_with("web.")
        || name.starts_with("sync.")
        || name.starts_with("iroh.")
        || name.starts_with("update.")
        || name.starts_with("gdrive.");
    let idempotent = name.contains(".set")
        || name.contains(".configure")
        || name.contains(".toggle")
        || name.contains(".lock")
        || name.contains(".reset")
        || name.contains(".clear")
        || name.contains(".stop");
    json!({
        "title": title_case(name),
        "readOnlyHint": read_only,
        "destructiveHint": destructive,
        "idempotentHint": idempotent,
        "openWorldHint": network
    })
}

fn tools() -> Vec<Value> {
    let empty = json!({});
    let mut list = Vec::new();
    let mut add = |name: &str, description: &str, props: Value, required: Vec<&str>| {
        let mut props = props;
        if let Some(object) = props.as_object_mut() {
            for (key, value) in object.iter_mut() {
                if let Some(schema) = value.as_object_mut() {
                    schema
                        .entry("title")
                        .or_insert_with(|| json!(title_case(key)));
                    schema
                        .entry("description")
                        .or_insert_with(|| json!(property_description(key)));
                }
            }
        }
        list.push(json!({
            "name": name,
            "title": title_case(name),
            "description": description,
            "annotations": tool_annotations(name),
            "inputSchema": {
                "type": "object",
                "properties": props,
                "required": required,
                "additionalProperties": false
            }
        }));
    };

    add(
        "workspace.get",
        "Return the full workspace: revision, viewport size, grids, folders, cards and settings. Secrets are redacted; use settings.get for the same redacted settings view.",
        empty.clone(),
        vec![],
    );
    add(
        "workspace.export",
        "Write a backup zip.",
        json!({"path":{"type":"string"}}),
        vec!["path"],
    );
    add(
        "workspace.import",
        "Import a backup or legacy folder.",
        json!({"path":{"type":"string"},"format":{"type":"string","enum":["corcho","legacy"]}}),
        vec!["path"],
    );
    add(
        "grid.list",
        "List grids with card counts, view state and folder membership. Use grid.get for the full card payloads.",
        json!({"folderId":{"type":"string"}}),
        vec![],
    );
    add(
        "grid.get",
        "Return one grid including its cards, pan, zoom and folder id. Use card.list with filters for a lighter response.",
        json!({"gridId":{"type":"string"}}),
        vec!["gridId"],
    );
    add(
        "grid.create",
        "Create a grid and switch to it.",
        json!({"name":{"type":"string"},"folderId":{"type":"string"}}),
        vec![],
    );
    add(
        "grid.rename",
        "Rename a grid.",
        json!({"gridId":{"type":"string"},"name":{"type":"string"}}),
        vec!["gridId", "name"],
    );
    add(
        "grid.delete",
        "Delete a grid (not the last one).",
        json!({"gridId":{"type":"string"}}),
        vec!["gridId"],
    );
    add(
        "grid.setIcon",
        "Set a grid icon to a Lucide name or an absolute .svg path.",
        json!({"gridId":{"type":"string"},"icon":{"type":"string"}}),
        vec!["gridId", "icon"],
    );
    add(
        "grid.switch",
        "Switch the active grid.",
        json!({"gridId":{"type":"string"}}),
        vec!["gridId"],
    );
    add(
        "grid.move",
        "Move a grid into a folder.",
        json!({"gridId":{"type":"string"},"folderId":{"type":"string"}}),
        vec!["gridId"],
    );
    add(
        "card.list",
        "List cards in a grid. Filter by kind or a case-insensitive query over title, text and payload keywords. Omit gridId for the active grid.",
        json!({"gridId":{"type":"string"},"kind":{"type":"string"},"query":{"type":"string"}}),
        vec![],
    );
    add(
        "card.get",
        "Return one card with its geometry, title, text, lock state and full kind-specific payload.",
        json!({"cardId":{"type":"string"}}),
        vec!["cardId"],
    );
    add(
        "card.create",
        "Create a card in the active grid.",
        json!({"gridId":{"type":"string"},"kind":{"type":"string"},"x":{"type":"number"},"y":{"type":"number"},"title":{"type":"string"},"width":{"type":"number"},"height":{"type":"number"},"payload":{"type":"object"}}),
        vec![],
    );
    add(
        "card.update",
        "Patch any card field: x, y, width, height, title, text, color, icon, locked, or a payload object. Omit patch to use the top-level arguments. Validation rejects invalid colours, icons, sizes and unknown payload shapes.",
        json!({"cardId":{"type":"string"},"patch":{"type":"object"}}),
        vec!["cardId"],
    );
    add(
        "card.delete",
        "Delete a card.",
        json!({"cardId":{"type":"string"}}),
        vec!["cardId"],
    );
    add(
        "card.duplicate",
        "Duplicate a card, optionally offset from the original.",
        json!({"cardId":{"type":"string"},"offset":{"type":"number"}}),
        vec!["cardId"],
    );
    add(
        "card.move",
        "Move a card to new canvas coordinates and/or into another grid. Returns the updated card.",
        json!({"cardId":{"type":"string"},"gridId":{"type":"string"},"x":{"type":"number"},"y":{"type":"number"}}),
        vec!["cardId"],
    );
    add(
        "card.search",
        "Search cards and grids.",
        json!({"query":{"type":"string"},"limit":{"type":"integer"}}),
        vec!["query"],
    );
    add(
        "file.pin",
        "Pin an image, PDF, Markdown file or folder on the active grid.",
        json!({"path":{"type":"string"},"gridId":{"type":"string"},"x":{"type":"number"},"y":{"type":"number"}}),
        vec!["path"],
    );
    add(
        "settings.get",
        "Read settings (secrets redacted).",
        empty.clone(),
        vec![],
    );
    add(
        "settings.set",
        "Set one setting.",
        json!({"key":{"type":"string"},"value":{}}),
        vec!["key", "value"],
    );
    add(
        "settings.batch",
        "Apply a settings patch atomically.",
        json!({"patch":{"type":"object"}}),
        vec!["patch"],
    );
    add("theme.list", "List themes.", empty.clone(), vec![]);
    add(
        "theme.get",
        "Return the system/light/dark theme selection and the effective theme when the mode is manual.",
        empty.clone(),
        vec![],
    );
    add(
        "theme.set",
        "Set a theme. Pass mode=light or mode=dark to change one slot; omit mode to set both slots.",
        json!({"themeId":{"type":"string"},"mode":{"type":"string","enum":["light","dark"]}}),
        vec!["themeId"],
    );
    add(
        "theme.mode.set",
        "Choose whether themes follow the system appearance, or force light or dark.",
        json!({"mode":{"type":"string","enum":["system","light","dark"]}}),
        vec!["mode"],
    );
    add("font.list", "List fonts.", empty.clone(), vec![]);
    add(
        "launcher.search",
        "Search the Launcher.",
        json!({"query":{"type":"string"}}),
        vec!["query"],
    );
    add(
        "launcher.run",
        "Run a Launcher command.",
        json!({"commandId":{"type":"string"}}),
        vec!["commandId"],
    );
    add(
        "ai.providers",
        "List providers and models.",
        empty.clone(),
        vec![],
    );
    add(
        "ai.complete",
        "Run a non-streaming AI completion. Omit provider and model to use the configured defaults. Streaming is not available over this transport yet.",
        json!({"provider":{"type":"string"},"model":{"type":"string"},"prompt":{"type":"string"}}),
        vec!["prompt"],
    );
    add(
        "sync.now",
        "Sync the folder provider.",
        empty.clone(),
        vec![],
    );
    add(
        "iroh.status",
        "Iroh peer-to-peer sync status, tickets and peers.",
        empty.clone(),
        vec![],
    );
    add(
        "iroh.create",
        "Create a new encrypted shared workspace and return its write setup code.",
        empty.clone(),
        vec![],
    );
    add(
        "iroh.join",
        "Join an existing shared workspace with a ticket.",
        json!({"ticket":{"type":"string"}}),
        vec!["ticket"],
    );
    add(
        "iroh.share",
        "Return a setup code for the current workspace.",
        json!({"readOnly":{"type":"boolean"}}),
        vec![],
    );
    add(
        "iroh.cache",
        "Return a read-only cache ticket without the encryption key.",
        empty.clone(),
        vec![],
    );
    add(
        "iroh.sync",
        "Publish local changes and pull remote ones.",
        empty.clone(),
        vec![],
    );
    add(
        "iroh.leave",
        "Leave the shared workspace on this device.",
        empty.clone(),
        vec![],
    );
    add(
        "iroh.peers",
        "List connected sync peers.",
        empty.clone(),
        vec![],
    );
    add(
        "backup.export",
        "Export a backup zip.",
        json!({"path":{"type":"string"}}),
        vec!["path"],
    );
    add(
        "backup.import",
        "Import a backup zip.",
        json!({"path":{"type":"string"}}),
        vec!["path"],
    );
    add(
        "update.check",
        "Check the update channel.",
        empty.clone(),
        vec![],
    );
    add(
        "update.channel.set",
        "Set stable/beta/experimental.",
        json!({"channel":{"type":"string","enum":["stable","beta","experimental"]}}),
        vec!["channel"],
    );
    add(
        "app.about",
        "App version and features.",
        empty.clone(),
        vec![],
    );
    add(
        "app.changelog",
        "Return the changelog.",
        empty.clone(),
        vec![],
    );
    add(
        "attachments.list",
        "List attachments.",
        empty.clone(),
        vec![],
    );
    add(
        "attachments.read",
        "Read an attachment as base64.",
        json!({"name":{"type":"string"}}),
        vec!["name"],
    );
    add(
        "script.run",
        "Run a script in /bin/sh. Disabled unless settings allow_scripts is true. The default timeout is 30 seconds.",
        json!({"script":{"type":"string"},"timeoutSeconds":{"type":"integer","minimum":1,"maximum":300}}),
        vec!["script"],
    );
    add("mpris.list", "List media players.", empty.clone(), vec![]);
    add(
        "mpris.action",
        "Control a media player.",
        json!({"player":{"type":"string"},"action":{"type":"string"}}),
        vec!["player", "action"],
    );
    add(
        "quit",
        "Quit the app (confirm=true).",
        json!({"confirm":{"type":"boolean"}}),
        vec!["confirm"],
    );

    add(
        "workspace.revision",
        "Return the current workspace revision.",
        empty.clone(),
        vec![],
    );
    add(
        "workspace.reset",
        "Wipe all data. Requires confirm=\"yes\".",
        json!({"confirm":{"type":"string"}}),
        vec!["confirm"],
    );
    add(
        "grid.reorder",
        "Reorder a grid, optionally before another grid.",
        json!({"gridId":{"type":"string"},"beforeId":{"type":"string"}}),
        vec!["gridId"],
    );
    add("folder.list", "List folders.", empty.clone(), vec![]);
    add(
        "folder.create",
        "Create a folder.",
        json!({"name":{"type":"string"}}),
        vec!["name"],
    );
    add(
        "folder.rename",
        "Rename a folder.",
        json!({"folderId":{"type":"string"},"name":{"type":"string"}}),
        vec!["folderId", "name"],
    );
    add(
        "folder.delete",
        "Delete a folder, optionally keeping its grids.",
        json!({"folderId":{"type":"string"},"keepContent":{"type":"boolean"}}),
        vec!["folderId"],
    );
    add(
        "folder.toggle",
        "Expand or collapse a folder.",
        json!({"folderId":{"type":"string"}}),
        vec!["folderId"],
    );
    add(
        "view.get",
        "Return the active (or requested) grid pan, zoom, canvas viewport size and the derived world-space frame.",
        json!({"gridId":{"type":"string"}}),
        vec![],
    );
    add(
        "view.set",
        "Set pan, zoom and canvas viewport size. Viewport size is normally reported by the GUI; pass it explicitly when headless.",
        json!({"gridId":{"type":"string"},"panX":{"type":"number"},"panY":{"type":"number"},"zoom":{"type":"number"},"viewportWidth":{"type":"number"},"viewportHeight":{"type":"number"}}),
        vec![],
    );
    add(
        "grid.view",
        "Alias of view.set.",
        json!({"gridId":{"type":"string"},"panX":{"type":"number"},"panY":{"type":"number"},"zoom":{"type":"number"},"viewportWidth":{"type":"number"},"viewportHeight":{"type":"number"}}),
        vec![],
    );
    add(
        "frame.get",
        "Return the visible world rectangle derived from the grid pan/zoom and the reported canvas size. Pass frame, viewportWidth or viewportHeight to override it.",
        json!({"gridId":{"type":"string"},"frame":{"type":"object"},"viewportWidth":{"type":"number"},"viewportHeight":{"type":"number"}}),
        vec![],
    );
    add(
        "card.visibility",
        "Compute card visibility against the current canvas frame or an explicit world-space rectangle. Returns inside, intersects and visibleFraction for every card in the grid.",
        json!({"gridId":{"type":"string"},"frame":{"type":"object"},"viewportWidth":{"type":"number"},"viewportHeight":{"type":"number"}}),
        vec![],
    );
    add(
        "card.resize",
        "Resize a card.",
        json!({"cardId":{"type":"string"},"width":{"type":"number"},"height":{"type":"number"}}),
        vec!["cardId"],
    );
    add(
        "card.setColor",
        "Set a card colour.",
        json!({"cardId":{"type":"string"},"color":{"type":"string"}}),
        vec!["cardId", "color"],
    );
    add(
        "card.setIcon",
        "Set a card icon.",
        json!({"cardId":{"type":"string"},"icon":{"type":"string"}}),
        vec!["cardId", "icon"],
    );
    add(
        "card.lock",
        "Lock or unlock a card.",
        json!({"cardId":{"type":"string"},"locked":{"type":"boolean"}}),
        vec!["cardId"],
    );
    add(
        "note.append",
        "Append text to a note.",
        json!({"cardId":{"type":"string"},"text":{"type":"string"}}),
        vec!["cardId", "text"],
    );
    add(
        "note.replace",
        "Replace note text.",
        json!({"cardId":{"type":"string"},"text":{"type":"string"}}),
        vec!["cardId", "text"],
    );
    add(
        "checklist.add",
        "Add a checklist item.",
        json!({"cardId":{"type":"string"},"text":{"type":"string"}}),
        vec!["cardId", "text"],
    );
    add(
        "checklist.update",
        "Update a checklist item's text or done state.",
        json!({"cardId":{"type":"string"},"itemId":{"type":"string"},"text":{"type":"string"},"done":{"type":"boolean"}}),
        vec!["cardId", "itemId"],
    );
    add(
        "checklist.remove",
        "Remove a checklist item.",
        json!({"cardId":{"type":"string"},"itemId":{"type":"string"}}),
        vec!["cardId", "itemId"],
    );
    add(
        "checklist.toggle",
        "Toggle a checklist item.",
        json!({"cardId":{"type":"string"},"itemId":{"type":"string"}}),
        vec!["cardId", "itemId"],
    );
    add(
        "kanban.addColumn",
        "Add a kanban column.",
        json!({"cardId":{"type":"string"},"title":{"type":"string"},"columnId":{"type":"string"}}),
        vec!["cardId", "title"],
    );
    add(
        "kanban.renameColumn",
        "Rename a kanban column.",
        json!({"cardId":{"type":"string"},"columnId":{"type":"string"},"title":{"type":"string"}}),
        vec!["cardId", "columnId", "title"],
    );
    add(
        "kanban.removeColumn",
        "Remove a kanban column.",
        json!({"cardId":{"type":"string"},"columnId":{"type":"string"}}),
        vec!["cardId", "columnId"],
    );
    add(
        "kanban.addItem",
        "Add a kanban item.",
        json!({"cardId":{"type":"string"},"columnId":{"type":"string"},"text":{"type":"string"}}),
        vec!["cardId", "columnId", "text"],
    );
    add(
        "kanban.moveItem",
        "Move a kanban item between columns.",
        json!({"cardId":{"type":"string"},"fromColumn":{"type":"string"},"toColumn":{"type":"string"},"itemId":{"type":"string"}}),
        vec!["cardId", "fromColumn", "toColumn", "itemId"],
    );
    add(
        "kanban.removeItem",
        "Remove a kanban item.",
        json!({"cardId":{"type":"string"},"columnId":{"type":"string"},"itemId":{"type":"string"}}),
        vec!["cardId", "columnId", "itemId"],
    );
    add(
        "whiteboard.clear",
        "Clear all whiteboard strokes.",
        json!({"cardId":{"type":"string"}}),
        vec!["cardId"],
    );
    add(
        "whiteboard.stroke",
        "Append a whiteboard stroke.",
        json!({"cardId":{"type":"string"},"color":{"type":"string"},"width":{"type":"number"},"points":{"type":"array"}}),
        vec!["cardId"],
    );
    add(
        "pomodoro.configure",
        "Configure a pomodoro card.",
        json!({"cardId":{"type":"string"},"workMinutes":{"type":"integer"},"breakMinutes":{"type":"integer"}}),
        vec!["cardId"],
    );
    add(
        "pomodoro.reset",
        "Reset a pomodoro card.",
        json!({"cardId":{"type":"string"}}),
        vec!["cardId"],
    );
    add(
        "api.fetch",
        "Run an API Data card request and store the body.",
        json!({"cardId":{"type":"string"},"url":{"type":"string"},"method":{"type":"string"}}),
        vec!["cardId"],
    );
    add(
        "calendar.list",
        "Return every calendar definition and every resolved date link.",
        empty.clone(),
        vec![],
    );
    add(
        "calendar.create",
        "Create a calendar with a name and colour.",
        json!({"name":{"type":"string"},"color":{"type":"string"}}),
        vec!["name"],
    );
    add(
        "calendar.update",
        "Rename or recolour a calendar.",
        json!({"calendarId":{"type":"string"},"name":{"type":"string"},"color":{"type":"string"}}),
        vec!["calendarId"],
    );
    add(
        "calendar.delete",
        "Delete a calendar and every date linked to it. Requires confirm=\"yes\".",
        json!({"calendarId":{"type":"string"},"confirm":{"type":"string"}}),
        vec!["calendarId", "confirm"],
    );
    add(
        "calendar.link.add",
        "Link a date to one or more calendars. A source card or item is optional for standalone reminders.",
        json!({"date":{"type":"string"},"calendarIds":{"type":"array","items":{"type":"string"}},"title":{"type":"string"},"sourceCardId":{"type":"string"},"sourceItemId":{"type":"string"}}),
        vec!["date", "calendarIds"],
    );
    add(
        "calendar.link.update",
        "Change a date link's date, calendars or title.",
        json!({"linkId":{"type":"string"},"date":{"type":"string"},"calendarIds":{"type":"array","items":{"type":"string"}},"title":{"type":"string"}}),
        vec!["linkId"],
    );
    add(
        "calendar.link.delete",
        "Delete a date link.",
        json!({"linkId":{"type":"string"}}),
        vec!["linkId"],
    );
    add(
        "calendar.link.list",
        "List resolved date links, filtered by calendars, source card or date range.",
        json!({"calendarIds":{"type":"array","items":{"type":"string"}},"from":{"type":"string"},"to":{"type":"string"},"sourceCardId":{"type":"string"}}),
        vec![],
    );
    add(
        "calendar.agenda",
        "Return resolved date links between two YYYY-MM-DD dates.",
        json!({"from":{"type":"string"},"to":{"type":"string"},"calendarIds":{"type":"array","items":{"type":"string"}}}),
        vec!["from", "to"],
    );
    add(
        "calendar.setMonth",
        "Set the month shown by a calendar card. Month is 0-11.",
        json!({"cardId":{"type":"string"},"year":{"type":"integer"},"month":{"type":"integer"}}),
        vec!["cardId", "year", "month"],
    );
    add(
        "calendar.today",
        "Reset a calendar card to the current month.",
        json!({"cardId":{"type":"string"}}),
        vec!["cardId"],
    );
    add(
        "timbre.configure",
        "Set a timbre card's volume, tone frequency and noise/tone mix without starting playback.",
        json!({"cardId":{"type":"string"},"volume":{"type":"number"},"toneFreq":{"type":"number"},"toneMix":{"type":"number"}}),
        vec!["cardId"],
    );
    add(
        "timbre.state",
        "Return a timbre card's saved audio settings.",
        json!({"cardId":{"type":"string"}}),
        vec!["cardId"],
    );
    add(
        "portal.set",
        "Set a grid portal target.",
        json!({"cardId":{"type":"string"},"targetGridId":{"type":"string"}}),
        vec!["cardId", "targetGridId"],
    );
    add(
        "ai.validateKey",
        "Validate an AI provider API key.",
        json!({"provider":{"type":"string"},"apiKey":{"type":"string"}}),
        vec!["provider", "apiKey"],
    );
    add(
        "ai.setKey",
        "Store an AI provider API key.",
        json!({"provider":{"type":"string"},"apiKey":{"type":"string"}}),
        vec!["provider", "apiKey"],
    );
    add(
        "ai.clearKey",
        "Remove an AI provider API key.",
        json!({"provider":{"type":"string"}}),
        vec!["provider"],
    );
    add(
        "ai.setModel",
        "Set the default model for a provider.",
        json!({"provider":{"type":"string"},"model":{"type":"string"}}),
        vec!["provider", "model"],
    );
    add(
        "ai.models.local",
        "List models from a local OpenAI-compatible server.",
        json!({"serverUrl":{"type":"string"},"apiKey":{"type":"string"}}),
        vec!["serverUrl"],
    );
    add(
        "ai.card.run",
        "Run an AI card with its saved prompt and context.",
        json!({"cardId":{"type":"string"}}),
        vec!["cardId"],
    );
    add(
        "ai.stats",
        "Return AI card counts and prompt bytes.",
        empty.clone(),
        vec![],
    );
    add(
        "web.state",
        "Return a web card's stored URL and geometry.",
        json!({"cardId":{"type":"string"}}),
        vec!["cardId"],
    );
    add(
        "web.navigate",
        "Set the URL stored on a web card.",
        json!({"cardId":{"type":"string"},"url":{"type":"string"}}),
        vec!["cardId", "url"],
    );
    add(
        "web.fetch",
        "Fetch a web card URL over HTTP and return status, content type and up to 2 MB of body text. Use web.dom for JavaScript-rendered pages.",
        json!({"cardId":{"type":"string"},"url":{"type":"string"}}),
        vec![],
    );
    add(
        "web.dom",
        "Render a web card URL in headless Chromium and return the DOM after JavaScript runs. Use waitMs to allow slow pages to settle.",
        json!({"cardId":{"type":"string"},"url":{"type":"string"},"waitMs":{"type":"integer"}}),
        vec![],
    );
    add(
        "web.screenshot",
        "Render a web card URL in headless Chromium and return a text summary plus an MCP image content block.",
        json!({"cardId":{"type":"string"},"url":{"type":"string"},"width":{"type":"number"},"height":{"type":"number"},"waitMs":{"type":"integer"}}),
        vec![],
    );
    add(
        "theme.custom.set",
        "Set and activate a custom theme.",
        json!({"background":{"type":"string"},"surface":{"type":"string"},"accent":{"type":"string"},"text":{"type":"string"}}),
        vec!["background", "surface", "accent", "text"],
    );
    add(
        "theme.css.template",
        "Return the annotated CSS theme template containing every theme token.",
        empty.clone(),
        vec![],
    );
    add(
        "theme.css.get",
        "Read a CSS theme from the data directory.",
        json!({"themeId":{"type":"string"}}),
        vec!["themeId"],
    );
    add(
        "theme.css.set",
        "Validate and create or replace a CSS theme in the data directory.",
        json!({"themeId":{"type":"string"},"css":{"type":"string"}}),
        vec!["themeId", "css"],
    );
    add(
        "theme.css.delete",
        "Delete a user CSS theme.",
        json!({"themeId":{"type":"string"}}),
        vec!["themeId"],
    );
    add(
        "font.set",
        "Set the UI font.",
        json!({"fontId":{"type":"string"}}),
        vec!["fontId"],
    );
    add(
        "effects.set",
        "Toggle spawn, parallax, ripple and reduced-motion effects.",
        json!({"spawn":{"type":"boolean"},"parallax":{"type":"boolean"},"ripple":{"type":"boolean"},"reducedMotion":{"type":"boolean"}}),
        vec![],
    );
    add(
        "audio.setVolume",
        "Set master volume (0-100).",
        json!({"volume":{"type":"integer"}}),
        vec!["volume"],
    );
    add(
        "sync.status",
        "Return cloud sync state.",
        empty.clone(),
        vec![],
    );
    add(
        "sync.configure",
        "Configure the folder sync provider.",
        json!({"enabled":{"type":"boolean"},"provider":{"type":"string"},"folder":{"type":"string"}}),
        vec![],
    );
    add(
        "gdrive.connect",
        "Start Google Drive OAuth (requires the GUI).",
        empty.clone(),
        vec![],
    );
    add(
        "gdrive.disconnect",
        "Forget Google Drive credentials.",
        empty.clone(),
        vec![],
    );
    add(
        "update.download",
        "Download the pending update.",
        empty.clone(),
        vec![],
    );
    add(
        "update.install",
        "Install the newest downloaded update, or the given path.",
        json!({"path":{"type":"string"}}),
        vec![],
    );
    add(
        "app.launchAtStartup",
        "Enable or disable launch at startup.",
        json!({"enabled":{"type":"boolean"}}),
        vec!["enabled"],
    );
    add(
        "app.openExternal",
        "Open an http(s) URL in the default browser.",
        json!({"url":{"type":"string"}}),
        vec!["url"],
    );
    add(
        "app.relaunch",
        "Relaunch the app. Requires confirm=\"yes\".",
        json!({"confirm":{"type":"string"}}),
        vec!["confirm"],
    );
    add(
        "diagnostics.run",
        "Run integrity checks.",
        empty.clone(),
        vec![],
    );
    add(
        "tutorial.reset",
        "Reset in-app tutorials and onboarding.",
        empty.clone(),
        vec![],
    );
    add(
        "onboarding.complete",
        "Mark onboarding as complete.",
        empty.clone(),
        vec![],
    );
    add(
        "data.setDir",
        "Move the data directory.",
        json!({"path":{"type":"string"}}),
        vec!["path"],
    );
    add(
        "attachments.write",
        "Write an attachment from base64.",
        json!({"name":{"type":"string"},"base64":{"type":"string"}}),
        vec!["name", "base64"],
    );
    add(
        "attachments.delete",
        "Delete an attachment.",
        json!({"name":{"type":"string"}}),
        vec!["name"],
    );
    add(
        "mpris.metadata",
        "Read metadata from a media player.",
        json!({"player":{"type":"string"}}),
        vec!["player"],
    );
    add(
        "adblock.hosts",
        "Return the active ad-block host list.",
        empty.clone(),
        vec![],
    );
    add(
        "adblock.check",
        "Check whether a URL would be blocked.",
        json!({"url":{"type":"string"}}),
        vec!["url"],
    );
    list
}

fn resources() -> Vec<Value> {
    vec![
        json!({"uri":"corcho://workspace","name":"Workspace","mimeType":"application/json"}),
        json!({"uri":"corcho://settings","name":"Settings","mimeType":"application/json"}),
        json!({"uri":"corcho://themes","name":"Themes","mimeType":"application/json"}),
        json!({"uri":"corcho://models","name":"Models","mimeType":"application/json"}),
        json!({"uri":"corcho://diagnostics","name":"Diagnostics","mimeType":"application/json"}),
        json!({"uri":"corcho://changelog","name":"Changelog","mimeType":"text/markdown"}),
    ]
}

fn read_resource(core: &mut Core, uri: &str) -> Result<(&'static str, String)> {
    if uri == "corcho://workspace" {
        return Ok((
            "application/json",
            serde_json::to_string_pretty(&workspace_public(&core.ws))?,
        ));
    }
    if uri == "corcho://settings" {
        return Ok((
            "application/json",
            serde_json::to_string_pretty(&settings_public(&core.ws.settings))?,
        ));
    }
    if uri == "corcho://themes" {
        return Ok(("application/json", core.themes_json()));
    }
    if uri == "corcho://models" {
        return Ok(("application/json", model::models_by_provider().to_string()));
    }
    if uri == "corcho://diagnostics" {
        return Ok((
            "application/json",
            json!({
                "revision": core.ws.revision,
                "grids": core.ws.grids.len(),
                "cards": core.ws.grids.iter().map(|g| g.cards.len()).sum::<usize>(),
                "folders": core.ws.folders.len(),
                "dataDir": core.data_dir.to_string_lossy(),
                "viewport": {"width": core.ws.viewport_width, "height": core.ws.viewport_height}
            })
            .to_string(),
        ));
    }
    if uri == "corcho://changelog" {
        return Ok(("text/markdown", CHANGELOG.into()));
    }
    if let Some(id) = uri.strip_prefix("corcho://grid/") {
        let grid = core.ws.grid(id).ok_or_else(|| anyhow!("grid not found"))?;
        return Ok(("application/json", serde_json::to_string_pretty(grid)?));
    }
    if let Some(id) = uri.strip_prefix("corcho://card/") {
        let card = core
            .ws
            .card(id)
            .map(|(_, c)| c)
            .ok_or_else(|| anyhow!("card not found"))?;
        return Ok(("application/json", serde_json::to_string_pretty(card)?));
    }
    Err(anyhow!("unknown resource: {uri}"))
}

fn prompts() -> Vec<Value> {
    vec![
        json!({"name":"organize-inbox","description":"Triage uncategorized notes into grids and checklists."}),
        json!({"name":"summarize-grid","description":"Summarize the active grid and its open work."}),
        json!({"name":"weekly-review","description":"Collect pomodoros, checklists and notes into a weekly review."}),
        json!({"name":"research-to-cards","description":"Run a web/AI research pass and create cards from it."}),
        json!({"name":"clean-duplicates","description":"Find duplicate cards and propose merges."}),
    ]
}

fn get_prompt(name: &str) -> Value {
    let text = match name {
        "organize-inbox" => "Read corcho://workspace, find notes that are not in a meaningful grid, and propose card.create calls to organize them. Use card.move for existing cards.",
        "summarize-grid" => "Read the active grid with grid.get and card.list, then write a short summary of themes, open tasks and stale items.",
        "weekly-review" => "Collect completed checklists, pomodoro sessions and notes from the last seven days. Write a weekly review card with highlight, blockers and next steps.",
        "research-to-cards" => "Use web.fetch or web.dom on the relevant URLs, summarize the result with ai.complete, then create note cards with card.create in the active grid.",
        "clean-duplicates" => "Read card.list for every grid, group cards with the same title or URL, and propose card.delete or card.update calls. Do not delete without listing the candidates first.",
        _ => "Unknown prompt.",
    };
    json!({
        "description": name,
        "messages": [{ "role": "user", "content": { "type": "text", "text": text } }]
    })
}

const CHANGELOG: &str = "Corcho 0.1.0\n\n- Native Rust + Qt6 rewrite.\n- Infinite canvas and fourteen card types.\n- Calendar cards with multi-calendar date links and reminders.\n- Dedicated Timbre audio card.\n- Five AI providers plus a local server.\n- Folder, Google Drive and git sync with union conflict merge.\n- Stable, beta and experimental update channels.\n- MCP server with tools, resources and prompts.\n- Fully free software (MIT), no telemetry.\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_and_tools_list_are_valid() {
        let mut core = Core::fallback();
        let init = handle(
            &mut core,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}),
        )
        .unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "corcho");

        let list = handle(
            &mut core,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .unwrap();
        let tools = list["result"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|t| t["name"] == "card.create"));
        assert!(tools.iter().any(|t| t["name"] == "iroh.status"));
    }

    #[test]
    fn missing_argument_is_an_error_result() {
        let mut core = Core::fallback();
        let response = handle(
            &mut core,
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"grid.get","arguments":{}}}),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], true);
    }

    #[test]
    fn registry_advertises_every_card_tool() {
        let list = tools();
        let names: Vec<&str> = list.iter().filter_map(|t| t["name"].as_str()).collect();
        for name in [
            "note.append",
            "note.replace",
            "checklist.add",
            "checklist.update",
            "checklist.remove",
            "checklist.toggle",
            "kanban.addColumn",
            "kanban.removeColumn",
            "kanban.addItem",
            "kanban.moveItem",
            "kanban.removeItem",
            "whiteboard.clear",
            "whiteboard.stroke",
            "pomodoro.configure",
            "pomodoro.reset",
            "api.fetch",
            "calendar.link.add",
            "calendar.agenda",
            "timbre.configure",
            "portal.set",
            "web.screenshot",
            "web.dom",
            "view.get",
            "frame.get",
            "card.visibility",
            "folder.create",
            "attachments.write",
        ] {
            assert!(names.contains(&name), "missing tool: {name}");
        }
    }

    #[test]
    fn visibility_reports_inside_and_outside_cards() {
        let dir = tempfile::tempdir().unwrap();
        let ws = model::Workspace::default();
        let mut core = Core {
            data_dir: dir.path().to_path_buf(),
            history: crate::history::History::new(20, ws.clone()),
            ws,
            adblock: crate::integrations::Adblock::new(),
            iroh: crate::irohsync::IrohSync::disabled(),
            update: crate::updates::UpdateState::default(),
            last_saved: None,
            last_file_mtime: None,
        };
        let inside = core.create_card("note", 20.0, 20.0).unwrap();
        let outside = core.create_card("note", 4000.0, 4000.0).unwrap();
        let response = handle(
            &mut core,
            json!({
                "jsonrpc":"2.0","id":9,"method":"tools/call",
                "params":{"name":"card.visibility","arguments":{"viewportWidth":1000,"viewportHeight":800}}
            }),
        )
        .unwrap();
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        let value: Value = serde_json::from_str(text).unwrap();
        let cards = value["cards"].as_array().unwrap();
        let find = |id: &str| cards.iter().find(|c| c["id"] == id).unwrap();
        assert_eq!(find(&inside.id)["inside"], true);
        assert_eq!(find(&outside.id)["inside"], false);
    }
    fn temp_core() -> (tempfile::TempDir, Core) {
        let dir = tempfile::tempdir().unwrap();
        let ws = model::Workspace::default();
        let core = Core {
            data_dir: dir.path().to_path_buf(),
            history: crate::history::History::new(20, ws.clone()),
            ws,
            adblock: crate::integrations::Adblock::new(),
            iroh: crate::irohsync::IrohSync::disabled(),
            update: crate::updates::UpdateState::default(),
            last_saved: None,
            last_file_mtime: None,
        };
        (dir, core)
    }

    fn spawn_mcp_http(core: Arc<Mutex<Core>>, token: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            let _ = serve_listener(listener, core, token, None);
        });
        addr
    }

    fn http_raw(
        addr: &str,
        token: &str,
        method: &str,
        extra_headers: &[(&str, &str)],
        body: &str,
    ) -> String {
        let mut req = format!(
            "{method} /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\n"
        );
        for (key, value) in extra_headers {
            req.push_str(&format!("{key}: {value}\r\n"));
        }
        req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        let _ = stream.read_to_string(&mut out);
        out
    }

    #[test]
    fn http_streamable_transport_negotiates_and_streams() {
        let (_dir, core) = temp_core();
        let core = Arc::new(Mutex::new(core));
        let token = {
            let c = core.lock().unwrap();
            http_token(&c.data_dir).unwrap()
        };
        let addr = spawn_mcp_http(core, token.clone());

        let init = http_raw(
            &addr,
            &token,
            "POST",
            &[
                ("Accept", "application/json"),
                ("Content-Type", "application/json"),
            ],
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#,
        );
        assert!(init.starts_with("HTTP/1.1 200"), "{init}");
        assert!(
            init.contains("\"protocolVersion\":\"2025-06-18\""),
            "{init}"
        );

        let note = http_raw(
            &addr,
            &token,
            "POST",
            &[("Content-Type", "application/json")],
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        );
        assert!(note.starts_with("HTTP/1.1 202"), "{note}");
        assert!(note.ends_with("\r\n\r\n"), "{note}");

        let sse = http_raw(
            &addr,
            &token,
            "POST",
            &[
                ("Accept", "application/json, text/event-stream"),
                ("Content-Type", "application/json"),
            ],
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        );
        assert!(sse.contains("Content-Type: text/event-stream"), "{sse}");
        assert!(sse.contains("event: message"), "{sse}");
        assert!(sse.contains("card.create"), "{sse}");

        let get = http_raw(&addr, &token, "GET", &[], "");
        assert!(get.starts_with("HTTP/1.1 405"), "{get}");
    }

    #[test]
    fn http_transport_rejects_bad_auth_and_protocol_version() {
        let (_dir, core) = temp_core();
        let core = Arc::new(Mutex::new(core));
        let token = {
            let c = core.lock().unwrap();
            http_token(&c.data_dir).unwrap()
        };
        let addr = spawn_mcp_http(core, token.clone());

        let unauth = http_raw(
            &addr,
            "wrong",
            "POST",
            &[("Content-Type", "application/json")],
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
        );
        assert!(unauth.starts_with("HTTP/1.1 401"), "{unauth}");

        let badver = http_raw(
            &addr,
            &token,
            "POST",
            &[
                ("Content-Type", "application/json"),
                ("MCP-Protocol-Version", "1999-01-01"),
            ],
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
        );
        assert!(badver.starts_with("HTTP/1.1 400"), "{badver}");
    }

    #[test]
    fn progress_notifications_are_streamed_to_the_client() {
        let (_dir, core) = temp_core();
        let core = Arc::new(Mutex::new(core));
        let token = {
            let c = core.lock().unwrap();
            http_token(&c.data_dir).unwrap()
        };
        let addr = spawn_mcp_http(core, token.clone());

        let sse = http_raw(
            &addr,
            &token,
            "POST",
            &[
                ("Accept", "application/json, text/event-stream"),
                ("Content-Type", "application/json"),
            ],
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"workspace.revision","arguments":{},"_meta":{"progressToken":"tok-1"}}}"#,
        );
        assert!(sse.contains("notifications/progress"), "{sse}");
        assert!(sse.contains("\"progressToken\":\"tok-1\""), "{sse}");
        assert!(sse.contains("\"id\":3"), "{sse}");
    }

    fn generated_args(
        tool: &Value,
        grid_id: &str,
        card_id: &str,
        folder_id: &str,
        calendar_id: &str,
        link_id: &str,
    ) -> Value {
        let mut args = serde_json::Map::new();
        let required = tool["inputSchema"]["required"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for key in required.iter().filter_map(|v| v.as_str()) {
            let value = match key {
                "gridId" | "targetGridId" => json!(grid_id),
                "cardId" => json!(card_id),
                "folderId" => json!(folder_id),
                "calendarId" => json!(calendar_id),
                "linkId" => json!(link_id),
                "calendarIds" => json!([calendar_id]),
                "date" | "from" | "to" => json!("2026-11-01"),
                "year" => json!(2026),
                "month" => json!(10),
                "toneFreq" => json!(220.0),
                "toneMix" => json!(0.4),
                "itemId" => json!("item"),
                "columnId" | "fromColumn" => json!("todo"),
                "toColumn" => json!("doing"),
                "kind" => json!("note"),
                "text" | "title" | "name" | "prompt" | "query" => json!("x"),
                "provider" => json!("openai"),
                "model" => json!("gpt-5.4"),
                "apiKey" => json!("test-key"),
                "themeId" => json!("corcho"),
                "fontId" => json!("inter"),
                "url" => json!("https://example.com/"),
                "color" | "accent" | "background" | "surface" => json!("#121417"),
                "icon" => json!("file-text"),
                "mode" => json!("thrum"),
                "speed" => json!("medium"),
                "path" => json!("/tmp/corcho-mcp-smoke-does-not-exist"),
                "confirm" => json!("no"),
                "key" => json!("theme"),
                "value" => json!("corcho"),
                "patch" | "payload" | "frame" => json!({}),
                "script" => json!("true"),
                "base64" => json!("eA=="),
                "player" => json!("none"),
                "action" => json!("play"),
                "channel" => json!("stable"),
                "volume" => json!(50),
                "workMinutes" | "breakMinutes" => json!(25),
                "width" | "height" => json!(320),
                "x" | "y" => json!(10),
                "panX" | "panY" => json!(0),
                "zoom" => json!(1),
                "viewportWidth" => json!(800),
                "viewportHeight" => json!(600),
                "enabled" => json!(false),
                "locked" => json!(false),
                "done" => json!(true),
                "format" => json!("corcho"),
                "beforeId" => json!(""),
                _ => json!("x"),
            };
            args.insert(key.to_string(), value);
        }
        if tool["name"] == "calendar.link.add" {
            args.insert("title".into(), json!("smoke reminder"));
        }
        Value::Object(args)
    }

    #[test]
    fn every_advertised_tool_is_callable() {
        let skip = [
            "quit",
            "app.relaunch",
            "app.launchAtStartup",
            "app.openExternal",
            "data.setDir",
            "workspace.reset",
            "workspace.import",
            "backup.import",
            "grid.delete",
            "folder.delete",
            "card.delete",
            "calendar.delete",
            "calendar.link.delete",
            "update.check",
            "update.download",
            "update.install",
            "ai.complete",
            "ai.validateKey",
            "ai.models.local",
            "web.fetch",
            "web.dom",
            "web.screenshot",
        ];
        let (_dir, mut core) = temp_core();
        core.set_viewport(800.0, 600.0);
        let grid_id = core.ws.active_grid_id.clone().unwrap();
        let card_id = core.create_card("note", 10.0, 10.0).unwrap().id;
        let folder_id = core.create_folder("smoke").id;
        let calendar_id = core.add_calendar("Smoke", "#d98e5c").id;
        let link_id = core
            .add_date_link(
                vec![calendar_id.clone()],
                "2026-11-01",
                "smoke reminder",
                None,
                None,
            )
            .unwrap()
            .id;
        let list = tools();
        let mut checked = 0;
        for tool in &list {
            let name = tool["name"].as_str().unwrap();
            if skip.contains(&name) {
                continue;
            }
            let args = generated_args(tool, &grid_id, &card_id, &folder_id, &calendar_id, &link_id);
            let response = handle(
                &mut core,
                json!({
                    "jsonrpc":"2.0","id":7,"method":"tools/call",
                    "params":{"name":name,"arguments":args}
                }),
            )
            .unwrap();
            assert!(response.get("result").is_some(), "no result for {name}");
            if response["result"].get("isError").and_then(|v| v.as_bool()) == Some(true) {
                let text = response["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap_or("");
                assert!(!text.contains("unknown tool"), "{name} is not wired");
            }
            checked += 1;
        }
        assert!(checked > 80, "only {checked} tools exercised");
    }
    fn tool_call(core: &mut Core, name: &str, args: Value) -> Value {
        handle(
            core,
            json!({
                "jsonrpc":"2.0","id":8,"method":"tools/call",
                "params":{"name":name,"arguments":args}
            }),
        )
        .unwrap()
    }

    #[test]
    fn calendar_links_resolve_source_titles_and_filters() {
        let (_dir, mut core) = temp_core();
        let calendar = core.add_calendar("Work", "#d98e5c");
        let card = core.create_card("checklist", 0.0, 0.0).unwrap();
        core.update_card_json(
            &json!({
                "id": card.id,
                "payload": {"items": [{"id": "i1", "text": "Ship it", "done": false}]}
            })
            .to_string(),
        );
        core.add_date_link(
            vec![calendar.id.clone()],
            "2026-11-05",
            "",
            Some(card.id.clone()),
            Some("i1".into()),
        )
        .unwrap();
        let agenda = core.calendar_agenda("2026-11-01", "2026-11-30", &[]);
        assert_eq!(agenda.len(), 1);
        assert_eq!(agenda[0]["title"], "Ship it");
        let filtered = core.calendar_agenda("2026-11-01", "2026-11-30", &["other".into()]);
        assert!(filtered.is_empty());
    }

    #[test]
    fn workspace_reads_redact_secrets() {
        let (_dir, mut core) = temp_core();
        core.ws
            .settings
            .ai_api_keys
            .insert("openai".into(), "sk-super-secret".into());
        core.ws.settings.gdrive_client_secret = Some("gdrive-secret".into());
        let response = tool_call(&mut core, "workspace.get", json!({}));
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(!text.contains("sk-super-secret"));
        assert!(!text.contains("gdrive-secret"));
    }

    #[test]
    fn theme_tools_select_mode_and_slots() {
        let (_dir, mut core) = temp_core();
        let mode = tool_call(&mut core, "theme.mode.set", json!({"mode":"dark"}));
        assert!(mode["result"].get("isError").is_none());
        let light = tool_call(
            &mut core,
            "theme.set",
            json!({"themeId":"chalk","mode":"light"}),
        );
        assert!(light["result"].get("isError").is_none());
        let state = tool_call(&mut core, "theme.get", json!({}));
        let value: Value =
            serde_json::from_str(state["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(value["mode"], "dark");
        assert_eq!(value["light"], "chalk");
        assert_eq!(value["dark"], "corcho");
        assert_eq!(value["effective"], "corcho");
    }

    #[test]
    fn config_tools_roundtrip() {
        let (_dir, mut core) = temp_core();
        for (key, value) in [
            ("theme_mode", "light"),
            ("theme_light", "chalk"),
            ("theme_dark", "trellis"),
            ("font", "mono"),
        ] {
            let response = tool_call(
                &mut core,
                "settings.set",
                json!({"key": key, "value": value}),
            );
            assert!(
                response["result"].get("isError").is_none(),
                "{key}: {response}"
            );
        }
        for (name, args) in [
            (
                "effects.set",
                json!({"spawn": false, "parallax": true, "ripple": true, "reducedMotion": true}),
            ),
            ("audio.setVolume", json!({"volume": 17})),
        ] {
            let response = tool_call(&mut core, name, args);
            assert!(
                response["result"].get("isError").is_none(),
                "{name}: {response}"
            );
        }

        let state = tool_call(&mut core, "settings.get", json!({}));
        let value: Value =
            serde_json::from_str(state["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(value["theme_mode"], "light");
        assert_eq!(value["theme_light"], "chalk");
        assert_eq!(value["theme_dark"], "trellis");
        assert_eq!(value["font"], "mono");
        assert_eq!(value["spawn_animations"], false);
        assert_eq!(value["card_parallax"], true);
        assert_eq!(value["grid_ripple"], true);
        assert_eq!(value["reduced_motion"], true);
        assert_eq!(value["master_volume"], 17);

        let custom = tool_call(
            &mut core,
            "theme.custom.set",
            json!({
                "background": "#101010",
                "surface": "#202020",
                "accent": "#ff8800",
                "text": "#eeeeee"
            }),
        );
        assert!(custom["result"].get("isError").is_none(), "{custom}");
        let themes = tool_call(&mut core, "theme.get", json!({}));
        let value: Value =
            serde_json::from_str(themes["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(value["light"], "custom");
        assert_eq!(value["dark"], "custom");

        let batch = tool_call(
            &mut core,
            "settings.batch",
            json!({"patch": {"theme_mode": "system", "theme_light": "paper", "theme_dark": "corcho"}}),
        );
        assert!(batch["result"].get("isError").is_none(), "{batch}");
        let state = tool_call(&mut core, "settings.get", json!({}));
        let value: Value =
            serde_json::from_str(state["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(value["theme_mode"], "system");
        assert_eq!(value["theme_light"], "paper");
        assert_eq!(value["theme_dark"], "corcho");

        let bad = tool_call(&mut core, "theme.set", json!({"themeId": "not-a-theme"}));
        assert_eq!(bad["result"]["isError"], true);
        let bad_key = tool_call(
            &mut core,
            "settings.set",
            json!({"key": "not_a_setting", "value": 1}),
        );
        assert_eq!(bad_key["result"]["isError"], true);
    }

    #[test]
    fn file_pins_accept_only_supported_types() {
        let (dir, mut core) = temp_core();
        let markdown = dir.path().join("pinned.md");
        std::fs::write(&markdown, "# Pinned").unwrap();
        let ok = tool_call(
            &mut core,
            "file.pin",
            json!({"path": markdown.to_string_lossy(), "x": 10, "y": 20}),
        );
        assert!(ok["result"].get("isError").is_none(), "{ok}");
        let card: Value =
            serde_json::from_str(ok["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(card["type"], "file");
        assert_eq!(card["fileKind"], "markdown");

        let text = dir.path().join("nope.txt");
        std::fs::write(&text, "no").unwrap();
        let bad = tool_call(
            &mut core,
            "file.pin",
            json!({"path": text.to_string_lossy()}),
        );
        assert_eq!(bad["result"]["isError"], true);
    }

    #[test]
    fn css_theme_lifecycle() {
        let (_dir, mut core) = temp_core();
        let template = tool_call(&mut core, "theme.css.template", json!({}));
        let css = template["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .replace("--theme-id: my-theme;", "--theme-id: css-test;")
            .replace("--theme-name: My Theme;", "--theme-name: CSS Test;");
        let created = tool_call(
            &mut core,
            "theme.css.set",
            json!({"themeId": "css-test", "css": css}),
        );
        assert!(created["result"].get("isError").is_none(), "{created}");
        let listed = tool_call(&mut core, "theme.list", json!({}));
        let themes: Value =
            serde_json::from_str(listed["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert!(themes
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["id"] == "css-test"));
        let set = tool_call(
            &mut core,
            "theme.set",
            json!({"themeId": "css-test", "mode": "light"}),
        );
        assert!(set["result"].get("isError").is_none(), "{set}");
        let read = tool_call(&mut core, "theme.css.get", json!({"themeId": "css-test"}));
        assert!(read["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("--theme-id: css-test"));
        let deleted = tool_call(
            &mut core,
            "theme.css.delete",
            json!({"themeId": "css-test"}),
        );
        assert!(deleted["result"].get("isError").is_none(), "{deleted}");
    }

    #[test]
    fn hardening_rejects_invalid_input() {
        let (_dir, mut core) = temp_core();
        let card = core.create_card("note", 0.0, 0.0).unwrap();

        let bad_kind = tool_call(&mut core, "card.create", json!({"kind":"bogus"}));
        assert_eq!(bad_kind["result"]["isError"], true);

        let bad_color = tool_call(
            &mut core,
            "card.setColor",
            json!({"cardId": card.id, "color": "red"}),
        );
        assert_eq!(bad_color["result"]["isError"], true);

        let bad_url = tool_call(
            &mut core,
            "web.navigate",
            json!({"cardId": card.id, "url": "file:///etc/passwd"}),
        );
        assert_eq!(bad_url["result"]["isError"], true);

        let too_large = tool_call(
            &mut core,
            "card.resize",
            json!({"cardId": card.id, "width": 999999}),
        );
        assert_eq!(too_large["result"]["isError"], true);

        let script = tool_call(&mut core, "script.run", json!({"script":"true"}));
        assert_eq!(script["result"]["isError"], true);
        let text = script["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("disabled"));
    }
}
