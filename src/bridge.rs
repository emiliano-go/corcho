use core::pin::Pin;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use base64::Engine;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;

use crate::ai::{self, AIRequest};
use crate::core::Core;
use crate::integrations;
use crate::model;
use crate::sync;
use crate::theme;
use crate::updates;

static THEMES_DIRTY: AtomicBool = AtomicBool::new(true);
static MCP_HTTP_STARTED: AtomicBool = AtomicBool::new(false);
static SNAPSHOT_STARTED: AtomicBool = AtomicBool::new(false);

fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(4)
            .build()
            .expect("tokio runtime")
    })
}

fn s(v: impl Into<String>) -> QString {
    QString::from(v.into())
}

fn p(v: &QString) -> String {
    v.to_string()
}

#[derive(serde::Deserialize)]
struct FloatingWindowPlacement {
    token: String,
    x: f64,
    y: f64,
}

/// Wayland compositors decide where toplevel windows go. Sway exposes its IPC
/// through swaymsg and can move floating windows, so use it to keep the
/// detached cards in the requested row. Other sessions simply keep the
/// client-side positions.
fn place_sway_floating_windows(json: &str) {
    if std::env::var_os("SWAYSOCK").is_none() {
        return;
    }
    let Ok(requests) = serde_json::from_str::<Vec<FloatingWindowPlacement>>(json) else {
        return;
    };
    let mut commands = Vec::new();
    for request in requests {
        let token = request.token.trim();
        if token.is_empty()
            || !token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            continue;
        }
        commands.push(format!(
            "[title=\"^Picture-in-Picture #{token} \"] floating enable, move absolute position {} {}",
            request.x.round() as i64,
            request.y.round() as i64
        ));
    }
    if commands.is_empty() {
        return;
    }
    let _ = std::process::Command::new("swaymsg")
        .arg(commands.join("; "))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(QString, grids_json, cxx_name = "gridsJson")]
        #[qproperty(QString, cards_json, cxx_name = "cardsJson")]
        #[qproperty(QString, all_cards_json, cxx_name = "allCardsJson")]
        #[qproperty(QString, folders_json, cxx_name = "foldersJson")]
        #[qproperty(QString, calendar_json, cxx_name = "calendarJson")]
        #[qproperty(QString, settings_json, cxx_name = "settingsJson")]
        #[qproperty(QString, active_grid_id, cxx_name = "activeGridId")]
        #[qproperty(f64, pan_x, cxx_name = "panX")]
        #[qproperty(f64, pan_y, cxx_name = "panY")]
        #[qproperty(f64, zoom, cxx_name = "zoom")]
        #[qproperty(f64, viewport_width, cxx_name = "viewportWidth")]
        #[qproperty(f64, viewport_height, cxx_name = "viewportHeight")]
        #[qproperty(QString, status_text, cxx_name = "statusText")]
        #[qproperty(bool, syncing, cxx_name = "syncing")]
        #[qproperty(bool, ai_busy, cxx_name = "aiBusy")]
        #[qproperty(QString, update_json, cxx_name = "updateJson")]
        #[qproperty(QString, iroh_json, cxx_name = "irohJson")]
        #[qproperty(QString, search_json, cxx_name = "searchJson")]
        #[qproperty(QString, themes_json, cxx_name = "themesJson")]
        #[qproperty(QString, fonts_json, cxx_name = "fontsJson")]
        #[qproperty(QString, models_json, cxx_name = "modelsJson")]
        #[qproperty(QString, version, cxx_name = "version")]
        type AppController = super::AppControllerRust;

        #[qsignal]
        #[cxx_name = "toast"]
        fn toast(self: Pin<&mut Self>, message: QString);

        #[qsignal]
        #[cxx_name = "aiChunk"]
        fn ai_chunk(self: Pin<&mut Self>, id: QString, chunk: QString);

        #[qsignal]
        #[cxx_name = "aiDone"]
        fn ai_done(self: Pin<&mut Self>, id: QString, full: QString);

        #[qsignal]
        #[cxx_name = "aiError"]
        fn ai_error(self: Pin<&mut Self>, id: QString, message: QString);

        #[qsignal]
        #[cxx_name = "gitCardUpdated"]
        fn git_card_updated(self: Pin<&mut Self>, card_id: QString);

        #[qsignal]
        #[cxx_name = "notice"]
        fn notice(self: Pin<&mut Self>, kind: QString, message: QString);

        #[qsignal]
        #[cxx_name = "aiKeyResult"]
        fn ai_key_result(self: Pin<&mut Self>, provider: QString, success: bool, message: QString);

        #[qsignal]
        #[cxx_name = "aiModelsResult"]
        fn ai_models_result(self: Pin<&mut Self>, json: QString);

        #[qinvokable]
        fn boot(self: Pin<&mut Self>);

        #[qinvokable]
        #[cxx_name = "handleDeepLink"]
        fn handle_deep_link(self: Pin<&mut Self>, url: &QString);

        #[qinvokable]
        #[cxx_name = "refresh"]
        fn refresh(self: Pin<&mut Self>);

        #[qinvokable]
        #[cxx_name = "reloadIfChanged"]
        fn reload_if_changed(self: Pin<&mut Self>) -> bool;

        #[qinvokable]
        #[cxx_name = "saveNow"]
        fn save_now(self: Pin<&mut Self>);

        #[qinvokable]
        #[cxx_name = "createCard"]
        fn create_card(self: Pin<&mut Self>, kind: &QString, x: f64, y: f64) -> QString;

        #[qinvokable]
        #[cxx_name = "pasteClipboard"]
        fn paste_clipboard(self: Pin<&mut Self>, x: f64, y: f64) -> QString;

        #[qinvokable]
        #[cxx_name = "copyWhiteboardSvg"]
        fn copy_whiteboard_svg(
            self: Pin<&mut Self>,
            card_id: &QString,
            width: f64,
            height: f64,
        ) -> bool;

        #[qinvokable]
        #[cxx_name = "copyWhiteboardStrokes"]
        fn copy_whiteboard_strokes(self: Pin<&mut Self>, card_id: &QString) -> bool;

        #[qinvokable]
        #[cxx_name = "updateCard"]
        fn update_card(self: Pin<&mut Self>, json: &QString) -> bool;

        #[qinvokable]
        #[cxx_name = "createCalendar"]
        fn create_calendar(self: Pin<&mut Self>, name: &QString, color: &QString) -> QString;

        #[qinvokable]
        #[cxx_name = "updateCalendar"]
        fn update_calendar(
            self: Pin<&mut Self>,
            id: &QString,
            name: &QString,
            color: &QString,
        ) -> bool;

        #[qinvokable]
        #[cxx_name = "deleteCalendar"]
        fn delete_calendar(self: Pin<&mut Self>, id: &QString) -> bool;

        #[qinvokable]
        #[cxx_name = "addDateLink"]
        fn add_date_link(
            self: Pin<&mut Self>,
            source_card_id: &QString,
            source_item_id: &QString,
            date: &QString,
            calendar_ids_json: &QString,
            title: &QString,
        ) -> QString;

        #[qinvokable]
        #[cxx_name = "updateDateLink"]
        fn update_date_link(
            self: Pin<&mut Self>,
            link_id: &QString,
            date: &QString,
            calendar_ids_json: &QString,
            title: &QString,
        ) -> bool;

        #[qinvokable]
        #[cxx_name = "deleteDateLink"]
        fn delete_date_link(self: Pin<&mut Self>, link_id: &QString) -> bool;

        #[qinvokable]
        #[cxx_name = "deleteCard"]
        fn delete_card(self: Pin<&mut Self>, id: &QString) -> bool;

        #[qinvokable]
        #[cxx_name = "duplicateCard"]
        fn duplicate_card(self: Pin<&mut Self>, id: &QString) -> QString;

        #[qinvokable]
        #[cxx_name = "sendCardToGrid"]
        fn send_card_to_grid(self: Pin<&mut Self>, card_id: &QString, grid_id: &QString) -> bool;

        #[qinvokable]
        #[cxx_name = "moveCards"]
        fn move_cards(self: Pin<&mut Self>, ids_json: &QString, dx: f64, dy: f64) -> bool;

        #[qinvokable]
        #[cxx_name = "placeFloatingWindows"]
        fn place_floating_windows(self: Pin<&mut Self>, json: &QString);

        #[qinvokable]
        #[cxx_name = "setCardGroup"]
        fn set_card_group(self: Pin<&mut Self>, ids_json: &QString, group: &QString) -> bool;

        #[qinvokable]
        #[qinvokable]
        #[cxx_name = "syncCards"]
        fn sync_cards(self: Pin<&mut Self>, source_id: &QString, target_id: &QString) -> bool;

        #[qinvokable]
        #[cxx_name = "unsyncCard"]
        fn unsync_card(self: Pin<&mut Self>, card_id: &QString) -> bool;

        #[qinvokable]
        #[cxx_name = "toggleAiLink"]
        fn toggle_ai_link(self: Pin<&mut Self>, ai_id: &QString, card_id: &QString) -> bool;

        #[qinvokable]
        #[cxx_name = "aiContext"]
        fn ai_context(&self, ids_json: &QString) -> QString;

        #[qinvokable]
        #[cxx_name = "gitCardRefresh"]
        fn git_card_refresh(self: Pin<&mut Self>, card_id: &QString);

        #[qinvokable]
        #[cxx_name = "createTemplate"]
        fn create_template(self: Pin<&mut Self>, name: &QString, ids_json: &QString) -> QString;

        #[qinvokable]
        #[cxx_name = "insertTemplate"]
        fn insert_template(self: Pin<&mut Self>, id: &QString, x: f64, y: f64) -> QString;

        #[qinvokable]
        #[cxx_name = "deleteTemplate"]
        fn delete_template(self: Pin<&mut Self>, id: &QString) -> bool;

        #[qinvokable]
        #[cxx_name = "templatesJson"]
        fn templates_json(self: &Self) -> QString;

        #[qinvokable]
        #[cxx_name = "kanbanAggregateJson"]
        fn kanban_aggregate_json(self: &Self) -> QString;

        #[qinvokable]
        #[cxx_name = "createGrid"]
        fn create_grid(self: Pin<&mut Self>, name: &QString) -> QString;

        #[qinvokable]
        #[cxx_name = "renameGrid"]
        fn rename_grid(self: Pin<&mut Self>, id: &QString, name: &QString) -> bool;

        #[qinvokable]
        #[cxx_name = "setGridIcon"]
        fn set_grid_icon(self: Pin<&mut Self>, id: &QString, icon: &QString) -> bool;

        #[qinvokable]
        #[cxx_name = "deleteGrid"]
        fn delete_grid(self: Pin<&mut Self>, id: &QString) -> bool;

        #[qinvokable]
        #[cxx_name = "switchGrid"]
        fn switch_grid(self: Pin<&mut Self>, id: &QString) -> bool;

        #[qinvokable]
        #[cxx_name = "createFolder"]
        fn create_folder(self: Pin<&mut Self>, name: &QString) -> QString;

        #[qinvokable]
        #[cxx_name = "renameFolder"]
        fn rename_folder(self: Pin<&mut Self>, id: &QString, name: &QString) -> bool;

        #[qinvokable]
        #[cxx_name = "deleteFolder"]
        fn delete_folder(self: Pin<&mut Self>, id: &QString, keep_content: bool) -> bool;

        #[qinvokable]
        #[cxx_name = "toggleFolder"]
        fn toggle_folder(self: Pin<&mut Self>, id: &QString);

        #[qinvokable]
        #[cxx_name = "moveGridToFolder"]
        fn move_grid_to_folder(
            self: Pin<&mut Self>,
            grid_id: &QString,
            folder_id: &QString,
        ) -> bool;

        #[qinvokable]
        #[cxx_name = "reorderGrid"]
        fn reorder_grid(self: Pin<&mut Self>, id: &QString, before: &QString) -> bool;

        #[qinvokable]
        #[cxx_name = "setSetting"]
        fn set_setting(self: Pin<&mut Self>, key: &QString, value_json: &QString) -> bool;

        #[qinvokable]
        #[cxx_name = "setView"]
        fn set_view(self: Pin<&mut Self>, pan_x: f64, pan_y: f64, zoom: f64);

        #[qinvokable]
        #[cxx_name = "setViewport"]
        fn set_viewport(self: Pin<&mut Self>, width: f64, height: f64);

        #[qinvokable]
        fn undo(self: Pin<&mut Self>) -> bool;

        #[qinvokable]
        fn redo(self: Pin<&mut Self>) -> bool;

        #[qinvokable]
        #[cxx_name = "search"]
        fn search_now(self: &Self, query: &QString) -> QString;

        #[qinvokable]
        #[cxx_name = "command"]
        fn command(self: Pin<&mut Self>, id: &QString) -> QString;

        #[qinvokable]
        #[cxx_name = "runScript"]
        fn run_script(self: &Self, script: &QString) -> QString;

        #[qinvokable]
        #[cxx_name = "mprisPlayers"]
        fn mpris_players(self: &Self) -> QString;

        #[qinvokable]
        #[cxx_name = "mprisAction"]
        fn mpris_action(self: &Self, player: &QString, action: &QString) -> QString;

        #[qinvokable]
        #[cxx_name = "aiSend"]
        fn ai_send(
            self: Pin<&mut Self>,
            provider: &QString,
            model_name: &QString,
            prompt: &QString,
            context: &QString,
        ) -> QString;

        #[qinvokable]
        #[cxx_name = "aiValidate"]
        fn ai_validate(self: Pin<&mut Self>, provider: &QString, key: &QString) -> QString;

        #[qinvokable]
        #[cxx_name = "aiFetchModels"]
        fn ai_fetch_models(self: Pin<&mut Self>, server_url: &QString, key: &QString) -> QString;

        #[qinvokable]
        #[cxx_name = "syncNow"]
        fn sync_now(self: Pin<&mut Self>);

        #[qinvokable]
        #[cxx_name = "irohCreate"]
        fn iroh_create(self: Pin<&mut Self>) -> QString;

        #[qinvokable]
        #[cxx_name = "irohJoin"]
        fn iroh_join(self: Pin<&mut Self>, ticket: &QString) -> QString;

        #[qinvokable]
        #[cxx_name = "irohShare"]
        fn iroh_share(self: Pin<&mut Self>, read_only: bool) -> QString;

        #[qinvokable]
        #[cxx_name = "irohSyncNow"]
        fn iroh_sync_now(self: Pin<&mut Self>);

        #[qinvokable]
        #[cxx_name = "irohRelayUrl"]
        fn iroh_relay_url(self: &Self) -> QString;

        #[qinvokable]
        #[cxx_name = "irohSetRelay"]
        fn iroh_set_relay(self: Pin<&mut Self>, url: &QString) -> QString;

        #[qinvokable]
        #[cxx_name = "irohCacheTicket"]
        fn iroh_cache_ticket(self: Pin<&mut Self>) -> QString;

        #[qinvokable]
        #[cxx_name = "irohPairCreate"]
        fn iroh_pair_create(self: Pin<&mut Self>) -> QString;

        #[qinvokable]
        #[cxx_name = "irohPairJoin"]
        fn iroh_pair_join(self: Pin<&mut Self>, code: &QString) -> QString;

        #[qinvokable]
        #[cxx_name = "irohLeave"]
        fn iroh_leave(self: Pin<&mut Self>) -> QString;

        #[qinvokable]
        #[cxx_name = "copyText"]
        fn copy_text(self: Pin<&mut Self>, text: &QString);

        #[qinvokable]
        #[cxx_name = "gdriveConnect"]
        fn gdrive_connect(self: Pin<&mut Self>) -> QString;

        #[qinvokable]
        #[cxx_name = "gdriveDisconnect"]
        fn gdrive_disconnect(self: Pin<&mut Self>);

        #[qinvokable]
        #[cxx_name = "exportBackup"]
        fn export_backup(self: Pin<&mut Self>, path: &QString) -> bool;

        #[qinvokable]
        #[cxx_name = "importBackup"]
        fn import_backup(self: Pin<&mut Self>, path: &QString) -> bool;

        #[qinvokable]
        #[cxx_name = "importLegacy"]
        fn import_legacy(self: Pin<&mut Self>, path: &QString) -> bool;

        #[qinvokable]
        #[cxx_name = "setDataDir"]
        fn set_data_dir(self: Pin<&mut Self>, path: &QString) -> bool;

        #[qinvokable]
        #[cxx_name = "checkUpdates"]
        fn check_updates(self: Pin<&mut Self>);

        #[qinvokable]
        #[cxx_name = "downloadUpdate"]
        fn download_update(self: Pin<&mut Self>);

        #[qinvokable]
        #[cxx_name = "installUpdate"]
        fn install_update(self: &Self) -> bool;

        #[qinvokable]
        #[cxx_name = "readAttachment"]
        fn read_attachment(self: &Self, name: &QString) -> QString;

        #[qinvokable]
        #[cxx_name = "writeAttachment"]
        fn write_attachment(self: &Self, name: &QString, base64_data: &QString) -> QString;

        #[qinvokable]
        #[cxx_name = "listAttachments"]
        fn list_attachments(self: &Self) -> QString;

        #[qinvokable]
        #[cxx_name = "openExternal"]
        fn open_external(self: &Self, url: &QString);

        #[qinvokable]
        #[cxx_name = "pinPath"]
        fn pin_path(
            self: Pin<&mut Self>,
            path: &QString,
            x: f64,
            y: f64,
            grid_id: &QString,
        ) -> QString;

        #[qinvokable]
        #[cxx_name = "readPinnedFile"]
        fn read_pinned_file(self: &Self, path: &QString) -> QString;

        #[qinvokable]
        #[cxx_name = "listPinnedFolder"]
        fn list_pinned_folder(self: &Self, path: &QString) -> QString;

        #[qinvokable]
        #[cxx_name = "openPath"]
        fn open_path(self: &Self, path: &QString);

        #[qinvokable]
        #[cxx_name = "openThemesFolder"]
        fn open_themes_folder(self: &Self);

        #[qinvokable]
        #[cxx_name = "createThemeFromTemplate"]
        fn create_theme_from_template(self: Pin<&mut Self>) -> QString;

        #[qinvokable]
        #[cxx_name = "setLaunchAtStartup"]
        fn set_launch_at_startup(self: Pin<&mut Self>, enabled: bool) -> bool;

        #[qinvokable]
        #[cxx_name = "resetTutorials"]
        fn reset_tutorials(self: Pin<&mut Self>);

        #[qinvokable]
        #[cxx_name = "timbreStart"]
        fn timbre_start(self: &Self, card_id: &QString, volume: f64, tone_freq: f64, tone_mix: f64);

        #[qinvokable]
        #[cxx_name = "timbreStop"]
        fn timbre_stop(self: &Self, card_id: &QString);

        #[qinvokable]
        #[cxx_name = "pomodoroChime"]
        fn pomodoro_chime(self: &Self);

        #[qinvokable]
        #[cxx_name = "adblockCheck"]
        fn adblock_check(self: &Self, url: &QString) -> bool;

        #[qinvokable]
        #[cxx_name = "adblockHosts"]
        fn adblock_hosts(self: &Self) -> QString;

        #[qinvokable]
        #[cxx_name = "changelog"]
        fn changelog(self: &Self) -> QString;

        #[qinvokable]
        fn quit(self: &Self);
    }

    impl cxx_qt::Threading for AppController {}

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(QString, tokens_json, cxx_name = "tokensJson")]
        #[qproperty(QString, mode, cxx_name = "mode")]
        #[qproperty(QString, light_theme, cxx_name = "lightTheme")]
        #[qproperty(QString, dark_theme, cxx_name = "darkTheme")]
        #[qproperty(QString, active_theme, cxx_name = "activeTheme")]
        #[qproperty(bool, system_dark, cxx_name = "systemDark")]
        type AppearanceController = super::AppearanceControllerRust;

        #[qinvokable]
        #[cxx_name = "refresh"]
        fn refresh(
            self: Pin<&mut Self>,
            settings_json: &QString,
            themes_json: &QString,
            fonts_json: &QString,
            system_dark: bool,
        );
    }
}

pub struct AppearanceControllerRust {
    tokens_json: QString,
    mode: QString,
    light_theme: QString,
    dark_theme: QString,
    active_theme: QString,
    system_dark: bool,
}

impl Default for AppearanceControllerRust {
    fn default() -> Self {
        let tokens = theme::builtin_entries()
            .into_iter()
            .find(|t| t.id == "corcho")
            .map(|t| t.tokens)
            .unwrap_or_else(theme::default_tokens);
        Self {
            tokens_json: serde_json::to_string(&tokens)
                .unwrap_or_else(|_| "{}".into())
                .into(),
            mode: "system".into(),
            light_theme: "paper".into(),
            dark_theme: "corcho".into(),
            active_theme: "corcho".into(),
            system_dark: true,
        }
    }
}

impl qobject::AppearanceController {
    pub fn refresh(
        mut self: Pin<&mut Self>,
        settings_json: &QString,
        themes_json: &QString,
        fonts_json: &QString,
        system_dark: bool,
    ) {
        let settings: serde_json::Value =
            serde_json::from_str(&p(settings_json)).unwrap_or_else(|_| serde_json::json!({}));
        let themes: serde_json::Value =
            serde_json::from_str(&p(themes_json)).unwrap_or_else(|_| serde_json::json!([]));
        let fonts: serde_json::Value =
            serde_json::from_str(&p(fonts_json)).unwrap_or_else(|_| serde_json::json!([]));
        let mode = settings["theme_mode"]
            .as_str()
            .unwrap_or("system")
            .to_string();
        let light = settings["theme_light"]
            .as_str()
            .unwrap_or("paper")
            .to_string();
        let dark = settings["theme_dark"]
            .as_str()
            .unwrap_or("corcho")
            .to_string();
        let use_dark = mode == "dark" || (mode == "system" && system_dark);
        let wanted = if use_dark {
            dark.clone()
        } else {
            light.clone()
        };
        let mut tokens = if wanted == "custom" {
            let custom = &settings["customTheme"];
            theme::build_tokens(
                custom["background"].as_str().unwrap_or("#121214"),
                custom["surface"].as_str().unwrap_or("#1b1b1f"),
                custom["accent"].as_str().unwrap_or("#c9a86a"),
                custom["text"].as_str().unwrap_or("#e8e8ea"),
                !use_dark,
                "Inter",
            )
        } else {
            themes
                .as_array()
                .and_then(|items| {
                    items
                        .iter()
                        .find(|t| t["id"].as_str() == Some(wanted.as_str()))
                })
                .and_then(|t| t.get("tokens"))
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_else(theme::default_tokens)
        };
        let font_id = settings["font"].as_str().unwrap_or("inter");
        if let Some(family) = fonts
            .as_array()
            .and_then(|items| items.iter().find(|f| f["id"].as_str() == Some(font_id)))
            .and_then(|f| f["family"].as_str())
            .filter(|family| !family.is_empty())
        {
            tokens.insert("fontFamily".into(), serde_json::json!(family));
        }
        self.as_mut().set_tokens_json(s(
            serde_json::to_string(&tokens).unwrap_or_else(|_| "{}".into())
        ));
        self.as_mut().set_mode(s(mode));
        self.as_mut().set_light_theme(s(light));
        self.as_mut().set_dark_theme(s(dark));
        self.as_mut().set_active_theme(s(wanted));
        self.as_mut().set_system_dark(system_dark);
    }
}

pub struct AppControllerRust {
    core: Arc<Mutex<Core>>,
    grids_json: QString,
    cards_json: QString,
    all_cards_json: QString,
    folders_json: QString,
    calendar_json: QString,
    settings_json: QString,
    active_grid_id: QString,
    pan_x: f64,
    pan_y: f64,
    zoom: f64,
    viewport_width: f64,
    viewport_height: f64,
    status_text: QString,
    syncing: bool,
    ai_busy: bool,
    update_json: QString,
    iroh_json: QString,
    search_json: QString,
    themes_json: QString,
    fonts_json: QString,
    models_json: QString,
    version: QString,
}

impl Default for AppControllerRust {
    fn default() -> Self {
        let core = Core::boot_gui().unwrap_or_else(|e| {
            tracing::error!("boot failed: {e}");
            Core::fallback()
        });
        let mut this = Self {
            core: Arc::new(Mutex::new(core)),
            grids_json: QString::default(),
            cards_json: QString::default(),
            all_cards_json: QString::from("[]"),
            folders_json: QString::default(),
            calendar_json: QString::from("{\"calendars\":[],\"dateLinks\":[]}"),
            settings_json: QString::default(),
            active_grid_id: QString::default(),
            pan_x: 0.0,
            pan_y: 0.0,
            zoom: 1.0,
            viewport_width: 0.0,
            viewport_height: 0.0,
            status_text: QString::default(),
            syncing: false,
            ai_busy: false,
            update_json: QString::default(),
            iroh_json: QString::from("{}"),
            search_json: QString::from("[]"),
            themes_json: model::themes().to_string().into(),
            fonts_json: model::fonts().to_string().into(),
            models_json: model::models_by_provider().to_string().into(),
            version: QString::from(env!("CARGO_PKG_VERSION")),
        };
        this.settings_json = QString::from("{}");
        this
    }
}

impl qobject::AppController {
    fn refresh_all(mut self: Pin<&mut Self>) {
        let themes_dirty = THEMES_DIRTY.swap(false, Ordering::Relaxed);
        let cached_themes = p(&self.as_ref().rust().themes_json);
        let (
            grids,
            cards,
            all_cards,
            folders,
            calendar,
            settings,
            themes,
            active,
            pan,
            pany,
            zoom,
            upd,
            git,
        ) = {
            let core_arc = self.as_ref().rust().core.clone();
            let core = core_arc.lock().unwrap();
            (
                core.grids_json(),
                core.cards_json(),
                core.all_cards_json(),
                core.folders_json(),
                core.calendar_json(),
                core.settings_json(),
                if themes_dirty {
                    core.themes_json()
                } else {
                    cached_themes
                },
                core.active_grid_id(),
                core.active_grid_view().0,
                core.active_grid_view().1,
                core.active_grid_view().2,
                serde_json::to_string(&core.update).unwrap_or_else(|_| "{}".into()),
                core.iroh.status_json(),
            )
        };
        macro_rules! set_str {
            ($get:ident, $set:ident, $value:expr) => {
                if p(&self.as_ref().rust().$get) != $value {
                    self.as_mut().$set(s($value));
                }
            };
        }
        set_str!(grids_json, set_grids_json, grids);
        set_str!(cards_json, set_cards_json, cards);
        set_str!(all_cards_json, set_all_cards_json, all_cards);
        set_str!(folders_json, set_folders_json, folders);
        set_str!(calendar_json, set_calendar_json, calendar);
        set_str!(settings_json, set_settings_json, settings);
        set_str!(themes_json, set_themes_json, themes);
        set_str!(active_grid_id, set_active_grid_id, active);
        if (self.as_ref().rust().pan_x - pan).abs() > f64::EPSILON {
            self.as_mut().set_pan_x(pan);
        }
        if (self.as_ref().rust().pan_y - pany).abs() > f64::EPSILON {
            self.as_mut().set_pan_y(pany);
        }
        if (self.as_ref().rust().zoom - zoom).abs() > f64::EPSILON {
            self.as_mut().set_zoom(zoom);
            let (vw, vh) = {
                let core_arc = self.as_ref().rust().core.clone();
                let core = core_arc.lock().unwrap();
                (core.ws.viewport_width, core.ws.viewport_height)
            };
            self.as_mut().set_viewport_width(vw);
            self.as_mut().set_viewport_height(vh);
        }
        set_str!(update_json, set_update_json, upd);
        set_str!(iroh_json, set_iroh_json, git);
    }

    pub fn boot(mut self: Pin<&mut Self>) {
        let qt = self.as_ref().qt_thread();
        crate::deeplink::register(qt.clone());
        // Give the sync engine a view of the live workspace and a way to
        // apply remote records without an outbound republish.
        {
            let core_arc = self.as_ref().rust().core.clone();
            let iroh = core_arc.lock().unwrap().iroh.clone();
            let snapshot_core = core_arc.clone();
            let apply_core = core_arc.clone();
            let apply_qt = qt.clone();
            iroh.attach(
                Arc::new(move || snapshot_core.try_lock().ok().map(|c| c.ws.clone())),
                Arc::new(move |records| {
                    let Ok(mut core) = apply_core.try_lock() else {
                        return None;
                    };
                    let changed = core.apply_remote(records);
                    if changed {
                        let qt = apply_qt.clone();
                        let _ = qt.queue(|mut q| q.as_mut().refresh());
                    }
                    Some(changed)
                }),
            );
        }
        // Independent local durability: a daily workspace snapshot, pruned to
        // the newest eight. This survives loss of the Iroh store or its peers.
        if !SNAPSHOT_STARTED.swap(true, Ordering::SeqCst) {
            let core_arc = self.as_ref().rust().core.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(300));
                let data_dir = { core_arc.lock().unwrap().data_dir.clone() };
                let backups = data_dir.join("backups");
                let _ = std::fs::create_dir_all(&backups);
                let stamp = time::OffsetDateTime::now_utc().unix_timestamp();
                let dest = backups.join(format!("snapshot-{stamp}.zip"));
                if crate::storage::export_zip(&data_dir, &dest).is_ok() {
                    prune_snapshots(&backups, 8);
                }
                std::thread::sleep(Duration::from_secs(86_400));
            });
        }
        // Expose this same Core over the Streamable HTTP MCP transport so an
        // agent shares the running process instead of a second copy.
        if !MCP_HTTP_STARTED.swap(true, Ordering::SeqCst) {
            let core_arc = self.as_ref().rust().core.clone();
            let qt_for_mcp = Arc::new(Mutex::new(qt.clone()));
            let on_change: crate::mcp::ChangeHook = Arc::new(move || {
                let qt = qt_for_mcp.lock().unwrap().clone();
                let _ = qt.queue(|mut q| q.as_mut().refresh());
            });
            let addr =
                std::env::var("CORCHO_MCP_HTTP").unwrap_or_else(|_| "127.0.0.1:7411".to_string());
            std::thread::spawn(move || {
                if let Err(err) = crate::mcp::serve_http_shared(&addr, core_arc, Some(on_change)) {
                    tracing::warn!("embedded mcp http unavailable: {err}");
                }
            });
        }
        self.as_mut().refresh_all();
        self.as_mut().set_status_text(s("Ready"));

        let data_dir = {
            let core_arc = self.as_ref().rust().core.clone();
            let core = core_arc.lock().unwrap();
            core.data_dir.clone()
        };
        let _ = std::fs::create_dir_all(&data_dir);
        std::thread::spawn(move || {
            use notify::Watcher;
            let (tx, rx) = std::sync::mpsc::channel();
            let mut watcher = match notify::recommended_watcher(move |event| {
                let _ = tx.send(event);
            }) {
                Ok(watcher) => watcher,
                Err(err) => {
                    tracing::warn!("workspace watcher: {err}");
                    return;
                }
            };
            if let Err(err) = watcher.watch(&data_dir, notify::RecursiveMode::Recursive) {
                tracing::warn!("workspace watcher: {err}");
                return;
            }
            while let Ok(event) = rx.recv() {
                let Ok(event) = event else { continue };
                let workspace = event
                    .paths
                    .iter()
                    .any(|p| p.file_name().and_then(|n| n.to_str()) == Some("workspace.json"));
                let theme_file = event
                    .paths
                    .iter()
                    .any(|p| p.extension().and_then(|e| e.to_str()) == Some("css"));
                if !workspace && !theme_file {
                    continue;
                }
                std::thread::sleep(Duration::from_millis(120));
                while rx.try_recv().is_ok() {}
                if theme_file {
                    THEMES_DIRTY.store(true, Ordering::Relaxed);
                    let _ = qt.queue(move |mut q| {
                        q.as_mut().refresh();
                    });
                } else {
                    let _ = qt.queue(move |mut q| {
                        q.as_mut().reload_if_changed();
                    });
                }
            }
        });
    }

    pub fn handle_deep_link(mut self: Pin<&mut Self>, url: &QString) {
        let raw = p(url);
        let target = raw.strip_prefix("corcho://").unwrap_or(&raw);
        let target = target.trim_end_matches('/');
        let mut parts = target.splitn(2, '/');
        let first = parts.next().unwrap_or("");
        let second = parts.next();
        let core_arc = self.as_ref().rust().core.clone();
        {
            let mut core = core_arc.lock().unwrap();
            match (first, second) {
                ("card", Some(id)) => {
                    if let Some((grid, _)) = core.ws.card(id) {
                        let grid_id = grid.id.clone();
                        core.switch_grid(&grid_id);
                    }
                }
                ("grid", Some(id)) => {
                    core.switch_grid(id);
                }
                _ if !first.is_empty() => {
                    core.switch_grid(first);
                }
                _ => {}
            }
        }
        self.as_mut().refresh_all();
        self.as_mut().toast(s(format!("Opened {raw}")));
    }

    pub fn refresh(mut self: Pin<&mut Self>) {
        self.as_mut().refresh_all();
    }

    pub fn reload_if_changed(mut self: Pin<&mut Self>) -> bool {
        let changed = self
            .as_ref()
            .rust()
            .core
            .clone()
            .lock()
            .map(|mut core| core.reload_if_changed().unwrap_or(false))
            .unwrap_or(false);
        if changed {
            self.as_mut().refresh_all();
        }
        changed
    }

    fn refresh_cards(mut self: Pin<&mut Self>) {
        let (cards, all_cards, calendar) = {
            let core_arc = self.as_ref().rust().core.clone();
            let core = core_arc.lock().unwrap();
            (
                core.cards_json(),
                core.all_cards_json(),
                core.calendar_json(),
            )
        };
        self.as_mut().set_cards_json(s(cards));
        if p(&self.as_ref().rust().all_cards_json) != all_cards {
            self.as_mut().set_all_cards_json(s(all_cards));
        }
        self.as_mut().set_calendar_json(s(calendar));
    }

    pub fn save_now(self: Pin<&mut Self>) {
        let core_arc = self.as_ref().rust().core.clone();
        let mut core = core_arc.lock().unwrap();
        core.save_quiet();
    }

    pub fn create_card(mut self: Pin<&mut Self>, kind: &QString, x: f64, y: f64) -> QString {
        let card = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.create_card(&p(kind), x, y)
        };
        self.as_mut().refresh_all();
        card.map(|c| s(serde_json::to_string(&c).unwrap_or_default()))
            .unwrap_or_default()
    }

    pub fn paste_clipboard(mut self: Pin<&mut Self>, x: f64, y: f64) -> QString {
        let content = crate::clipboard::read();
        let created = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            match content {
                crate::clipboard::ClipboardContent::Empty => None,
                crate::clipboard::ClipboardContent::Text(text) => {
                    if let Some(markup) = crate::svg::extract_svg(&text) {
                        Some(match core.create_svg_card(markup, x, y) {
                            Ok(card) => serde_json::to_string(&card).unwrap_or_default(),
                            Err(err) => {
                                tracing::warn!("paste svg: {err}");
                                format!("error:{err}")
                            }
                        })
                    } else {
                        core.create_text_card(&text, x, y)
                            .map(|card| serde_json::to_string(&card).unwrap_or_default())
                    }
                }
                crate::clipboard::ClipboardContent::Image { png, width, height } => {
                    Some(match core.create_image_card(&png, width, height, x, y) {
                        Ok(card) => serde_json::to_string(&card).unwrap_or_default(),
                        Err(err) => {
                            tracing::warn!("paste image: {err}");
                            format!("error:{err}")
                        }
                    })
                }
            }
        };
        match created {
            Some(result) => {
                self.as_mut().refresh_all();
                s(result)
            }
            None => s("empty"),
        }
    }

    pub fn copy_whiteboard_svg(
        mut self: Pin<&mut Self>,
        card_id: &QString,
        width: f64,
        height: f64,
    ) -> bool {
        let svg = {
            let core_arc = self.as_ref().rust().core.clone();
            let core = core_arc.lock().unwrap();
            core.whiteboard_svg(&p(card_id), width, height)
        };
        let copied = svg
            .as_deref()
            .map(crate::clipboard::write_text)
            .unwrap_or(false);
        if copied {
            self.as_mut().toast(s("Copied whiteboard as SVG"));
        }
        copied
    }

    pub fn copy_whiteboard_strokes(mut self: Pin<&mut Self>, card_id: &QString) -> bool {
        let strokes = {
            let core_arc = self.as_ref().rust().core.clone();
            let core = core_arc.lock().unwrap();
            core.whiteboard_strokes_json(&p(card_id))
        };
        let copied = strokes
            .as_deref()
            .map(crate::clipboard::write_text)
            .unwrap_or(false);
        if copied {
            self.as_mut().toast(s("Copied whiteboard strokes"));
        }
        copied
    }

    pub fn update_card(mut self: Pin<&mut Self>, json: &QString) -> bool {
        let ok = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.update_card_json(&p(json))
        };
        self.as_mut().refresh_cards();
        ok
    }

    pub fn create_calendar(mut self: Pin<&mut Self>, name: &QString, color: &QString) -> QString {
        let calendar = self
            .as_ref()
            .rust()
            .core
            .clone()
            .lock()
            .unwrap()
            .add_calendar(&p(name), &p(color));
        self.as_mut().refresh_all();
        s(serde_json::to_string(&calendar).unwrap_or_default())
    }

    pub fn update_calendar(
        mut self: Pin<&mut Self>,
        id: &QString,
        name: &QString,
        color: &QString,
    ) -> bool {
        let name = p(name);
        let color = p(color);
        let ok = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.update_calendar(
                &p(id),
                if name.is_empty() { None } else { Some(&name) },
                if color.is_empty() { None } else { Some(&color) },
            )
        };
        self.as_mut().refresh_all();
        ok
    }

    pub fn delete_calendar(mut self: Pin<&mut Self>, id: &QString) -> bool {
        let ok = self
            .as_ref()
            .rust()
            .core
            .clone()
            .lock()
            .unwrap()
            .delete_calendar(&p(id));
        self.as_mut().refresh_all();
        ok
    }

    pub fn add_date_link(
        mut self: Pin<&mut Self>,
        source_card_id: &QString,
        source_item_id: &QString,
        date: &QString,
        calendar_ids_json: &QString,
        title: &QString,
    ) -> QString {
        let ids: Vec<String> =
            serde_json::from_str(&p(calendar_ids_json)).unwrap_or_else(|_| Vec::new());
        let card = p(source_card_id);
        let item = p(source_item_id);
        let result = self
            .as_ref()
            .rust()
            .core
            .clone()
            .lock()
            .unwrap()
            .add_date_link(
                ids,
                &p(date),
                &p(title),
                if card.is_empty() { None } else { Some(card) },
                if item.is_empty() { None } else { Some(item) },
            );
        self.as_mut().refresh_all();
        match result {
            Ok(link) => s(link.id),
            Err(err) => s(format!("error:{err}")),
        }
    }

    pub fn update_date_link(
        mut self: Pin<&mut Self>,
        link_id: &QString,
        date: &QString,
        calendar_ids_json: &QString,
        title: &QString,
    ) -> bool {
        let ids: Vec<String> =
            serde_json::from_str(&p(calendar_ids_json)).unwrap_or_else(|_| Vec::new());
        let ok = self
            .as_ref()
            .rust()
            .core
            .clone()
            .lock()
            .unwrap()
            .update_date_link(
                &p(link_id),
                if ids.is_empty() { None } else { Some(ids) },
                Some(&p(date)),
                Some(&p(title)),
            )
            .unwrap_or(false);
        self.as_mut().refresh_all();
        ok
    }

    pub fn delete_date_link(mut self: Pin<&mut Self>, link_id: &QString) -> bool {
        let ok = self
            .as_ref()
            .rust()
            .core
            .clone()
            .lock()
            .unwrap()
            .delete_date_link(&p(link_id));
        self.as_mut().refresh_all();
        ok
    }

    pub fn delete_card(mut self: Pin<&mut Self>, id: &QString) -> bool {
        let ok = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.delete_card(&p(id))
        };
        self.as_mut().refresh_all();
        ok
    }

    pub fn duplicate_card(mut self: Pin<&mut Self>, id: &QString) -> QString {
        let card = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.duplicate_card(&p(id))
        };
        self.as_mut().refresh_all();
        card.map(|c| s(serde_json::to_string(&c).unwrap_or_default()))
            .unwrap_or_default()
    }

    pub fn send_card_to_grid(
        mut self: Pin<&mut Self>,
        card_id: &QString,
        grid_id: &QString,
    ) -> bool {
        let ok = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.send_card_to_grid(&p(card_id), &p(grid_id))
        };
        self.as_mut().refresh_all();
        ok
    }

    pub fn move_cards(mut self: Pin<&mut Self>, ids_json: &QString, dx: f64, dy: f64) -> bool {
        let ids: Vec<String> = serde_json::from_str(&p(ids_json)).unwrap_or_default();
        let ok = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.move_cards(&ids, dx, dy)
        };
        self.as_mut().refresh_cards();
        ok
    }

    pub fn place_floating_windows(self: Pin<&mut Self>, json: &QString) {
        place_sway_floating_windows(&p(json));
    }

    pub fn set_card_group(mut self: Pin<&mut Self>, ids_json: &QString, group: &QString) -> bool {
        let ids: Vec<String> = serde_json::from_str(&p(ids_json)).unwrap_or_default();
        let ok = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.set_cards_group(&ids, &p(group))
        };
        self.as_mut().refresh_cards();
        ok
    }

    pub fn sync_cards(mut self: Pin<&mut Self>, source_id: &QString, target_id: &QString) -> bool {
        let ok = self
            .as_ref()
            .rust()
            .core
            .clone()
            .lock()
            .unwrap()
            .sync_cards(&p(source_id), &p(target_id));
        if ok {
            self.as_mut().refresh_cards();
        }
        ok
    }

    pub fn unsync_card(mut self: Pin<&mut Self>, card_id: &QString) -> bool {
        let ok = self
            .as_ref()
            .rust()
            .core
            .clone()
            .lock()
            .unwrap()
            .unsync_card(&p(card_id));
        if ok {
            self.as_mut().refresh_cards();
        }
        ok
    }

    pub fn toggle_ai_link(mut self: Pin<&mut Self>, ai_id: &QString, card_id: &QString) -> bool {
        let ok = self
            .as_ref()
            .rust()
            .core
            .clone()
            .lock()
            .unwrap()
            .toggle_ai_link(&p(ai_id), &p(card_id));
        if ok {
            self.as_mut().refresh_cards();
        }
        ok
    }

    pub fn git_card_refresh(mut self: Pin<&mut Self>, card_id: &QString) {
        let (url, tokens) = {
            let core = self.core.lock().unwrap();
            let Some((_, card)) = core.ws.card(&p(card_id)) else {
                return;
            };
            let settings = &core.ws.settings;
            (
                card.str_field("url", ""),
                crate::gitcard::RepoTokens {
                    github: settings.repo_token_github.clone(),
                    gitlab: settings.repo_token_gitlab.clone(),
                    forgejo: settings.repo_token_forgejo.clone(),
                    auth_gh: settings.git_auth == "gh",
                },
            )
        };
        if url.trim().is_empty() {
            return;
        }
        let id = p(card_id);
        let qt = self.as_mut().qt_thread();
        runtime().spawn(async move {
            let data = crate::gitcard::fetch(&url, tokens).await;
            let _ = qt.queue(move |mut q| {
                {
                    let core = q.as_ref().rust().core.clone();
                    let mut core = core.lock().unwrap();
                    core.set_git_card_data(&id, &data);
                }
                q.as_mut().refresh_cards();
                q.as_mut().git_card_updated(s(id.clone()));
            });
        });
    }

    pub fn ai_context(&self, ids_json: &QString) -> QString {
        let ids: Vec<String> = serde_json::from_str(&p(ids_json)).unwrap_or_default();
        let core = self.core.lock().unwrap();
        s(core.ai_context(&ids))
    }

    pub fn create_template(self: Pin<&mut Self>, name: &QString, ids_json: &QString) -> QString {
        let ids: Vec<String> = serde_json::from_str(&p(ids_json)).unwrap_or_default();
        let result = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.create_template(&p(name), &ids)
        };
        match result {
            Ok(template) => s(serde_json::to_string(&template).unwrap_or_default()),
            Err(err) => s(format!("error:{err}")),
        }
    }

    pub fn insert_template(mut self: Pin<&mut Self>, id: &QString, x: f64, y: f64) -> QString {
        let result = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.insert_template(&p(id), x, y, None)
        };
        self.as_mut().refresh_all();
        match result {
            Ok(cards) => s(serde_json::to_string(&cards).unwrap_or_default()),
            Err(err) => s(format!("error:{err}")),
        }
    }

    pub fn delete_template(self: Pin<&mut Self>, id: &QString) -> bool {
        let ok = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.delete_template(&p(id))
        };
        ok
    }

    pub fn templates_json(&self) -> QString {
        let core = self.core.lock().unwrap();
        s(serde_json::to_string(&core.ws.templates).unwrap_or_else(|_| "[]".into()))
    }

    pub fn kanban_aggregate_json(&self) -> QString {
        let core = self.core.lock().unwrap();
        s(core.kanban_aggregate_json())
    }

    pub fn create_grid(mut self: Pin<&mut Self>, name: &QString) -> QString {
        let g = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.create_grid(&p(name))
        };
        self.as_mut().refresh_all();
        s(serde_json::to_string(&g).unwrap_or_default())
    }

    pub fn set_grid_icon(mut self: Pin<&mut Self>, id: &QString, icon: &QString) -> bool {
        let ok = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.set_grid_icon(&p(id), &p(icon))
        };
        self.as_mut().refresh_all();
        ok
    }

    pub fn rename_grid(mut self: Pin<&mut Self>, id: &QString, name: &QString) -> bool {
        let ok = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.rename_grid(&p(id), &p(name))
        };
        self.as_mut().refresh_all();
        ok
    }

    pub fn delete_grid(mut self: Pin<&mut Self>, id: &QString) -> bool {
        let ok = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.delete_grid(&p(id))
        };
        self.as_mut().refresh_all();
        ok
    }

    pub fn switch_grid(mut self: Pin<&mut Self>, id: &QString) -> bool {
        let ok = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.switch_grid(&p(id))
        };
        self.as_mut().refresh_all();
        ok
    }

    pub fn create_folder(mut self: Pin<&mut Self>, name: &QString) -> QString {
        let f = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.create_folder(&p(name))
        };
        self.as_mut().refresh_all();
        s(serde_json::to_string(&f).unwrap_or_default())
    }

    pub fn rename_folder(mut self: Pin<&mut Self>, id: &QString, name: &QString) -> bool {
        let ok = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.rename_folder(&p(id), &p(name))
        };
        self.as_mut().refresh_all();
        ok
    }

    pub fn delete_folder(mut self: Pin<&mut Self>, id: &QString, keep_content: bool) -> bool {
        let ok = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.delete_folder(&p(id), keep_content)
        };
        self.as_mut().refresh_all();
        ok
    }

    pub fn toggle_folder(mut self: Pin<&mut Self>, id: &QString) {
        {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.toggle_folder(&p(id));
        }
        self.as_mut().refresh_all();
    }

    pub fn move_grid_to_folder(
        mut self: Pin<&mut Self>,
        grid_id: &QString,
        folder_id: &QString,
    ) -> bool {
        let fid = p(folder_id);
        let ok = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.move_grid_to_folder(
                &p(grid_id),
                if fid.is_empty() {
                    None
                } else {
                    Some(fid.as_str())
                },
            )
        };
        self.as_mut().refresh_all();
        ok
    }

    pub fn reorder_grid(mut self: Pin<&mut Self>, id: &QString, before: &QString) -> bool {
        let b = p(before);
        let ok = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.reorder_grid(&p(id), if b.is_empty() { None } else { Some(b.as_str()) })
        };
        self.as_mut().refresh_all();
        ok
    }

    pub fn set_setting(mut self: Pin<&mut Self>, key: &QString, value_json: &QString) -> bool {
        let ok = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.set_setting_json(&p(key), &p(value_json))
        };
        self.as_mut().refresh_all();
        ok
    }

    pub fn set_view(mut self: Pin<&mut Self>, pan_x: f64, pan_y: f64, zoom: f64) {
        let zoom = zoom.clamp(0.2, 3.0);
        self.as_ref()
            .rust()
            .core
            .clone()
            .lock()
            .unwrap()
            .set_view(pan_x, pan_y, zoom);
        if (self.as_ref().rust().pan_x - pan_x).abs() > f64::EPSILON {
            self.as_mut().set_pan_x(pan_x);
        }
        if (self.as_ref().rust().pan_y - pan_y).abs() > f64::EPSILON {
            self.as_mut().set_pan_y(pan_y);
        }
        if (self.as_ref().rust().zoom - zoom).abs() > f64::EPSILON {
            self.as_mut().set_zoom(zoom);
        }
    }

    pub fn set_viewport(self: Pin<&mut Self>, width: f64, height: f64) {
        self.as_ref()
            .rust()
            .core
            .clone()
            .lock()
            .unwrap()
            .set_viewport(width, height);
    }

    pub fn undo(mut self: Pin<&mut Self>) -> bool {
        let ok = self.as_ref().rust().core.clone().lock().unwrap().undo();
        self.as_mut().refresh_all();
        ok
    }

    pub fn redo(mut self: Pin<&mut Self>) -> bool {
        let ok = self.as_ref().rust().core.clone().lock().unwrap().redo();
        self.as_mut().refresh_all();
        ok
    }

    pub fn search_now(&self, query: &QString) -> QString {
        let core = self.core.lock().unwrap();
        s(core.search_json(&p(query)))
    }

    pub fn command(mut self: Pin<&mut Self>, id: &QString) -> QString {
        let v = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.command(&p(id))
        };
        self.as_mut().refresh_all();
        s(v.to_string())
    }

    pub fn run_script(&self, script: &QString) -> QString {
        match integrations::run_script(&p(script), Duration::from_secs(30)) {
            Ok(v) => s(v.to_string()),
            Err(e) => s(serde_json::json!({"ok": false, "error": e.to_string()}).to_string()),
        }
    }

    pub fn mpris_players(&self) -> QString {
        s(integrations::mpris_players().to_string())
    }

    pub fn mpris_action(&self, player: &QString, action: &QString) -> QString {
        s(integrations::mpris_action(&p(player), &p(action)).to_string())
    }

    pub fn ai_send(
        mut self: Pin<&mut Self>,
        provider: &QString,
        model_name: &QString,
        prompt: &QString,
        context: &QString,
    ) -> QString {
        let (api_key, server_url, web, stream) = {
            let core_arc = self.as_ref().rust().core.clone();
            let core = core_arc.lock().unwrap();
            let prov = p(provider);
            (
                core.ws
                    .settings
                    .ai_api_keys
                    .get(&prov)
                    .cloned()
                    .unwrap_or_default(),
                core.ws.settings.ai_local_server_url.clone(),
                core.ws.settings.ai_web_search,
                core.ws.settings.ai_stream,
            )
        };
        let id = uuid::Uuid::new_v4().to_string();
        let ctx = p(context);
        let prompt = if ctx.trim().is_empty() {
            p(prompt)
        } else {
            format!("{}\n\nContext:\n{}", p(prompt), ctx)
        };
        let req = AIRequest {
            provider: p(provider),
            model: if p(model_name).is_empty() {
                ai::default_model(&p(provider)).to_string()
            } else {
                p(model_name)
            },
            api_key,
            prompt,
            system_prompt: ai::system_prompt(),
            web_search: web,
            server_url,
        };
        let qt = self.as_mut().qt_thread();
        self.as_mut().set_ai_busy(true);
        let id_for_stream = id.clone();
        let id_for_done = id.clone();
        let qt_stream = qt.clone();
        runtime().spawn(async move {
            let cb = move |chunk: &str| {
                let text = chunk.to_string();
                let id = id_for_stream.clone();
                let _ = qt_stream.queue(move |mut q| {
                    q.as_mut().ai_chunk(s(id.clone()), s(text.clone()));
                });
            };
            let _ = stream;
            match ai::complete(req, cb).await {
                Ok(full) => {
                    let id = id_for_done;
                    let _ = qt.queue(move |mut q| {
                        q.as_mut().ai_done(s(id), s(full));
                        q.as_mut().set_ai_busy(false);
                    });
                }
                Err(e) => {
                    let id = id_for_done;
                    let msg = e.to_string();
                    let _ = qt.queue(move |mut q| {
                        q.as_mut().ai_error(s(id), s(msg));
                        q.as_mut().set_ai_busy(false);
                    });
                }
            }
        });
        s(id)
    }

    pub fn ai_validate(mut self: Pin<&mut Self>, provider: &QString, key: &QString) -> QString {
        let prov = p(provider);
        let key = p(key);
        let server = self
            .as_ref()
            .rust()
            .core
            .clone()
            .lock()
            .unwrap()
            .ws
            .settings
            .ai_local_server_url
            .clone();
        let qt = self.as_mut().qt_thread();
        runtime().spawn(async move {
            match ai::validate_key(&prov, &key, &server).await {
                Ok(()) => {
                    let provider = prov.clone();
                    let _ = qt.queue(move |mut q| {
                        q.as_mut().ai_key_result(s(provider), true, s("Connected"));
                    });
                }
                Err(err) => {
                    let provider = prov.clone();
                    let message = err.to_string();
                    let _ = qt.queue(move |mut q| {
                        q.as_mut().ai_key_result(s(provider), false, s(message));
                    });
                }
            }
        });
        s("")
    }

    pub fn ai_fetch_models(
        mut self: Pin<&mut Self>,
        server_url: &QString,
        key: &QString,
    ) -> QString {
        let url = p(server_url);
        let key = p(key);
        let qt = self.as_mut().qt_thread();
        runtime().spawn(async move {
            let json = match ai::fetch_models(&url, &key).await {
                Ok(models) => serde_json::json!({"success": true, "models": models}).to_string(),
                Err(err) => {
                    serde_json::json!({"success": false, "error": err.to_string()}).to_string()
                }
            };
            let _ = qt.queue(move |mut q| {
                q.as_mut().ai_models_result(s(json));
            });
        });
        s("")
    }

    pub fn sync_now(mut self: Pin<&mut Self>) {
        let (provider, folder, refresh, client_id, secret, data_dir, local_json) = {
            let core_arc = self.as_ref().rust().core.clone();
            let core = core_arc.lock().unwrap();
            (
                core.ws.settings.cloud_provider.clone(),
                core.ws.settings.cloud_folder.clone(),
                core.ws.settings.gdrive_refresh_token.clone(),
                core.ws.settings.gdrive_client_id.clone(),
                core.ws.settings.gdrive_client_secret.clone(),
                core.data_dir.clone(),
                core.ws.clone(),
            )
        };
        if !local_json.settings.cloud_sync_enabled {
            self.as_mut().toast(s("Cloud Sync is off"));
            return;
        }
        let qt = self.as_mut().qt_thread();
        self.as_mut().set_syncing(true);
        runtime().spawn(async move {
            let result = async {
                if provider == "gdrive" {
                    let Some(refresh) = refresh else {
                        return Err(anyhow::anyhow!("Google Drive is not connected"));
                    };
                    let Some(client_id) = client_id else {
                        return Err(anyhow::anyhow!("Google Drive client id missing"));
                    };
                    let token =
                        sync::refresh_token(&client_id, secret.as_deref(), &refresh).await?;
                    let remote = sync::drive_read(&token).await?;
                    let local = std::fs::read(data_dir.join("workspace.json")).unwrap_or_default();
                    if let Some(remote) = remote {
                        if remote != local {
                            let _ = std::fs::create_dir_all(data_dir.join("backups"));
                            let name = format!(
                                "conflict-{}.json",
                                time::OffsetDateTime::now_utc().unix_timestamp()
                            );
                            let _ = std::fs::write(data_dir.join("backups").join(name), &remote);
                            let ws: crate::model::Workspace = serde_json::from_slice(&remote)?;
                            std::fs::write(data_dir.join("workspace.json"), &remote)?;
                            return Ok::<_, anyhow::Error>((Some(ws), "pulled".to_string()));
                        }
                    } else {
                        sync::drive_write(&token, &local).await?;
                        return Ok((None, "pushed".to_string()));
                    }
                    Ok((None, "up-to-date".to_string()))
                } else if let Some(folder) = folder {
                    let (status, ws) = sync::sync_folder(&data_dir, std::path::Path::new(&folder))?;
                    Ok((ws, status))
                } else {
                    Err(anyhow::anyhow!("No sync folder selected"))
                }
            }
            .await;
            match result {
                Ok((ws, status)) => {
                    let _ = qt.queue(move |mut q| {
                        if let Some(mut ws) = ws {
                            ws.migrate();
                            if let Ok(mut core) = q.as_ref().rust().core.clone().lock() {
                                core.ws = ws;
                                let ws_snapshot = core.ws.clone();
                                core.history.reset(&ws_snapshot);
                            }
                        }
                        q.as_mut().set_syncing(false);
                        q.as_mut().refresh_all();
                        q.as_mut().notice(s("sync"), s(status));
                    });
                }
                Err(e) => {
                    let msg = e.to_string();
                    let _ = qt.queue(move |mut q| {
                        q.as_mut().set_syncing(false);
                        q.as_mut().notice(s("sync-error"), s(msg));
                    });
                }
            }
        });
    }

    pub fn iroh_create(mut self: Pin<&mut Self>) -> QString {
        let iroh = self.as_ref().rust().core.lock().unwrap().iroh.clone();
        match iroh.create_workspace() {
            Ok(ticket) => {
                self.as_mut().refresh_all();
                s(ticket)
            }
            Err(err) => {
                self.as_mut().notice(s("sync-error"), s(err.to_string()));
                s("")
            }
        }
    }

    pub fn iroh_join(mut self: Pin<&mut Self>, ticket: &QString) -> QString {
        let iroh = self.as_ref().rust().core.lock().unwrap().iroh.clone();
        match iroh.join(&p(ticket)) {
            Ok(namespace) => {
                self.as_mut().refresh_all();
                s(namespace)
            }
            Err(err) => {
                self.as_mut().notice(s("sync-error"), s(err.to_string()));
                s("")
            }
        }
    }

    pub fn iroh_share(mut self: Pin<&mut Self>, read_only: bool) -> QString {
        let iroh = self.as_ref().rust().core.lock().unwrap().iroh.clone();
        match iroh.share(read_only) {
            Ok(ticket) => s(ticket),
            Err(err) => {
                self.as_mut().notice(s("sync-error"), s(err.to_string()));
                s("")
            }
        }
    }

    pub fn iroh_sync_now(mut self: Pin<&mut Self>) {
        let iroh = self.as_ref().rust().core.lock().unwrap().iroh.clone();
        let qt = self.as_mut().qt_thread();
        std::thread::spawn(move || {
            let result = iroh.sync_now();
            let _ = qt.queue(move |mut q| match result {
                Ok(_) => {
                    q.as_mut().refresh_all();
                    q.as_mut().notice(s("sync"), s("Synced"));
                }
                Err(err) => {
                    q.as_mut().notice(s("sync-error"), s(err.to_string()));
                }
            });
        });
    }

    pub fn copy_text(mut self: Pin<&mut Self>, text: &QString) {
        if crate::clipboard::write_text(&p(text)) {
            self.as_mut().notice(s("copied"), s("Copied to clipboard"));
        }
    }

    pub fn iroh_cache_ticket(mut self: Pin<&mut Self>) -> QString {
        let iroh = self.as_ref().rust().core.lock().unwrap().iroh.clone();
        match iroh.cache_ticket() {
            Ok(ticket) => s(ticket),
            Err(err) => {
                self.as_mut().notice(s("sync-error"), s(err.to_string()));
                s("")
            }
        }
    }

    pub fn iroh_pair_create(mut self: Pin<&mut Self>) -> QString {
        let iroh = self.as_ref().rust().core.lock().unwrap().iroh.clone();
        let setup = match iroh.share(false) {
            Ok(setup) => setup,
            Err(err) => {
                self.as_mut().notice(s("sync-error"), s(err.to_string()));
                return s("");
            }
        };
        match crate::pairing::create(&setup) {
            Ok(code) => s(code),
            Err(err) => {
                self.as_mut().notice(s("sync-error"), s(err.to_string()));
                s("")
            }
        }
    }

    pub fn iroh_pair_join(mut self: Pin<&mut Self>, code: &QString) -> QString {
        let setup = match crate::pairing::take(&p(code)) {
            Ok(setup) => setup,
            Err(err) => {
                self.as_mut().notice(s("sync-error"), s(err.to_string()));
                return s(err.to_string());
            }
        };
        let iroh = self.as_ref().rust().core.lock().unwrap().iroh.clone();
        match iroh.join(&setup) {
            Ok(_) => {
                self.as_mut().refresh_all();
                s("")
            }
            Err(err) => {
                self.as_mut().notice(s("sync-error"), s(err.to_string()));
                s(err.to_string())
            }
        }
    }

    pub fn iroh_leave(mut self: Pin<&mut Self>) -> QString {
        let iroh = self.as_ref().rust().core.lock().unwrap().iroh.clone();
        match iroh.leave() {
            Ok(_) => s("left"),
            Err(err) => {
                self.as_mut().notice(s("sync-error"), s(err.to_string()));
                s("")
            }
        }
    }

    pub fn iroh_relay_url(&self) -> QString {
        s(crate::irohsync::relay_url())
    }

    pub fn iroh_set_relay(mut self: Pin<&mut Self>, url: &QString) -> QString {
        if let Err(err) = crate::irohsync::set_relay_url(&p(url)) {
            return s(err.to_string());
        }
        let iroh = self.as_ref().rust().core.lock().unwrap().iroh.clone();
        match iroh.reload_relay() {
            Ok(_) => {
                self.as_mut().refresh_all();
                s("")
            }
            Err(err) => {
                self.as_mut().notice(s("sync-error"), s(err.to_string()));
                s(err.to_string())
            }
        }
    }

    pub fn gdrive_connect(mut self: Pin<&mut Self>) -> QString {
        let (client_id, secret) = {
            let core_arc = self.as_ref().rust().core.clone();
            let core = core_arc.lock().unwrap();
            (
                core.ws.settings.gdrive_client_id.clone(),
                core.ws.settings.gdrive_client_secret.clone(),
            )
        };
        let Some(client_id) = client_id else {
            self.as_mut()
                .toast(s("Add a Google OAuth client id in Settings first"));
            return s("{\"success\":false}");
        };
        let (verifier, challenge) = sync::pkce_pair();
        let redirect = "http://127.0.0.1:47823/";
        let url = sync::auth_url(&client_id, redirect, &challenge);
        let _ = open::that_detached(&url);
        let qt = self.as_mut().qt_thread();
        let data_dir = self
            .as_ref()
            .rust()
            .core
            .clone()
            .lock()
            .unwrap()
            .data_dir
            .clone();
        runtime().spawn(async move {
            let code = wait_for_oauth_code(47823).await;
            let result = async {
                let code = code?;
                let tokens =
                    sync::exchange_code(&client_id, secret.as_deref(), &code, &verifier, redirect)
                        .await?;
                let refresh = tokens
                    .get("refresh_token")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .ok_or_else(|| anyhow::anyhow!("no refresh token returned"))?;
                let access = tokens
                    .get("access_token")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let email = sync::user_email(access).await;
                Ok::<_, anyhow::Error>((refresh, email))
            }
            .await;
            match result {
                Ok((refresh, email)) => {
                    let _ = qt.queue(move |mut q| {
                        if let Ok(mut core) = q.as_ref().rust().core.clone().lock() {
                            core.ws.settings.gdrive_connected = true;
                            core.ws.settings.gdrive_refresh_token = Some(refresh.clone());
                            core.ws.settings.gdrive_email = email.clone();
                            core.ws.settings.cloud_provider = "gdrive".into();
                            core.ws.settings.cloud_sync_enabled = true;
                            core.save_quiet();
                        }
                        q.as_mut().refresh_all();
                        q.as_mut().notice(s("gdrive"), s("Google Drive connected"));
                    });
                }
                Err(e) => {
                    let _ = data_dir;
                    let msg = e.to_string();
                    let _ = qt.queue(move |mut q| q.as_mut().notice(s("gdrive-error"), s(msg)));
                }
            }
        });
        s("{\"success\":true,\"pending\":true}")
    }

    pub fn gdrive_disconnect(mut self: Pin<&mut Self>) {
        {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.ws.settings.gdrive_connected = false;
            core.ws.settings.gdrive_refresh_token = None;
            core.ws.settings.gdrive_email = None;
            core.ws.settings.cloud_provider = "none".into();
            core.save_quiet();
        }
        self.as_mut().refresh_all();
    }

    pub fn export_backup(self: Pin<&mut Self>, path: &QString) -> bool {
        let core_arc = self.as_ref().rust().core.clone();
        let mut core = core_arc.lock().unwrap();
        core.export_backup(&p(path)).is_ok()
    }

    pub fn import_backup(mut self: Pin<&mut Self>, path: &QString) -> bool {
        let ok = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.import_backup(&p(path)).is_ok()
        };
        self.as_mut().refresh_all();
        ok
    }

    pub fn import_legacy(mut self: Pin<&mut Self>, path: &QString) -> bool {
        let ok = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.import_legacy(&p(path)).is_ok()
        };
        self.as_mut().refresh_all();
        ok
    }

    pub fn set_data_dir(mut self: Pin<&mut Self>, path: &QString) -> bool {
        let ok = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.set_data_dir(&p(path)).is_ok()
        };
        self.as_mut().refresh_all();
        ok
    }

    pub fn check_updates(mut self: Pin<&mut Self>) {
        let channel = self
            .as_ref()
            .rust()
            .core
            .lock()
            .unwrap()
            .ws
            .settings
            .update_channel
            .clone();
        let qt = self.as_mut().qt_thread();
        runtime().spawn(async move {
            match updates::check(&channel).await {
                Ok(state) => {
                    let _ = qt.queue(move |mut q| {
                        if let Ok(mut core) = q.as_ref().rust().core.clone().lock() {
                            core.update = state.clone();
                            core.save_quiet();
                        }
                        q.as_mut()
                            .set_update_json(s(serde_json::to_string(&state).unwrap_or_default()));
                    });
                }
                Err(e) => {
                    let msg = e.to_string();
                    let _ = qt.queue(move |mut q| q.as_mut().notice(s("update-error"), s(msg)));
                }
            }
        });
    }

    pub fn download_update(mut self: Pin<&mut Self>) {
        let (url, data_dir) = {
            let core_arc = self.as_ref().rust().core.clone();
            let core = core_arc.lock().unwrap();
            (core.update.url.clone(), core.data_dir.clone())
        };
        let Some(url) = url else {
            self.as_mut().toast(s("No update available"));
            return;
        };
        let qt = self.as_mut().qt_thread();
        runtime().spawn(async move {
            let dest = data_dir.join("updates").join("Corcho.AppImage");
            let qt_progress = qt.clone();
            let res = updates::download(&url, &dest, move |pct| {
                let _ = qt_progress.queue(move |mut q| {
                    let mut value: serde_json::Value =
                        serde_json::from_str(&q.update_json().to_string())
                            .unwrap_or_else(|_| serde_json::json!({}));
                    value["progress"] = serde_json::json!(pct);
                    value["status"] = serde_json::json!("downloading");
                    q.as_mut().set_update_json(s(value.to_string()));
                });
            })
            .await;
            match res {
                Ok(()) => {
                    let _ = qt.queue(move |mut q| {
                        q.as_mut().notice(
                            s("update"),
                            s(format!("Update downloaded to {}", dest.display())),
                        );
                    });
                }
                Err(e) => {
                    let msg = e.to_string();
                    let _ = qt.queue(move |mut q| q.as_mut().notice(s("update-error"), s(msg)));
                }
            }
        });
    }

    pub fn install_update(&self) -> bool {
        let dir = self.core.lock().unwrap().data_dir.clone();
        let path = dir.join("updates").join("Corcho.AppImage");
        updates::install(&path).is_ok()
    }

    pub fn read_attachment(&self, name: &QString) -> QString {
        let data_dir = self.core.lock().unwrap().data_dir.clone();
        match crate::storage::read_attachment(&data_dir, &p(name)) {
            Ok(bytes) => s(base64::engine::general_purpose::STANDARD.encode(bytes)),
            Err(_) => QString::default(),
        }
    }

    pub fn write_attachment(&self, name: &QString, base64_data: &QString) -> QString {
        let data_dir = self.core.lock().unwrap().data_dir.clone();
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(p(base64_data)) else {
            return QString::default();
        };
        match crate::storage::write_attachment(&data_dir, &p(name), &bytes) {
            Ok(path) => s(path.to_string_lossy().to_string()),
            Err(_) => QString::default(),
        }
    }

    pub fn list_attachments(&self) -> QString {
        let data_dir = self.core.lock().unwrap().data_dir.clone();
        s(
            serde_json::to_string(&crate::storage::list_attachments(&data_dir))
                .unwrap_or_else(|_| "[]".into()),
        )
    }

    pub fn open_external(&self, url: &QString) {
        let _ = open::that_detached(p(url));
    }

    pub fn pin_path(
        mut self: Pin<&mut Self>,
        path: &QString,
        x: f64,
        y: f64,
        grid_id: &QString,
    ) -> QString {
        let grid = p(grid_id);
        let result = {
            let core_arc = self.as_ref().rust().core.clone();
            let mut core = core_arc.lock().unwrap();
            core.pin_path(
                &p(path),
                x,
                y,
                if grid.is_empty() { None } else { Some(&grid) },
            )
        };
        self.as_mut().refresh_all();
        match result {
            Ok(card) => s(serde_json::to_string(&card).unwrap_or_default()),
            Err(err) => s(format!("error:{err}")),
        }
    }

    pub fn read_pinned_file(&self, path: &QString) -> QString {
        let core = self.core.lock().unwrap();
        match core.read_pinned_file(&p(path)) {
            Ok(text) => s(text),
            Err(err) => s(format!("error:{err}")),
        }
    }

    pub fn list_pinned_folder(&self, path: &QString) -> QString {
        let core = self.core.lock().unwrap();
        match core.list_pinned_folder(&p(path)) {
            Ok(value) => s(value.to_string()),
            Err(err) => s(format!("error:{err}")),
        }
    }

    pub fn open_path(&self, path: &QString) {
        let _ = open::that_detached(p(path));
    }

    pub fn open_themes_folder(&self) {
        let dir = self.core.lock().unwrap().data_dir.join("themes");
        let _ = std::fs::create_dir_all(&dir);
        let _ = open::that_detached(dir);
    }

    pub fn create_theme_from_template(mut self: Pin<&mut Self>) -> QString {
        let dir = {
            let core_arc = self.as_ref().rust().core.clone();
            let core = core_arc.lock().unwrap();
            theme::themes_dir(&core.data_dir)
        };
        let _ = std::fs::create_dir_all(&dir);
        let mut index = 1;
        let path = loop {
            let name = if index == 1 {
                "my-theme.css".to_string()
            } else {
                format!("my-theme-{index}.css")
            };
            let candidate = dir.join(name);
            if !candidate.exists() {
                break candidate;
            }
            index += 1;
        };
        if let Err(err) = std::fs::write(&path, theme::TEMPLATE) {
            tracing::warn!("create theme: {err}");
            return QString::default();
        }
        self.as_mut().refresh_all();
        s(path.display().to_string())
    }

    pub fn set_launch_at_startup(mut self: Pin<&mut Self>, enabled: bool) -> bool {
        let ok = write_autostart(enabled);
        if ok {
            if let Ok(mut core) = self.as_ref().rust().core.clone().lock() {
                core.ws.settings.launch_at_startup = enabled;
                core.save_quiet();
            }
            self.as_mut().refresh_all();
        }
        ok
    }

    pub fn reset_tutorials(mut self: Pin<&mut Self>) {
        self.as_ref()
            .rust()
            .core
            .clone()
            .lock()
            .unwrap()
            .reset_tutorials();
        self.as_mut().refresh_all();
    }

    pub fn timbre_start(&self, card_id: &QString, volume: f64, tone_freq: f64, tone_mix: f64) {
        if let Err(e) = crate::audio::start(
            &p(card_id),
            volume as f32,
            tone_freq as f32,
            tone_mix as f32,
        ) {
            tracing::warn!("timbre audio: {e}");
        }
    }

    pub fn timbre_stop(&self, card_id: &QString) {
        crate::audio::stop(&p(card_id));
    }

    pub fn pomodoro_chime(&self) {
        if let Err(e) = crate::audio::chime() {
            tracing::warn!("pomodoro chime: {e}");
        }
    }

    pub fn adblock_check(&self, url: &QString) -> bool {
        self.core.lock().unwrap().adblock.blocked(&p(url))
    }

    pub fn adblock_hosts(&self) -> QString {
        s(self.core.lock().unwrap().adblock.hosts_json())
    }

    pub fn changelog(&self) -> QString {
        s(CHANGELOG)
    }

    pub fn quit(&self) {
        // QML mapsto Qt.quit(); kept as an invokable for menu wiring.
    }
}

const CHANGELOG: &str = "Corcho 0.1.0\n\n- Rewritten as a native Rust + Qt6 application.\n- Infinite grid canvas with fourteen card types.\n- Calendar cards with multi-calendar date links and reminders.\n- Dedicated Timbre audio card.\n- Five AI providers plus a local server.\n- Folder and Google Drive sync, encrypted backups.\n- Stable, beta and experimental update channels.";

pub(crate) fn write_autostart(enabled: bool) -> bool {
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    let dir = PathBuf::from(home).join(".config/autostart");
    let path = dir.join("corcho.desktop");
    if enabled {
        if std::fs::create_dir_all(&dir).is_err() {
            return false;
        }
        let exe = std::env::current_exe()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "corcho".into());
        let body = format!(
            "[Desktop Entry]\nType=Application\nName=Corcho\nExec={exe}\nX-GNOME-Autostart-enabled=true\n"
        );
        std::fs::write(path, body).is_ok()
    } else {
        let _ = std::fs::remove_file(path);
        true
    }
}

async fn wait_for_oauth_code(port: u16) -> anyhow::Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let (mut socket, _) = listener.accept().await?;
    let mut buf = [0u8; 4096];
    let n = socket.read(&mut buf).await?;
    let request = String::from_utf8_lossy(&buf[..n]).to_string();
    let code = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|path| url::Url::parse(&format!("http://127.0.0.1{path}")).ok())
        .and_then(|url| {
            url.query_pairs()
                .find(|(k, _)| k == "code")
                .map(|(_, v)| v.to_string())
        })
        .ok_or_else(|| anyhow::anyhow!("OAuth cancelled"))?;
    let body = "<html><body style=\"font-family:sans-serif;background:#121214;color:#e8e8ea\"><h2>Corcho connected</h2><p>You can close this window.</p></body></html>";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = socket.write_all(response.as_bytes()).await;
    Ok(code)
}

fn prune_snapshots(dir: &std::path::Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut snapshots: Vec<_> = entries
        .flatten()
        .filter(|e| {
            e.file_name().to_string_lossy().starts_with("snapshot-")
                && e.path().extension().and_then(|v| v.to_str()) == Some("zip")
        })
        .collect();
    snapshots.sort_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
    while snapshots.len() > keep {
        let entry = snapshots.remove(0);
        let _ = std::fs::remove_file(entry.path());
    }
}
