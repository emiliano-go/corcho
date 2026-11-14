use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const SCHEMA_VERSION: u32 = 2;

#[allow(dead_code)]
pub const CARD_KINDS: &[&str] = &[
    "note",
    "website",
    "bookmark",
    "api",
    "ai",
    "script",
    "calendar",
    "timbre",
    "portal",
    "checklist",
    "kanban",
    "whiteboard",
    "pomodoro",
    "mpris",
    "file",
    "git",
    "legend",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Card {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub locked: bool,
    #[serde(flatten, default)]
    pub payload: Map<String, Value>,
}

impl Card {
    pub fn new(kind: &str, x: f64, y: f64) -> Self {
        let (w, h, color, icon, title) = default_card_style(kind);
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            kind: kind.to_string(),
            x,
            y,
            width: w,
            height: h,
            color: color.to_string(),
            icon: icon.to_string(),
            title: title.to_string(),
            text: String::new(),
            locked: false,
            payload: Map::new(),
        }
    }

    #[allow(dead_code)]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.payload.get(key)
    }

    pub fn set(&mut self, key: &str, value: Value) {
        self.payload.insert(key.to_string(), value);
    }

    #[allow(dead_code)]
    pub fn str_field(&self, key: &str, fallback: &str) -> String {
        self.payload
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or(fallback)
            .to_string()
    }

    pub fn search_text(&self) -> String {
        let mut s = format!("{} {} {}", self.title, self.text, self.kind);
        for key in ["url", "prompt", "name", "artist", "album"] {
            if let Some(v) = self.payload.get(key).and_then(|v| v.as_str()) {
                s.push(' ');
                s.push_str(v);
            }
        }
        s
    }
}

pub fn default_card_style(kind: &str) -> (f64, f64, &'static str, &'static str, &'static str) {
    match kind {
        "note" => (384.0, 320.0, "#3d3f52", "file-text", ""),
        "website" => (512.0, 384.0, "#2f3b45", "globe", "Website"),
        "bookmark" => (320.0, 160.0, "#2f3b45", "bookmark", "Bookmark"),
        "api" => (480.0, 320.0, "#2f3b45", "database", "API Data"),
        "ai" => (416.0, 330.0, "#3a3348", "cpu", "AI"),
        "script" => (416.0, 256.0, "#333a33", "terminal", "Script"),
        "calendar" => (520.0, 560.0, "#343a3d", "calendar", "Calendar"),
        "timbre" => (320.0, 260.0, "#3d3436", "audio-lines", "Timbre"),
        "portal" => (288.0, 128.0, "#323a3d", "arrow-right-circle", "Grid Portal"),
        "checklist" => (352.0, 320.0, "#353a33", "list-checks", "Checklist"),
        "kanban" => (416.0, 320.0, "#33384a", "kanban", "Kanban"),
        "whiteboard" => (416.0, 320.0, "#333333", "pencil", "Whiteboard"),
        "pomodoro" => (320.0, 256.0, "#3d3436", "timer", "Pomodoro"),
        "mpris" => (384.0, 192.0, "#333842", "music", "Now Playing"),
        "file" => (480.0, 420.0, "#333a3d", "file-text", ""),
        "git" => (440.0, 320.0, "#26343a", "git-branch", "Git"),
        "legend" => (320.0, 280.0, "#333a3d", "list", "Legend"),
        _ => (320.0, 240.0, "#3d3f52", "square", ""),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Grid {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_grid_icon")]
    pub icon: String,
    #[serde(default, rename = "folderId")]
    pub folder_id: Option<String>,
    #[serde(default)]
    pub cards: Vec<Card>,
    #[serde(default)]
    pub pan_x: f64,
    #[serde(default)]
    pub pan_y: f64,
    #[serde(default = "default_zoom")]
    pub zoom: f64,
}

pub fn default_zoom() -> f64 {
    1.0
}

pub fn default_grid_icon() -> String {
    "layout-grid".into()
}

impl Default for Grid {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            icon: default_grid_icon(),
            folder_id: None,
            cards: Vec::new(),
            pan_x: 0.0,
            pan_y: 0.0,
            zoom: 1.0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Folder {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub collapsed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CustomTheme {
    pub background: String,
    pub surface: String,
    pub accent: String,
    pub text: String,
}

impl Default for CustomTheme {
    fn default() -> Self {
        Self {
            background: "#121214".into(),
            surface: "#1b1b1f".into(),
            accent: "#c9a86a".into(),
            text: "#e8e8ea".into(),
        }
    }
}

fn t() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AiPreset {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: String,
    pub theme_mode: String,
    pub theme_light: String,
    pub theme_dark: String,
    pub font: String,
    pub grid_opacity: f64,
    pub card_radius: f64,
    pub minimap_display: String,
    pub minimap_size: String,
    pub trackpad_pan: bool,
    pub grid_magnetic: bool,
    pub spawn_animations: bool,
    pub card_parallax: bool,
    pub grid_ripple: bool,
    pub hardware_acceleration: bool,
    pub master_volume: i32,
    pub link_behavior: String,
    pub show_card_tooltips: bool,
    pub whiteboard_pressure: bool,
    pub canvas_long_press_menu: bool,
    pub ad_blocker: bool,
    pub web_shared_profile: bool,
    pub web_sleep_minutes: u32,
    pub low_memory_mode: bool,
    pub undo_steps: usize,
    pub launcher_shortcut: String,
    pub launch_at_startup: bool,
    pub show_changelog: bool,
    pub update_mode: String,
    pub update_channel: String,
    #[serde(rename = "aiProvider")]
    pub ai_provider: String,
    #[serde(rename = "aiApiKeys")]
    pub ai_api_keys: HashMap<String, String>,
    #[serde(rename = "aiModel")]
    pub ai_model: HashMap<String, String>,
    #[serde(rename = "aiPresets")]
    pub ai_presets: Vec<AiPreset>,
    pub ai_local_server_url: String,
    pub ai_stream: bool,
    pub ai_web_search: bool,
    pub ai_refresh_interval: String,
    pub cloud_sync_enabled: bool,
    pub cloud_provider: String,
    pub cloud_folder: Option<String>,
    pub cloud_file_sync: bool,
    pub cloud_indicator: bool,
    pub gdrive_connected: bool,
    pub gdrive_client_id: Option<String>,
    pub gdrive_client_secret: Option<String>,
    pub gdrive_refresh_token: Option<String>,
    pub gdrive_email: Option<String>,
    pub onboarding_completed: bool,
    pub card_tutorial_completed: bool,
    pub context_tutorial_completed: bool,
    pub launcher_tutorial_completed: bool,
    #[serde(rename = "customTheme")]
    pub custom_theme: Option<CustomTheme>,
    pub last_seen_version: Option<String>,
    pub debug_mode: bool,
    pub allow_scripts: bool,
    pub reduced_motion: bool,
    pub data_dir: Option<String>,
    pub iroh_sync_enabled: bool,
    pub iroh_include_attachments: bool,
    pub git_auth: String,
    pub repo_token_github: String,
    pub repo_token_gitlab: String,
    pub repo_token_forgejo: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "corcho".into(),
            theme_mode: "system".into(),
            theme_light: "paper".into(),
            theme_dark: "corcho".into(),
            font: "inter".into(),
            grid_opacity: 0.6,
            card_radius: 14.0,
            minimap_display: "always".into(),
            minimap_size: "medium".into(),
            trackpad_pan: t(),
            grid_magnetic: t(),
            spawn_animations: t(),
            card_parallax: false,
            grid_ripple: false,
            hardware_acceleration: t(),
            master_volume: 50,
            link_behavior: "integrated".into(),
            show_card_tooltips: t(),
            whiteboard_pressure: t(),
            canvas_long_press_menu: t(),
            ad_blocker: false,
            web_shared_profile: t(),
            web_sleep_minutes: 10,
            low_memory_mode: false,
            undo_steps: 20,
            launcher_shortcut: "CmdOrCtrl+K".into(),
            launch_at_startup: false,
            show_changelog: t(),
            update_mode: "check".into(),
            update_channel: "stable".into(),
            ai_provider: "claude".into(),
            ai_api_keys: HashMap::new(),
            ai_model: HashMap::new(),
            ai_presets: Vec::new(),
            ai_local_server_url: String::new(),
            ai_stream: t(),
            ai_web_search: t(),
            ai_refresh_interval: "launch".into(),
            cloud_sync_enabled: false,
            cloud_provider: "none".into(),
            cloud_folder: None,
            cloud_file_sync: false,
            cloud_indicator: false,
            gdrive_connected: false,
            gdrive_client_id: None,
            gdrive_client_secret: None,
            gdrive_refresh_token: None,
            gdrive_email: None,
            onboarding_completed: false,
            card_tutorial_completed: false,
            context_tutorial_completed: false,
            launcher_tutorial_completed: false,
            custom_theme: None,
            last_seen_version: None,
            debug_mode: false,
            allow_scripts: false,
            reduced_motion: false,
            data_dir: None,
            iroh_sync_enabled: false,
            iroh_include_attachments: true,
            git_auth: "system".into(),
            repo_token_github: String::new(),
            repo_token_gitlab: String::new(),
            repo_token_forgejo: String::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarDef {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub color: String,
}

impl CalendarDef {
    pub fn new(name: &str, color: &str) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            color: color.to_string(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DateLink {
    pub id: String,
    #[serde(default)]
    pub calendar_ids: Vec<String>,
    pub date: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub source_card_id: Option<String>,
    #[serde(default)]
    pub source_item_id: Option<String>,
}

impl DateLink {
    pub fn new(date: &str, calendar_ids: Vec<String>, title: &str) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            calendar_ids,
            date: date.to_string(),
            title: title.to_string(),
            source_card_id: None,
            source_item_id: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Template {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub cards: Vec<Card>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    pub version: u32,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub viewport_width: f64,
    #[serde(default)]
    pub viewport_height: f64,
    #[serde(default)]
    pub calendars: Vec<CalendarDef>,
    #[serde(default, rename = "dateLinks")]
    pub date_links: Vec<DateLink>,
    pub grids: Vec<Grid>,
    pub folders: Vec<Folder>,
    #[serde(default)]
    pub templates: Vec<Template>,
    pub settings: Settings,
    pub active_grid_id: Option<String>,
}

impl Default for Workspace {
    fn default() -> Self {
        let grid = Grid {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Grid A".into(),
            ..Default::default()
        };
        Self {
            version: SCHEMA_VERSION,
            revision: 0,
            viewport_width: 0.0,
            viewport_height: 0.0,
            calendars: vec![CalendarDef::new("Personal", "#d98e5c")],
            date_links: Vec::new(),
            active_grid_id: Some(grid.id.clone()),
            grids: vec![grid],
            folders: Vec::new(),
            templates: Vec::new(),
            settings: Settings::default(),
        }
    }
}

impl Workspace {
    pub fn active_grid(&self) -> Option<&Grid> {
        let id = self.active_grid_id.as_ref()?;
        self.grids.iter().find(|g| &g.id == id)
    }

    pub fn active_grid_mut(&mut self) -> Option<&mut Grid> {
        let id = self.active_grid_id.clone()?;
        self.grids.iter_mut().find(|g| g.id == id)
    }

    pub fn grid(&self, id: &str) -> Option<&Grid> {
        self.grids.iter().find(|g| g.id == id)
    }

    pub fn grid_mut(&mut self, id: &str) -> Option<&mut Grid> {
        self.grids.iter_mut().find(|g| g.id == id)
    }

    pub fn card(&self, id: &str) -> Option<(&Grid, &Card)> {
        for g in &self.grids {
            if let Some(c) = g.cards.iter().find(|c| c.id == id) {
                return Some((g, c));
            }
        }
        None
    }

    pub fn card_mut(&mut self, id: &str) -> Option<&mut Card> {
        for g in &mut self.grids {
            if let Some(c) = g.cards.iter_mut().find(|c| c.id == id) {
                return Some(c);
            }
        }
        None
    }

    pub fn remove_card(&mut self, id: &str) -> Option<Card> {
        for g in &mut self.grids {
            if let Some(pos) = g.cards.iter().position(|c| c.id == id) {
                return Some(g.cards.remove(pos));
            }
        }
        None
    }

    pub fn add_grid(&mut self, name: &str) -> Grid {
        let g = Grid {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            ..Default::default()
        };
        self.grids.push(g.clone());
        g
    }

    pub fn add_folder(&mut self, name: &str) -> Folder {
        let f = Folder {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            collapsed: false,
        };
        self.folders.push(f.clone());
        f
    }

    /// Upgrade older workspaces in place. Returns true when anything changed.
    pub fn migrate(&mut self) -> bool {
        let mut changed = false;
        if self.version < SCHEMA_VERSION {
            self.version = SCHEMA_VERSION;
            changed = true;
        }
        for grid in &mut self.grids {
            let before = grid.cards.len();
            grid.cards.retain(|card| card.kind != "aura");
            if grid.cards.len() != before {
                changed = true;
            }
        }
        if self.calendars.is_empty() {
            self.calendars.push(CalendarDef::new("Personal", "#d98e5c"));
            changed = true;
        }
        changed | self.prune_date_links()
    }

    /// Drop date links whose source card or source item no longer exists.
    pub fn prune_date_links(&mut self) -> bool {
        let mut valid: HashMap<String, HashSet<String>> = HashMap::new();
        for grid in &self.grids {
            for card in &grid.cards {
                let mut items = HashSet::new();
                if let Some(list) = card.payload.get("items").and_then(|v| v.as_array()) {
                    for item in list {
                        if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                            items.insert(id.to_string());
                        }
                    }
                }
                if let Some(columns) = card.payload.get("columns").and_then(|v| v.as_array()) {
                    for column in columns {
                        if let Some(list) = column.get("items").and_then(|v| v.as_array()) {
                            for item in list {
                                if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                                    items.insert(id.to_string());
                                }
                            }
                        }
                    }
                }
                valid.insert(card.id.clone(), items);
            }
        }
        let before = self.date_links.len();
        self.date_links.retain(|link| {
            let Some(card_id) = link.source_card_id.as_deref() else {
                return true;
            };
            let Some(items) = valid.get(card_id) else {
                return false;
            };
            match link.source_item_id.as_deref() {
                Some(item_id) => items.contains(item_id),
                None => true,
            }
        });
        self.date_links.len() != before
    }

    #[allow(dead_code)]
    pub fn total_cards(&self) -> usize {
        self.grids.iter().map(|g| g.cards.len()).sum()
    }
}

pub fn models_by_provider() -> serde_json::Value {
    serde_json::json!({
        "claude": [
            {"id":"claude-opus-4-7","name":"Claude Opus 4.7"},
            {"id":"claude-opus-4-6","name":"Claude Opus 4.6"},
            {"id":"claude-sonnet-4-6","name":"Claude Sonnet 4.6"},
            {"id":"claude-opus-4-5-20251101","name":"Claude Opus 4.5"},
            {"id":"claude-sonnet-4-5-20250929","name":"Claude Sonnet 4.5"},
            {"id":"claude-haiku-4-5-20251001","name":"Claude Haiku 4.5"}
        ],
        "openai": [
            {"id":"gpt-5.4-pro","name":"GPT-5.4 Pro"},
            {"id":"gpt-5.4","name":"GPT-5.4"},
            {"id":"gpt-5.4-mini","name":"GPT-5.4 Mini"},
            {"id":"gpt-5.4-nano","name":"GPT-5.4 Nano"},
            {"id":"gpt-5.3-chat-latest","name":"GPT-5.3"},
            {"id":"gpt-5.2","name":"GPT-5.2"},
            {"id":"gpt-5.1","name":"GPT-5.1"},
            {"id":"gpt-5","name":"GPT-5"},
            {"id":"gpt-5-mini","name":"GPT-5 Mini"},
            {"id":"gpt-4o","name":"GPT-4o"}
        ],
        "google": [
            {"id":"gemini-3-pro-preview","name":"Gemini 3 Pro"},
            {"id":"gemini-3-flash-preview","name":"Gemini 3 Flash"},
            {"id":"gemini-2.5-pro","name":"Gemini 2.5 Pro"},
            {"id":"gemini-2.5-flash","name":"Gemini 2.5 Flash"},
            {"id":"gemini-2.5-flash-lite","name":"Gemini 2.5 Flash Lite"}
        ],
        "grok": [
            {"id":"grok-4-1-fast-reasoning","name":"Grok 4.1 Fast Reasoning"},
            {"id":"grok-4-1-fast-non-reasoning","name":"Grok 4.1 Fast"},
            {"id":"grok-4-fast-reasoning","name":"Grok 4 Fast Reasoning"},
            {"id":"grok-4-fast-non-reasoning","name":"Grok 4 Fast"},
            {"id":"grok-code-fast-1","name":"Grok Code Fast"},
            {"id":"grok-4-0709","name":"Grok 4"},
            {"id":"grok-3","name":"Grok 3"},
            {"id":"grok-3-mini","name":"Grok 3 Mini"}
        ],
        "mistral": [
            {"id":"mistral-large-latest","name":"Mistral Large"},
            {"id":"mistral-small-latest","name":"Mistral Small"}
        ],
        "openrouter": [
            {"id":"openai/gpt-5.4","name":"OpenAI GPT-5.4"},
            {"id":"anthropic/claude-opus-4.7","name":"Claude Opus 4.7"},
            {"id":"anthropic/claude-sonnet-4.6","name":"Claude Sonnet 4.6"},
            {"id":"google/gemini-3-pro-preview","name":"Gemini 3 Pro"},
            {"id":"x-ai/grok-4.1-fast","name":"Grok 4.1 Fast"},
            {"id":"mistralai/mistral-large","name":"Mistral Large"},
            {"id":"deepseek/deepseek-v4","name":"DeepSeek V4"},
            {"id":"nvidia/nemotron-3-ultra-550b-a55b","name":"NVIDIA Nemotron 3 Ultra"}
        ],
        "opencode-go": [
            {"id":"deepseek-v4.1-flash","name":"DeepSeek V4.1 Flash"},
            {"id":"deepseek-v4","name":"DeepSeek V4"},
            {"id":"gpt-5.4","name":"GPT-5.4"},
            {"id":"claude-sonnet-4-6","name":"Claude Sonnet 4.6"},
            {"id":"gemini-3.5-flash","name":"Gemini 3.5 Flash"},
            {"id":"grok-4.1-fast","name":"Grok 4.1 Fast"}
        ],
        "local": []
    })
}

pub fn themes() -> serde_json::Value {
    serde_json::json!([
        {"id":"corcho","name":"Corcho","bg":"#121417","surface":"#1b1e23","accent":"#d98e5c","text":"#ece7de","builtin":true},
        {"id":"trellis","name":"Trellis","bg":"#16171f","surface":"#1e1f2b","accent":"#7aa2f7","text":"#d7d9e6","builtin":true},
        {"id":"chalk","name":"Chalk","bg":"#f5f7fa","surface":"#ffffff","accent":"#3b6ea5","text":"#20242c","builtin":true,"light":true},
        {"id":"high-contrast","name":"High Contrast","bg":"#000000","surface":"#0a0a0a","accent":"#00ff00","text":"#ffffff","builtin":true},
        {"id":"rift","name":"Rift","bg":"#232830","surface":"#2c333d","accent":"#88c0d0","text":"#e3e8ee","builtin":true},
        {"id":"cinder","name":"Cinder","bg":"#1a1423","surface":"#241b31","accent":"#e08b7a","text":"#ece2e6","builtin":true},
        {"id":"tidepool","name":"Tidepool","bg":"#0a1628","surface":"#122238","accent":"#0891b2","text":"#dbe7f0","builtin":true},
        {"id":"saffron","name":"Saffron","bg":"#0a0a0a","surface":"#141414","accent":"#bfa450","text":"#ece7d8","builtin":true},
        {"id":"graphite","name":"Graphite","bg":"#0f0f0f","surface":"#191919","accent":"#f0f0f0","text":"#ededed","builtin":true},
        {"id":"obsidian","name":"Obsidian","bg":"#0a0a0a","surface":"#121212","accent":"#a78bfa","text":"#ece9f6","builtin":true},
        {"id":"monokai","name":"Monokai","bg":"#22221d","surface":"#2b2b25","accent":"#f92672","text":"#ecebdc","builtin":true},
        {"id":"alpine","name":"Alpine","bg":"#f8fafc","surface":"#ffffff","accent":"#3b82f6","text":"#1e293b","builtin":true,"light":true},
        {"id":"overcast","name":"Overcast","bg":"#f1f5f9","surface":"#ffffff","accent":"#5b6472","text":"#27303b","builtin":true,"light":true},
        {"id":"blueprint","name":"Blueprint","bg":"#eff6ff","surface":"#ffffff","accent":"#1d4ed8","text":"#1e2a45","builtin":true,"light":true},
        {"id":"solarized-dark","name":"Solarized Dark","bg":"#002b36","surface":"#073642","accent":"#2aa198","text":"#d6e2e3","builtin":true},
        {"id":"gruvbox","name":"Gruvbox","bg":"#282828","surface":"#35302a","accent":"#fe8019","text":"#ebdbb2","builtin":true},
        {"id":"paper","name":"Paper","bg":"#f7f4ee","surface":"#fdfbf7","accent":"#a0522d","text":"#1f1b16","builtin":true,"light":true},
        {"id":"custom","name":"Custom","bg":"#121214","surface":"#1b1b1f","accent":"#c9a86a","text":"#e8e8ea","builtin":false},
        {"id":"voltage","name":"Voltage","bg":"#0a0910","surface":"#141320","accent":"#b24aff","text":"#eae4f5","builtin":false},
        {"id":"vermilion","name":"Vermilion","bg":"#0b0608","surface":"#180d12","accent":"#ff2d2d","text":"#f4e3e3","builtin":false}
    ])
}

pub fn fonts() -> serde_json::Value {
    serde_json::json!([
        {"id":"inter","name":"Inter","family":"Inter"},
        {"id":"dm-sans","name":"DM Sans","family":"DM Sans"},
        {"id":"manrope","name":"Manrope","family":"Manrope"},
        {"id":"plus-jakarta-sans","name":"Plus Jakarta Sans","family":"Plus Jakarta Sans"},
        {"id":"ibm-plex-sans","name":"IBM Plex Sans","family":"IBM Plex Sans"},
        {"id":"roboto","name":"Roboto","family":"Roboto"},
        {"id":"system","name":"System UI","family":""},
        {"id":"jetbrains-mono","name":"JetBrains Mono","family":"JetBrains Mono"},
        {"id":"fira-code","name":"Fira Code","family":"Fira Code"},
        {"id":"source-code-pro","name":"Source Code Pro","family":"Source Code Pro"},
        {"id":"ibm-plex-mono","name":"IBM Plex Mono","family":"IBM Plex Mono"},
        {"id":"space-mono","name":"Space Mono","family":"Space Mono"},
        {"id":"ocr-a","name":"OCR-A","family":"OCR A Std"},
        {"id":"open-dyslexic","name":"OpenDyslexic","family":"OpenDyslexic"}
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_data_covers_every_bundled_icon() {
        let js = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/qml/IconData.js"))
            .unwrap();
        let dir = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icons")).unwrap();
        for entry in dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("svg") {
                continue;
            }
            let stem = path.file_stem().unwrap().to_string_lossy();
            assert!(
                js.contains(&format!("\"{stem}\"")),
                "missing icon data for {stem}"
            );
        }
    }

    #[test]
    fn migration_drops_aura_cards_and_adds_default_calendar() {
        let mut ws = Workspace::default();
        ws.calendars.clear();
        ws.grids[0].cards.push(Card::new("aura", 0.0, 0.0));
        ws.grids[0].cards.push(Card::new("note", 0.0, 0.0));
        assert!(ws.migrate());
        assert!(ws.grids[0].cards.iter().all(|card| card.kind != "aura"));
        assert_eq!(ws.calendars.len(), 1);
        assert_eq!(ws.version, SCHEMA_VERSION);
    }

    #[test]
    fn memory_settings_have_safe_defaults() {
        let settings = Settings::default();
        assert!(settings.web_shared_profile);
        assert_eq!(settings.web_sleep_minutes, 10);
        assert!(!settings.low_memory_mode);
    }

    #[test]
    fn whiteboard_pressure_defaults_on_for_old_workspaces() {
        assert!(Settings::default().whiteboard_pressure);
        // Settings written before the key existed must pick up the default.
        let old: Settings = serde_json::from_str("{}").unwrap();
        assert!(old.whiteboard_pressure);
    }

    #[test]
    fn canvas_long_press_menu_defaults_on_for_old_workspaces() {
        assert!(Settings::default().canvas_long_press_menu);
        let old: Settings = serde_json::from_str("{}").unwrap();
        assert!(old.canvas_long_press_menu);
    }

    #[test]
    fn default_workspace_has_one_grid() {
        let ws = Workspace::default();
        assert_eq!(ws.grids.len(), 1);
        assert_eq!(ws.grids[0].zoom, 1.0);
        assert_eq!(ws.active_grid_id.as_deref(), Some(ws.grids[0].id.as_str()));
    }

    #[test]
    fn card_roundtrips_with_flat_payload() {
        let mut card = Card::new("website", 12.0, 24.0);
        card.set("url", serde_json::json!("https://example.com"));
        let raw = serde_json::to_string(&card).unwrap();
        assert!(raw.contains("\"url\":\"https://example.com\""));
        assert!(raw.contains("\"type\":\"website\""));
        let back: Card = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.kind, "website");
        assert_eq!(back.str_field("url", ""), "https://example.com");
    }

    #[test]
    fn settings_use_camel_case_for_ai_keys() {
        let s = Settings::default();
        let v = serde_json::to_value(&s).unwrap();
        assert!(v.get("aiProvider").is_some());
        assert!(v.get("launch_at_startup").is_some());
    }

    #[test]
    fn themes_follow_system_by_default() {
        let s = Settings::default();
        assert_eq!(s.theme_mode, "system");
        assert_eq!(s.theme_light, "paper");
        assert_eq!(s.theme_dark, "corcho");
        let themes = themes();
        let paper = themes
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == "paper")
            .unwrap();
        assert_eq!(paper["light"], true);
    }
}

#[cfg(test)]
mod contrast_tests {
    use super::*;

    fn channel(value: u8) -> f64 {
        let c = value as f64 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    fn luminance(hex: &str) -> f64 {
        let h = hex.trim_start_matches('#');
        let r = u8::from_str_radix(&h[0..2], 16).unwrap();
        let g = u8::from_str_radix(&h[2..4], 16).unwrap();
        let b = u8::from_str_radix(&h[4..6], 16).unwrap();
        0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
    }

    fn contrast(a: &str, b: &str) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    fn blend(fg: &str, bg: &str, alpha: f64) -> String {
        let f: Vec<u8> = (0..3)
            .map(|i| u8::from_str_radix(&fg[1 + i * 2..3 + i * 2], 16).unwrap())
            .collect();
        let b: Vec<u8> = (0..3)
            .map(|i| u8::from_str_radix(&bg[1 + i * 2..3 + i * 2], 16).unwrap())
            .collect();
        let mix: Vec<u8> = (0..3)
            .map(|i| (f[i] as f64 * alpha + b[i] as f64 * (1.0 - alpha)).round() as u8)
            .collect();
        format!("#{:02x}{:02x}{:02x}", mix[0], mix[1], mix[2])
    }

    #[test]
    fn every_theme_meets_text_and_accent_contrast() {
        let value = themes();
        for theme in value.as_array().unwrap() {
            let id = theme["id"].as_str().unwrap();
            let bg = theme["bg"].as_str().unwrap();
            let surface = theme["surface"].as_str().unwrap();
            let text = theme["text"].as_str().unwrap();
            let accent = theme["accent"].as_str().unwrap();
            let light = theme["light"].as_bool().unwrap_or(false);
            let t2_alpha = if light { 0.80 } else { 0.86 };
            let muted_alpha = if light { 0.72 } else { 0.66 };

            assert!(contrast(text, bg) >= 4.5, "{id}: primary text");
            assert!(
                contrast(text, surface) >= 4.5,
                "{id}: primary text on surface"
            );
            assert!(
                contrast(&blend(text, bg, t2_alpha), bg) >= 4.5,
                "{id}: secondary text"
            );
            assert!(
                contrast(&blend(text, bg, muted_alpha), bg) >= 4.5,
                "{id}: muted text"
            );

            let dark_ink = "#171717";
            let light_ink = "#f7f4ec";
            let best = if contrast(accent, dark_ink) >= contrast(accent, light_ink) {
                contrast(accent, dark_ink)
            } else {
                contrast(accent, light_ink)
            };
            assert!(best >= 4.5, "{id}: accent ink");
            assert!(contrast(accent, bg) >= 3.0, "{id}: accent non-text");
        }
    }
}
