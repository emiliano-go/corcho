mod ai;
mod audio;
mod bridge;
mod clipboard;
mod core;
mod deeplink;
mod gitbackup;
mod gitcard;
mod history;
mod integrations;
mod irohsync;
mod logging;
mod mcp;
mod model;
mod pairing;
mod search;
mod single_instance;
mod storage;
mod svg;
mod sync;
mod theme;
mod updates;

use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QUrl};

fn append_chromium_flags(flags: &str) {
    let key = "QTWEBENGINE_CHROMIUM_FLAGS";
    let existing = std::env::var(key).unwrap_or_default();
    let mut added: Vec<&str> = Vec::new();
    for flag in flags.split_whitespace() {
        let name = flag.split('=').next().unwrap_or(flag);
        if !existing.split_whitespace().any(|f| f.starts_with(name)) {
            added.push(flag);
        }
    }
    if added.is_empty() {
        return;
    }
    let joined = added.join(" ");
    let value = if existing.is_empty() {
        joined
    } else {
        format!("{existing} {joined}")
    };
    std::env::set_var(key, value);
}

fn main() {
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        std::env::set_var("RUST_BACKTRACE", "1");
    }
    if std::env::var_os("MALLOC_ARENA_MAX").is_none() {
        std::env::set_var("MALLOC_ARENA_MAX", "2");
    }
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--print-endpoint-id") {
        let data_dir = args
            .iter()
            .position(|a| a == "--data-dir")
            .and_then(|i| args.get(i + 1))
            .map(std::path::PathBuf::from)
            .unwrap_or_else(storage::default_data_dir);
        match irohsync::endpoint_id(&data_dir) {
            Ok(id) => println!("{id}"),
            Err(err) => {
                eprintln!("corcho: {err}");
                std::process::exit(1);
            }
        }
        return;
    }
    let mcp_mode = args.iter().any(|a| a == "--mcp" || a == "--mcp-http")
        || std::env::var("CORCHO_MCP").as_deref() == Ok("1");
    if !mcp_mode {
        let low_memory = storage::load_workspace(&storage::default_data_dir())
            .map(|ws| ws.settings.low_memory_mode)
            .unwrap_or(false);
        if low_memory {
            append_chromium_flags(
                "--renderer-process-limit=2 --process-per-site --js-flags=--max-old-space-size=256 --aggressive-cache-discard",
            );
        }
    }
    let (log_path, crash_path) = logging::init(!mcp_mode);
    unsafe {
        extern "C" {
            fn corcho_install_log_handler();
        }
        corcho_install_log_handler();
    }
    if args.iter().any(|a| a == "--crash-test") {
        eprintln!("[corcho] crash test armed; log {log_path:?} crash {crash_path:?}");
        logging::arm_crash_test();
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("corcho=info")),
        )
        .init();

    let http_index = args.iter().position(|a| a == "--mcp-http");
    if let Some(index) = http_index {
        let addr = args
            .get(index + 1)
            .cloned()
            .unwrap_or_else(|| "127.0.0.1:7411".to_string());
        if let Err(err) = mcp::serve_http(&addr) {
            eprintln!("mcp http: {err}");
            std::process::exit(1);
        }
        return;
    }
    if args.iter().any(|a| a == "--mcp") || std::env::var("CORCHO_MCP").as_deref() == Ok("1") {
        if let Err(err) = mcp::serve() {
            eprintln!("mcp: {err}");
            std::process::exit(1);
        }
        return;
    }
    if let Some(index) = args.iter().position(|a| a == "--sync-daemon") {
        let mut ticket = String::new();
        let mut data_dir = storage::default_data_dir();
        let mut ttl_hours = 0u64;
        let mut i = index + 1;
        while i < args.len() {
            match args[i].as_str() {
                "--ticket" => {
                    if let Some(value) = args.get(i + 1) {
                        ticket = value.clone();
                    }
                    i += 2;
                }
                "--data-dir" => {
                    if let Some(value) = args.get(i + 1) {
                        data_dir = std::path::PathBuf::from(value);
                    }
                    i += 2;
                }
                "--ttl-hours" => {
                    if let Some(value) = args.get(i + 1) {
                        ttl_hours = value.parse().unwrap_or(0);
                    }
                    i += 2;
                }
                other if !other.starts_with("--") => {
                    ticket = other.to_string();
                    i += 1;
                }
                _ => i += 1,
            }
        }
        if ticket.trim().is_empty() {
            eprintln!("corcho sync daemon: --ticket <read-ticket> is required");
            std::process::exit(2);
        }
        if let Err(err) = irohsync::run_daemon(data_dir, ticket, ttl_hours) {
            eprintln!("corcho sync daemon: {err}");
            std::process::exit(1);
        }
        return;
    }
    if single_instance::forward_if_running(&args) {
        return;
    }

    if std::env::var_os("QT_QPA_PLATFORMTHEME").is_none() {
        std::env::set_var("QT_QPA_PLATFORMTHEME", "xdgdesktopportal");
    }

    let mut app = QGuiApplication::new();
    unsafe {
        extern "C" {
            fn corcho_set_app_identity();
        }
        corcho_set_app_identity();
    }
    let mut engine = QQmlApplicationEngine::new();

    if let Some(engine) = engine.as_mut() {
        engine.load(&QUrl::from("qrc:/qt/qml/Corcho/qml/Main.qml"));
    }

    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(5));
        if !deeplink::is_registered() {
            eprintln!("[corcho] QML window did not initialize; exiting");
            std::process::exit(1);
        }
    });

    if let Some(app) = app.as_mut() {
        app.exec();
    }
}
