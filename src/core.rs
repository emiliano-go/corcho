use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::Result;
use serde_json::{json, Value};

use crate::history::History;
use crate::integrations::Adblock;
use crate::model::{CalendarDef, Card, DateLink, Folder, Grid, Settings, Workspace};
use crate::search;
use crate::storage;
use crate::theme;

const PAYLOAD_CAP: usize = 256 * 1024;

#[cfg(target_env = "gnu")]
fn trim_allocator() {
    static LAST: AtomicI64 = AtomicI64::new(0);
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let last = LAST.load(Ordering::Relaxed);
    if now - last < 30 {
        return;
    }
    if LAST
        .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        unsafe {
            libc::malloc_trim(0);
        }
    }
}

#[cfg(not(target_env = "gnu"))]
fn trim_allocator() {}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{cut}…")
}

pub struct Core {
    pub data_dir: PathBuf,
    pub ws: Workspace,
    pub history: History,
    pub adblock: Adblock,
    pub iroh: crate::irohsync::IrohSync,
    pub update: crate::updates::UpdateState,
    pub last_saved: Option<std::time::Instant>,
    pub last_file_mtime: Option<std::time::SystemTime>,
}

impl Core {
    /// Boot without any networking; used by headless MCP clients and tests.
    pub fn boot() -> Result<Self> {
        Self::boot_with(false)
    }

    /// Boot the GUI, which owns the Iroh endpoint.
    pub fn boot_gui() -> Result<Self> {
        Self::boot_with(true)
    }

    fn boot_with(networking: bool) -> Result<Self> {
        let default_dir = storage::default_data_dir();
        storage::ensure_dirs(&default_dir)?;
        let ws = storage::load_workspace(&default_dir)?;
        let iroh = if networking {
            crate::irohsync::IrohSync::start(default_dir.clone(), iroh_config(&ws.settings))
        } else {
            crate::irohsync::IrohSync::disabled()
        };
        let last_file_mtime = storage::workspace_path(&default_dir)
            .metadata()
            .and_then(|m| m.modified())
            .ok();

        let mut core = Self {
            data_dir: default_dir,
            history: History::new(ws.settings.undo_steps, ws.clone()),
            ws,
            adblock: Adblock::new(),
            iroh,
            update: crate::updates::UpdateState::default(),
            last_saved: None,
            last_file_mtime,
        };
        core.adblock.set_enabled(core.ws.settings.ad_blocker);
        core.adblock.load_lists(&core.data_dir);
        if networking {
            crate::gitbackup::start(core.data_dir.clone());
        }
        Ok(core)
    }

    pub fn fallback() -> Self {
        let ws = Workspace::default();
        Self {
            data_dir: storage::default_data_dir(),
            history: History::new(ws.settings.undo_steps, ws.clone()),
            ws,
            adblock: Adblock::new(),
            iroh: crate::irohsync::IrohSync::disabled(),
            update: crate::updates::UpdateState::default(),
            last_saved: None,
            last_file_mtime: None,
        }
    }

    pub fn save(&mut self) -> Result<()> {
        self.ws.revision = self.ws.revision.saturating_add(1);
        storage::save_workspace(&self.data_dir, &self.ws)?;
        self.last_saved = Some(std::time::Instant::now());
        self.last_file_mtime = storage::workspace_path(&self.data_dir)
            .metadata()
            .and_then(|m| m.modified())
            .ok();
        self.iroh.mark_dirty();
        trim_allocator();
        Ok(())
    }

    /// Persist a state that arrived from a peer. Does not mark it dirty, so it
    /// is not echoed back out as a local edit.
    pub fn save_remote(&mut self) -> Result<()> {
        self.ws.revision = self.ws.revision.saturating_add(1);
        storage::save_workspace(&self.data_dir, &self.ws)?;
        self.last_saved = Some(std::time::Instant::now());
        self.last_file_mtime = storage::workspace_path(&self.data_dir)
            .metadata()
            .and_then(|m| m.modified())
            .ok();
        Ok(())
    }

    /// Apply a batch of records from a peer and persist the result.
    pub fn apply_remote(&mut self, records: Vec<(String, crate::irohsync::Record)>) -> bool {
        if records.is_empty() {
            return false;
        }
        snapshot_before_remote(&self.data_dir, &self.ws);
        let local = crate::irohsync::split_workspace(&self.ws);
        let grid_names: std::collections::HashMap<String, String> = self
            .ws
            .grids
            .iter()
            .map(|grid| (grid.id.clone(), grid.name.clone()))
            .collect();
        let mut conflicts: Vec<(String, Value, Value)> = Vec::new();
        for (key, record) in &records {
            if record.deleted
                || !key.starts_with("card/")
                || key == "settings"
                || key.starts_with("sync/")
                || key.starts_with("attachment/")
            {
                continue;
            }
            let Some(remote) = &record.value else {
                continue;
            };
            let Some(value) = local.get(key) else {
                continue;
            };
            if value == remote {
                continue;
            }
            crate::irohsync::backup_conflict(&self.data_dir, key, value, remote);
            let grid_id = remote
                .get("grid")
                .and_then(|value| value.as_str())
                .or_else(|| value.get("grid").and_then(|value| value.as_str()))
                .unwrap_or_default()
                .to_string();
            if !grid_id.is_empty() {
                conflicts.push((grid_id, value.clone(), remote.clone()));
            }
        }
        if !conflicts.is_empty() {
            add_conflict_cards(&mut self.ws, &grid_names, &conflicts);
        }
        let changed = crate::irohsync::apply_records(&mut self.ws, &records);
        if changed || !conflicts.is_empty() {
            let _ = self.save_remote();
            self.iroh.mark_dirty();
        }
        changed || !conflicts.is_empty()
    }

    pub fn reload_from_disk(&mut self) -> Result<()> {
        self.ws = storage::load_workspace(&self.data_dir)?;
        self.history.reset(&self.ws);
        self.iroh.configure(iroh_config(&self.ws.settings));
        self.adblock.set_enabled(self.ws.settings.ad_blocker);
        self.last_file_mtime = storage::workspace_path(&self.data_dir)
            .metadata()
            .and_then(|m| m.modified())
            .ok();
        Ok(())
    }

    /// Reload when another process (typically the GUI or another MCP client)
    /// has written the workspace since this Core last loaded or saved it.
    pub fn reload_if_changed(&mut self) -> Result<bool> {
        let mtime = storage::workspace_path(&self.data_dir)
            .metadata()
            .and_then(|m| m.modified())
            .ok();
        if mtime.is_some() && mtime != self.last_file_mtime {
            self.reload_from_disk()?;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn iroh_config(&self) -> crate::irohsync::IrohConfig {
        iroh_config(&self.ws.settings)
    }

    pub fn save_quiet(&mut self) {
        if let Err(e) = self.save() {
            tracing::error!("save failed: {e}");
        }
    }

    /// Snapshot the pre-mutation state so undo can restore it.
    pub fn checkpoint(&mut self) {
        self.history.checkpoint(&self.ws);
    }

    pub fn undo(&mut self) -> bool {
        if let Some(ws) = self.history.undo(&self.ws) {
            self.ws = ws;
            self.save_quiet();
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self) -> bool {
        if let Some(ws) = self.history.redo() {
            self.ws = ws;
            self.save_quiet();
            true
        } else {
            false
        }
    }

    pub fn grids_json(&self) -> String {
        serde_json::to_string(&self.ws.grids).unwrap_or_else(|_| "[]".into())
    }

    pub fn cards_json(&self) -> String {
        let cards: Vec<Card> = self
            .ws
            .active_grid()
            .map(|g| g.cards.clone())
            .unwrap_or_default();
        serde_json::to_string(&cards).unwrap_or_else(|_| "[]".into())
    }

    pub fn all_cards_json(&self) -> String {
        let mut out = Vec::new();
        for grid in &self.ws.grids {
            for card in &grid.cards {
                let mut value = serde_json::to_value(card).unwrap_or_else(|_| json!({}));
                if let Some(obj) = value.as_object_mut() {
                    obj.insert("gridId".into(), json!(grid.id));
                    obj.insert("gridName".into(), json!(grid.name));
                }
                out.push(value);
            }
        }
        Value::Array(out).to_string()
    }

    pub fn folders_json(&self) -> String {
        serde_json::to_string(&self.ws.folders).unwrap_or_else(|_| "[]".into())
    }

    pub fn settings_json(&self) -> String {
        serde_json::to_string(&self.ws.settings).unwrap_or_else(|_| "{}".into())
    }

    pub fn themes_json(&self) -> String {
        theme::themes_json(&self.data_dir).to_string()
    }

    pub fn active_grid_id(&self) -> String {
        self.ws.active_grid_id.clone().unwrap_or_default()
    }

    pub fn active_grid_view(&self) -> (f64, f64, f64) {
        self.ws
            .active_grid()
            .map(|g| (g.pan_x, g.pan_y, g.zoom))
            .unwrap_or((0.0, 0.0, 1.0))
    }

    pub fn set_view(&mut self, pan_x: f64, pan_y: f64, zoom: f64) {
        if let Some(g) = self.ws.active_grid_mut() {
            g.pan_x = pan_x;
            g.pan_y = pan_y;
            g.zoom = zoom.clamp(0.2, 3.0);
        }
    }

    pub fn set_viewport(&mut self, width: f64, height: f64) {
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        if (self.ws.viewport_width - width).abs() > 0.5
            || (self.ws.viewport_height - height).abs() > 0.5
        {
            self.ws.viewport_width = width;
            self.ws.viewport_height = height;
            self.save_quiet();
        }
    }

    /// The world-space rectangle currently visible in the canvas.
    pub fn world_frame(
        &self,
        grid_id: Option<&str>,
        width: Option<f64>,
        height: Option<f64>,
    ) -> Option<(String, f64, f64, f64, f64)> {
        let grid = match grid_id {
            Some(id) => self.ws.grid(id)?,
            None => self.ws.active_grid()?,
        };
        let zoom = grid.zoom.clamp(0.2, 3.0);
        let w = width.filter(|v| *v > 0.0).unwrap_or(self.ws.viewport_width);
        let h = height
            .filter(|v| *v > 0.0)
            .unwrap_or(self.ws.viewport_height);
        if w <= 0.0 || h <= 0.0 {
            return None;
        }
        Some((
            grid.id.clone(),
            -grid.pan_x / zoom,
            -grid.pan_y / zoom,
            w / zoom,
            h / zoom,
        ))
    }

    pub fn switch_grid(&mut self, id: &str) -> bool {
        if self.ws.grid(id).is_some() {
            self.ws.active_grid_id = Some(id.to_string());
            self.save_quiet();
            true
        } else {
            false
        }
    }

    pub fn create_card(&mut self, kind: &str, x: f64, y: f64) -> Option<Card> {
        self.checkpoint();
        let mut card = Card::new(kind, x.round(), y.round());
        populate_defaults(&mut card, &self.ws);
        {
            let g = self.ws.active_grid_mut()?;
            g.cards.push(card.clone());
        }
        self.save_quiet();
        Some(card)
    }

    /// Create a note card holding text pulled from the clipboard. The first
    /// non-empty line doubles as the card title so the new card is identifiable
    /// in the header and in search results.
    pub fn create_text_card(&mut self, text: &str, x: f64, y: f64) -> Option<Card> {
        if text.trim().is_empty() {
            return None;
        }
        self.checkpoint();
        let mut card = Card::new("note", x.round(), y.round());
        populate_defaults(&mut card, &self.ws);
        card.text = text.to_string();
        card.title = text
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(|line| clip(line, 64))
            .unwrap_or_default();
        {
            let grid = self.ws.active_grid_mut()?;
            grid.cards.push(card.clone());
        }
        self.save_quiet();
        Some(card)
    }

    /// Persist a clipboard image in the attachments folder and create a file
    /// card that displays it.
    pub fn create_image_card(
        &mut self,
        png: &[u8],
        width: u32,
        height: u32,
        x: f64,
        y: f64,
    ) -> Result<Card> {
        storage::ensure_dirs(&self.data_dir)?;
        let name = unique_attachment_name(&self.data_dir, "Pasted image", "png");
        let path = storage::write_attachment(&self.data_dir, &name, png)?;
        self.checkpoint();
        let mut card = Card::new("file", x.round(), y.round());
        card.width = 420.0;
        card.height = if width > 0 {
            (420.0 * f64::from(height) / f64::from(width)).clamp(180.0, 720.0)
        } else {
            360.0
        };
        card.icon = "image".to_string();
        card.title = "Pasted image".to_string();
        card.set("path", json!(path.to_string_lossy()));
        card.set("fileKind", json!("image"));
        card.set("name", json!(name));
        {
            let grid = self
                .ws
                .active_grid_mut()
                .ok_or_else(|| anyhow::anyhow!("grid not found"))?;
            grid.cards.push(card.clone());
        }
        self.save_quiet();
        Ok(card)
    }

    /// Persist pasted SVG markup in the attachments folder and create a file
    /// card that displays it.
    pub fn create_svg_card(&mut self, markup: &str, x: f64, y: f64) -> Result<Card> {
        storage::ensure_dirs(&self.data_dir)?;
        let name = unique_attachment_name(&self.data_dir, "Pasted drawing", "svg");
        let path = storage::write_attachment(&self.data_dir, &name, markup.as_bytes())?;
        self.checkpoint();
        let mut card = Card::new("file", x.round(), y.round());
        card.width = 420.0;
        card.height = 360.0;
        card.icon = "image".to_string();
        card.title = "Pasted drawing".to_string();
        card.set("path", json!(path.to_string_lossy()));
        card.set("fileKind", json!("image"));
        card.set("name", json!(name));
        {
            let grid = self
                .ws
                .active_grid_mut()
                .ok_or_else(|| anyhow::anyhow!("grid not found"))?;
            grid.cards.push(card.clone());
        }
        self.save_quiet();
        Ok(card)
    }

    /// Build an SVG document for a whiteboard card so it can be copied to the
    /// clipboard.
    pub fn whiteboard_svg(&self, card_id: &str, width: f64, height: f64) -> Option<String> {
        let (_, card) = self.ws.card(card_id)?;
        if card.kind != "whiteboard" {
            return None;
        }
        let strokes = card
            .get("strokes")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        Some(crate::svg::strokes_to_svg(strokes, width, height))
    }

    /// Serialize a whiteboard card's raw stroke list for "Copy as Stroke".
    pub fn whiteboard_strokes_json(&self, card_id: &str) -> Option<String> {
        let (_, card) = self.ws.card(card_id)?;
        if card.kind != "whiteboard" {
            return None;
        }
        let strokes = card
            .get("strokes")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        Some(crate::svg::strokes_to_json(strokes))
    }

    pub fn update_card_json(&mut self, json_str: &str) -> bool {
        let Ok(value) = serde_json::from_str::<Value>(json_str) else {
            return false;
        };
        let Some(id) = value.get("id").and_then(|v| v.as_str()).map(str::to_string) else {
            return false;
        };
        self.checkpoint();
        let Some(card) = self.ws.card_mut(&id) else {
            return false;
        };
        if let Some(v) = value.get("x").and_then(|v| v.as_f64()) {
            card.x = v;
        }
        if let Some(v) = value.get("y").and_then(|v| v.as_f64()) {
            card.y = v;
        }
        if let Some(v) = value.get("width").and_then(|v| v.as_f64()) {
            card.width = v.max(80.0);
        }
        if let Some(v) = value.get("height").and_then(|v| v.as_f64()) {
            card.height = v.max(64.0);
        }
        if let Some(v) = value.get("title").and_then(|v| v.as_str()) {
            card.title = v.to_string();
        }
        if let Some(v) = value.get("text").and_then(|v| v.as_str()) {
            card.text = v.to_string();
        }
        if let Some(v) = value.get("color").and_then(|v| v.as_str()) {
            card.color = v.to_string();
        }
        if let Some(v) = value.get("icon").and_then(|v| v.as_str()) {
            card.icon = v.to_string();
        }
        if let Some(v) = value.get("locked").and_then(|v| v.as_bool()) {
            card.locked = v;
        }
        if let Some(obj) = value.get("payload").and_then(|v| v.as_object()) {
            for (k, v) in obj {
                if matches!(k.as_str(), "lastBody" | "lastResult") {
                    if let Some(text) = v.as_str() {
                        if text.len() > PAYLOAD_CAP {
                            let mut cut = PAYLOAD_CAP;
                            while !text.is_char_boundary(cut) {
                                cut -= 1;
                            }
                            card.set(k, json!(format!("{}\n… (truncated)", &text[..cut])));
                            continue;
                        }
                    }
                }
                card.set(k, v.clone());
            }
        }
        self.propagate_sync(&id);
        if !self.ws.date_links.is_empty() {
            self.ws.prune_date_links();
        }
        self.save_quiet();
        true
    }

    pub fn delete_card(&mut self, id: &str) -> bool {
        self.checkpoint();
        let sync_id = self
            .ws
            .card(id)
            .and_then(|(_, c)| c.get("syncId").and_then(|v| v.as_str()))
            .map(str::to_string);
        let removed = self.ws.remove_card(id).is_some();
        if let Some(sync_id) = sync_id {
            let partners: Vec<String> = self
                .ws
                .grids
                .iter()
                .flat_map(|g| g.cards.iter())
                .filter(|c| c.get("syncId").and_then(|v| v.as_str()) == Some(sync_id.as_str()))
                .map(|c| c.id.clone())
                .collect();
            for partner in partners {
                if let Some(card) = self.ws.card_mut(&partner) {
                    card.payload.remove("syncId");
                }
            }
        }
        if removed {
            self.ws.prune_date_links();
            self.save_quiet();
        }
        removed
    }

    pub fn duplicate_card(&mut self, id: &str) -> Option<Card> {
        let (grid_id, mut card) = {
            let (g, c) = self.ws.card(id)?;
            (g.id.clone(), c.clone())
        };
        self.checkpoint();
        card.id = uuid::Uuid::new_v4().to_string();
        card.x += 32.0;
        card.y += 32.0;
        if let Some(g) = self.ws.grid_mut(&grid_id) {
            g.cards.push(card.clone());
        }
        self.save_quiet();
        Some(card)
    }

    pub fn send_card_to_grid(&mut self, card_id: &str, grid_id: &str) -> bool {
        if self.ws.grid(grid_id).is_none() {
            return false;
        }
        let Some(card) = self.ws.remove_card(card_id) else {
            return false;
        };
        self.checkpoint();
        if let Some(g) = self.ws.grid_mut(grid_id) {
            g.cards.push(card);
            self.save_quiet();
            true
        } else {
            false
        }
    }

    pub fn create_grid(&mut self, name: &str) -> Grid {
        self.checkpoint();
        let g = self.ws.add_grid(name);
        self.ws.active_grid_id = Some(g.id.clone());
        self.save_quiet();
        g
    }

    pub fn set_grid_icon(&mut self, id: &str, icon: &str) -> bool {
        let icon = icon.trim();
        if icon.is_empty() {
            return false;
        }
        self.checkpoint();
        let Some(grid) = self.ws.grid_mut(id) else {
            return false;
        };
        grid.icon = icon.to_string();
        self.save_quiet();
        true
    }

    pub fn rename_grid(&mut self, id: &str, name: &str) -> bool {
        self.checkpoint();
        let ok = if let Some(g) = self.ws.grid_mut(id) {
            g.name = name.to_string();
            true
        } else {
            false
        };
        if ok {
            self.save_quiet();
        }
        ok
    }

    pub fn delete_grid(&mut self, id: &str) -> bool {
        if self.ws.grids.len() <= 1 {
            return false;
        }
        self.checkpoint();
        self.ws.grids.retain(|g| g.id != id);
        if self.ws.active_grid_id.as_deref() == Some(id) {
            self.ws.active_grid_id = self.ws.grids.first().map(|g| g.id.clone());
        }
        self.ws.prune_date_links();
        self.save_quiet();
        true
    }

    pub fn create_folder(&mut self, name: &str) -> Folder {
        self.checkpoint();
        let f = self.ws.add_folder(name);
        self.save_quiet();
        f
    }

    pub fn rename_folder(&mut self, id: &str, name: &str) -> bool {
        self.checkpoint();
        let ok = if let Some(f) = self.ws.folders.iter_mut().find(|f| f.id == id) {
            f.name = name.to_string();
            true
        } else {
            false
        };
        if ok {
            self.save_quiet();
        }
        ok
    }

    pub fn delete_folder(&mut self, id: &str, keep_content: bool) -> bool {
        self.checkpoint();
        if !keep_content {
            self.ws.grids.retain(|g| g.folder_id.as_deref() != Some(id));
        } else {
            for g in &mut self.ws.grids {
                if g.folder_id.as_deref() == Some(id) {
                    g.folder_id = None;
                }
            }
        }
        self.ws.folders.retain(|f| f.id != id);
        self.ws.prune_date_links();
        if self.ws.grids.is_empty() {
            self.ws = {
                let mut w = self.ws.clone();
                let g = w.add_grid("Grid A");
                w.active_grid_id = Some(g.id);
                w
            };
        }
        self.save_quiet();
        true
    }

    pub fn toggle_folder(&mut self, id: &str) {
        if let Some(f) = self.ws.folders.iter_mut().find(|f| f.id == id) {
            f.collapsed = !f.collapsed;
        }
        self.save_quiet();
    }

    pub fn move_grid_to_folder(&mut self, grid_id: &str, folder_id: Option<&str>) -> bool {
        self.checkpoint();
        let ok = if let Some(g) = self.ws.grid_mut(grid_id) {
            g.folder_id = folder_id.map(str::to_string);
            true
        } else {
            false
        };
        if ok {
            self.save_quiet();
        }
        ok
    }

    pub fn reorder_grid(&mut self, id: &str, before_id: Option<&str>) -> bool {
        let Some(pos) = self.ws.grids.iter().position(|g| g.id == id) else {
            return false;
        };
        self.checkpoint();
        let g = self.ws.grids.remove(pos);
        let insert = before_id
            .and_then(|b| self.ws.grids.iter().position(|x| x.id == b))
            .unwrap_or(self.ws.grids.len());
        self.ws.grids.insert(insert, g);
        self.save_quiet();
        true
    }

    pub fn set_setting_json(&mut self, key: &str, value_json: &str) -> bool {
        let Ok(value) = serde_json::from_str::<Value>(value_json) else {
            return false;
        };
        let known = serde_json::to_value(&self.ws.settings)
            .ok()
            .and_then(|v| v.as_object().map(|o| o.contains_key(key)))
            .unwrap_or(false);
        if !known {
            return false;
        }
        let mut extra = serde_json::Map::new();
        extra.insert(key.to_string(), value);
        let mut value = Value::Object(extra);
        merge_settings_value(&mut self.ws.settings, &mut value);
        if key == "undo_steps" {
            self.history.set_limit(self.ws.settings.undo_steps);
        }
        if key == "ad_blocker" {
            self.adblock.set_enabled(self.ws.settings.ad_blocker);
        }
        if key.starts_with("iroh_") {
            let config = self.iroh_config();
            self.iroh.configure(config);
        }
        self.save_quiet();
        true
    }

    pub fn search_json(&self, query: &str) -> String {
        serde_json::to_string(&search::search(&self.ws, query)).unwrap_or_else(|_| "[]".into())
    }

    pub fn calendar_json(&self) -> String {
        let links: Vec<Value> = self
            .ws
            .date_links
            .iter()
            .filter_map(|link| self.resolve_date_link(link))
            .collect();
        json!({
            "calendars": self.ws.calendars,
            "dateLinks": links
        })
        .to_string()
    }

    fn resolve_date_link(&self, link: &DateLink) -> Option<Value> {
        let mut title = link.title.clone();
        let mut source_kind = None;
        let mut done = false;
        if let Some(card_id) = link.source_card_id.as_deref() {
            let (_, card) = self.ws.card(card_id)?;
            source_kind = Some(card.kind.clone());
            if let Some(item_id) = link.source_item_id.as_deref() {
                let (item_title, item_done) = find_item(card, item_id)?;
                if !item_title.is_empty() {
                    title = item_title;
                }
                done = item_done;
            } else if !card.title.is_empty() {
                title = card.title.clone();
            }
        }
        Some(json!({
            "id": link.id,
            "calendarIds": link.calendar_ids,
            "date": link.date,
            "title": title,
            "sourceCardId": link.source_card_id,
            "sourceItemId": link.source_item_id,
            "sourceKind": source_kind,
            "done": done
        }))
    }

    pub fn calendar_agenda(&self, from: &str, to: &str, calendar_ids: &[String]) -> Vec<Value> {
        self.ws
            .date_links
            .iter()
            .filter(|link| link.date.as_str() >= from && link.date.as_str() <= to)
            .filter(|link| {
                calendar_ids.is_empty()
                    || link
                        .calendar_ids
                        .iter()
                        .any(|id| calendar_ids.iter().any(|wanted| wanted == id))
            })
            .filter_map(|link| self.resolve_date_link(link))
            .collect()
    }

    pub fn add_calendar(&mut self, name: &str, color: &str) -> CalendarDef {
        self.checkpoint();
        let calendar = CalendarDef::new(name, color);
        self.ws.calendars.push(calendar.clone());
        self.save_quiet();
        calendar
    }

    pub fn update_calendar(&mut self, id: &str, name: Option<&str>, color: Option<&str>) -> bool {
        if !self.ws.calendars.iter().any(|c| c.id == id) {
            return false;
        }
        self.checkpoint();
        let Some(calendar) = self.ws.calendars.iter_mut().find(|c| c.id == id) else {
            return false;
        };
        if let Some(name) = name {
            calendar.name = name.to_string();
        }
        if let Some(color) = color {
            calendar.color = color.to_string();
        }
        self.save_quiet();
        true
    }

    pub fn delete_calendar(&mut self, id: &str) -> bool {
        if self.ws.calendars.len() <= 1 {
            return false;
        }
        let Some(pos) = self.ws.calendars.iter().position(|c| c.id == id) else {
            return false;
        };
        self.checkpoint();
        self.ws.calendars.remove(pos);
        for link in &mut self.ws.date_links {
            link.calendar_ids.retain(|calendar_id| calendar_id != id);
        }
        self.ws
            .date_links
            .retain(|link| !link.calendar_ids.is_empty());
        for grid in &mut self.ws.grids {
            for card in &mut grid.cards {
                if let Some(ids) = card
                    .payload
                    .get_mut("calendarIds")
                    .and_then(|v| v.as_array_mut())
                {
                    ids.retain(|value| value.as_str() != Some(id));
                }
            }
        }
        self.save_quiet();
        true
    }

    pub fn add_date_link(
        &mut self,
        calendar_ids: Vec<String>,
        date: &str,
        title: &str,
        source_card_id: Option<String>,
        source_item_id: Option<String>,
    ) -> Result<DateLink> {
        if !valid_date(date) {
            return Err(anyhow::anyhow!("date must be YYYY-MM-DD"));
        }
        if calendar_ids.is_empty() {
            return Err(anyhow::anyhow!("at least one calendar is required"));
        }
        for id in &calendar_ids {
            if !self.ws.calendars.iter().any(|c| &c.id == id) {
                return Err(anyhow::anyhow!("calendar not found: {id}"));
            }
        }
        if let Some(card_id) = source_card_id.as_deref() {
            let (_, card) = self
                .ws
                .card(card_id)
                .ok_or_else(|| anyhow::anyhow!("card not found"))?;
            if let Some(item_id) = source_item_id.as_deref() {
                if find_item(card, item_id).is_none() {
                    return Err(anyhow::anyhow!("item not found"));
                }
            }
        }
        self.checkpoint();
        let mut link = DateLink::new(date, calendar_ids, title);
        link.source_card_id = source_card_id;
        link.source_item_id = source_item_id;
        self.ws.date_links.push(link.clone());
        self.save_quiet();
        Ok(link)
    }

    pub fn update_date_link(
        &mut self,
        id: &str,
        calendar_ids: Option<Vec<String>>,
        date: Option<&str>,
        title: Option<&str>,
    ) -> Result<bool> {
        if let Some(date) = date {
            if !valid_date(date) {
                return Err(anyhow::anyhow!("date must be YYYY-MM-DD"));
            }
        }
        if let Some(ids) = calendar_ids.as_ref() {
            if ids.is_empty() {
                return Err(anyhow::anyhow!("at least one calendar is required"));
            }
            for id in ids {
                if !self.ws.calendars.iter().any(|c| &c.id == id) {
                    return Err(anyhow::anyhow!("calendar not found: {id}"));
                }
            }
        }
        if !self.ws.date_links.iter().any(|l| l.id == id) {
            return Ok(false);
        }
        self.checkpoint();
        let Some(link) = self.ws.date_links.iter_mut().find(|l| l.id == id) else {
            return Ok(false);
        };
        if let Some(ids) = calendar_ids {
            link.calendar_ids = ids;
        }
        if let Some(date) = date {
            link.date = date.to_string();
        }
        if let Some(title) = title {
            link.title = title.to_string();
        }
        self.save_quiet();
        Ok(true)
    }

    pub fn delete_date_link(&mut self, id: &str) -> bool {
        if !self.ws.date_links.iter().any(|link| link.id == id) {
            return false;
        }
        self.checkpoint();
        self.ws.date_links.retain(|link| link.id != id);
        self.save_quiet();
        true
    }

    pub fn set_calendar_month(&mut self, card_id: &str, year: i64, month: i64) -> bool {
        if !(1970..=3000).contains(&year) || !(0..=11).contains(&month) {
            return false;
        }
        self.update_card_json(
            &json!({"id": card_id, "payload": {"viewYear": year, "viewMonth": month}}).to_string(),
        )
    }

    pub fn calendar_today(&mut self, card_id: &str) -> bool {
        let now =
            time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
        self.set_calendar_month(card_id, now.year() as i64, now.month() as i64 - 1)
    }

    pub fn export_backup(&mut self, dest: &str) -> Result<()> {
        self.save()?;
        storage::export_zip(&self.data_dir, &PathBuf::from(dest))
    }

    pub fn import_backup(&mut self, src: &str) -> Result<()> {
        let ws = storage::import_zip(&self.data_dir, &PathBuf::from(src))?;
        self.ws = ws;
        self.history.reset(&self.ws);
        self.adblock.set_enabled(self.ws.settings.ad_blocker);
        self.save()
    }

    pub fn import_legacy(&mut self, src: &str) -> Result<()> {
        let ws = storage::import_legacy(&PathBuf::from(src))?;
        self.ws = ws;
        self.history.reset(&self.ws);
        self.save()
    }

    pub fn set_data_dir(&mut self, dir: &str) -> Result<()> {
        self.data_dir = PathBuf::from(dir);
        storage::ensure_dirs(&self.data_dir)?;
        self.ws.settings.data_dir = Some(dir.to_string());
        self.save()
    }

    pub fn spawn(&mut self, kind: &str) -> Option<Card> {
        let n = self.ws.active_grid().map(|g| g.cards.len()).unwrap_or(0);
        let x = 64.0 + (n % 4) as f64 * 380.0;
        let y = 64.0 + (n / 4) as f64 * 330.0;
        self.create_card(kind, x, y)
    }

    pub fn command(&mut self, id: &str) -> Value {
        tracing::info!("launcher command: {id}");
        match id {
            "create-note" => self.spawn("note").map(card_json).unwrap_or(Value::Null),
            "create-website" => self.spawn("website").map(card_json).unwrap_or(Value::Null),
            "create-bookmark" => self.spawn("bookmark").map(card_json).unwrap_or(Value::Null),
            "create-api" => self.spawn("api").map(card_json).unwrap_or(Value::Null),
            "create-ai" => self.spawn("ai").map(card_json).unwrap_or(Value::Null),
            "create-script" => self.spawn("script").map(card_json).unwrap_or(Value::Null),
            "create-calendar" => self.spawn("calendar").map(card_json).unwrap_or(Value::Null),
            "create-timbre" => self.spawn("timbre").map(card_json).unwrap_or(Value::Null),
            "create-checklist" => self
                .spawn("checklist")
                .map(card_json)
                .unwrap_or(Value::Null),
            "create-kanban" => self.spawn("kanban").map(card_json).unwrap_or(Value::Null),
            "create-whiteboard" => self
                .spawn("whiteboard")
                .map(card_json)
                .unwrap_or(Value::Null),
            "create-pomodoro" => self.spawn("pomodoro").map(card_json).unwrap_or(Value::Null),
            "create-mpris" => self.spawn("mpris").map(card_json).unwrap_or(Value::Null),
            "create-grid" => json!({"grid": self.create_grid("New Grid")}),
            "create-folder" => json!({"folder": self.create_folder("New Folder")}),
            "undo" => json!({"ok": self.undo()}),
            "redo" => json!({"ok": self.redo()}),
            _ if id.starts_with("theme-") => {
                let theme = id.trim_start_matches("theme-");
                self.set_setting_json("theme", &serde_json::to_string(theme).unwrap());
                json!({"ok": true})
            }
            _ => json!({"ok": false, "error": "unknown command"}),
        }
    }

    pub fn reset_tutorials(&mut self) {
        let s = &mut self.ws.settings;
        s.onboarding_completed = false;
        s.card_tutorial_completed = false;
        s.context_tutorial_completed = false;
        s.launcher_tutorial_completed = false;
        self.save_quiet();
    }
}

pub fn card_json(card: Card) -> Value {
    serde_json::to_value(card).unwrap_or(Value::Null)
}

pub fn merge_settings_value(settings: &mut Settings, value: &mut Value) {
    let mut base = serde_json::to_value(&*settings).unwrap_or_else(|_| json!({}));
    if let (Some(base_obj), Some(extra)) = (base.as_object_mut(), value.as_object()) {
        for (k, v) in extra {
            base_obj.insert(k.clone(), v.clone());
        }
    }
    if let Ok(next) = serde_json::from_value::<Settings>(base) {
        *settings = next;
    }
}

fn file_kind(path: &std::path::Path) -> Option<&'static str> {
    if path.is_dir() {
        return Some("folder");
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "svg" => Some("image"),
        "md" | "markdown" => Some("markdown"),
        _ => None,
    }
}

fn unique_attachment_name(data_dir: &std::path::Path, base: &str, ext: &str) -> String {
    let dir = storage::attachments_dir(data_dir);
    let mut index = 1;
    loop {
        let name = if index == 1 {
            format!("{base}.{ext}")
        } else {
            format!("{base} {index}.{ext}")
        };
        if !dir.join(&name).exists() {
            return name;
        }
        index += 1;
    }
}

fn entry_kind(path: &std::path::Path) -> &'static str {
    if path.is_dir() {
        "folder"
    } else {
        file_kind(path).unwrap_or("other")
    }
}

fn file_icon(kind: &str) -> &'static str {
    match kind {
        "image" => "image",
        "folder" => "folder-plus",
        _ => "file-text",
    }
}

impl Core {
    pub fn pin_path(&mut self, path: &str, x: f64, y: f64, grid_id: Option<&str>) -> Result<Card> {
        let canonical = std::fs::canonicalize(path)
            .map_err(|err| anyhow::anyhow!("cannot open {path}: {err}"))?;
        let kind = file_kind(&canonical).ok_or_else(|| {
            anyhow::anyhow!("only image and Markdown files and folders can be pinned")
        })?;
        let name = canonical
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(path)
            .to_string();
        let (width, height) = match kind {
            "image" => (420.0, 360.0),
            "markdown" => (480.0, 420.0),
            _ => (420.0, 360.0),
        };
        self.checkpoint();
        let mut card = Card::new("file", x, y);
        card.width = width;
        card.height = height;
        card.icon = file_icon(kind).to_string();
        card.title = name.clone();
        card.set("path", json!(canonical.to_string_lossy()));
        card.set("fileKind", json!(kind));
        card.set("name", json!(name));
        let target = grid_id
            .map(str::to_string)
            .unwrap_or_else(|| self.ws.active_grid_id.clone().unwrap_or_default());
        let Some(grid) = self.ws.grid_mut(&target) else {
            return Err(anyhow::anyhow!("grid not found"));
        };
        grid.cards.push(card.clone());
        self.save_quiet();
        Ok(card)
    }

    pub fn move_cards(&mut self, ids: &[String], dx: f64, dy: f64) -> bool {
        if ids.is_empty() || (dx == 0.0 && dy == 0.0) {
            return false;
        }
        if !ids.iter().any(|id| {
            self.ws
                .card(id)
                .map(|(_, card)| !card.locked)
                .unwrap_or(false)
        }) {
            return false;
        }
        self.checkpoint();
        for id in ids {
            if let Some(card) = self.ws.card_mut(id) {
                if card.locked {
                    continue;
                }
                card.x += dx;
                card.y += dy;
            }
        }
        self.save_quiet();
        true
    }

    pub fn set_git_card_data(&mut self, card_id: &str, data: &Value) {
        if let Some(card) = self.ws.card_mut(card_id) {
            card.set("gitData", data.clone());
            self.save_quiet();
        }
    }

    pub fn sync_cards(&mut self, source_id: &str, target_id: &str) -> bool {
        if source_id == target_id {
            return false;
        }
        let Some((_, source)) = self.ws.card(source_id) else {
            return false;
        };
        let Some((_, target)) = self.ws.card(target_id) else {
            return false;
        };
        if source.kind != target.kind {
            return false;
        }
        let sync_id = uuid::Uuid::new_v4().to_string();
        if let Some(card) = self.ws.card_mut(source_id) {
            card.set("syncId", json!(sync_id));
        }
        if let Some(card) = self.ws.card_mut(target_id) {
            card.set("syncId", json!(sync_id));
        }
        self.copy_synced_content(source_id, target_id);
        self.save_quiet();
        true
    }

    pub fn unsync_card(&mut self, card_id: &str) -> bool {
        let Some((_, card)) = self.ws.card(card_id) else {
            return false;
        };
        let Some(sync_id) = card
            .get("syncId")
            .and_then(|v| v.as_str())
            .map(str::to_string)
        else {
            return false;
        };
        let ids: Vec<String> = self
            .ws
            .grids
            .iter()
            .flat_map(|g| g.cards.iter())
            .filter(|c| c.get("syncId").and_then(|v| v.as_str()) == Some(sync_id.as_str()))
            .map(|c| c.id.clone())
            .collect();
        for id in ids {
            if let Some(card) = self.ws.card_mut(&id) {
                card.payload.remove("syncId");
            }
        }
        self.save_quiet();
        true
    }

    fn copy_synced_content(&mut self, from_id: &str, to_id: &str) {
        let Some((_, source)) = self.ws.card(from_id) else {
            return;
        };
        let title = source.title.clone();
        let text = source.text.clone();
        let mut payload = source.payload.clone();
        payload.remove("linkedIds");
        if let Some(target) = self.ws.card_mut(to_id) {
            target.title = title;
            target.text = text;
            target.payload = payload;
        }
    }

    fn propagate_sync(&mut self, source_id: &str) {
        let Some(sync_id) = self
            .ws
            .card(source_id)
            .and_then(|(_, c)| c.get("syncId").and_then(|v| v.as_str()))
            .map(str::to_string)
        else {
            return;
        };
        let partners: Vec<String> = self
            .ws
            .grids
            .iter()
            .flat_map(|g| g.cards.iter())
            .filter(|c| {
                c.id != source_id
                    && c.get("syncId").and_then(|v| v.as_str()) == Some(sync_id.as_str())
            })
            .map(|c| c.id.clone())
            .collect();
        for id in partners {
            self.copy_synced_content(source_id, &id);
        }
    }

    pub fn toggle_ai_link(&mut self, ai_id: &str, card_id: &str) -> bool {
        if ai_id == card_id {
            return false;
        }
        let Some((_, ai)) = self.ws.card(ai_id) else {
            return false;
        };
        if ai.kind != "ai" || self.ws.card(card_id).is_none() {
            return false;
        }
        let mut ids: Vec<String> = ai
            .get("linkedIds")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(pos) = ids.iter().position(|id| id == card_id) {
            ids.remove(pos);
        } else {
            ids.push(card_id.to_string());
        }
        if let Some(card) = self.ws.card_mut(ai_id) {
            card.set("linkedIds", json!(ids));
        }
        self.save_quiet();
        true
    }

    pub fn ai_context(&self, ids: &[String]) -> String {
        let mut parts = Vec::new();
        for id in ids {
            let Some((_, card)) = self.ws.card(id) else {
                continue;
            };
            if card.kind == "ai" {
                continue;
            }
            let text = self.card_context(card);
            if !text.is_empty() {
                parts.push(text);
            }
        }
        parts.join("\n\n")
    }

    fn card_context(&self, card: &Card) -> String {
        let title = if card.title.is_empty() {
            card.kind.clone()
        } else {
            card.title.clone()
        };
        let body = match card.kind.as_str() {
            "note" => card.text.clone(),
            "website" | "bookmark" => card.str_field("url", ""),
            "api" => format!(
                "{}\n{}",
                card.str_field("apiUrl", ""),
                clip(&card.str_field("lastBody", ""), 2000)
            ),
            "script" => format!(
                "{}\n{}",
                clip(&card.str_field("script", ""), 1000),
                clip(&card.str_field("lastOutput", ""), 1000)
            ),
            "calendar" => {
                let cids: Vec<String> = card
                    .get("calendarIds")
                    .and_then(|v| v.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let dates: Vec<String> = self
                    .ws
                    .date_links
                    .iter()
                    .filter(|link| link.calendar_ids.iter().any(|id| cids.contains(id)))
                    .map(|link| format!("{} {}", link.date, link.title))
                    .collect();
                dates.join("\n")
            }
            "checklist" => card
                .get("items")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .map(|item| {
                            let done = item.get("done").and_then(|v| v.as_bool()).unwrap_or(false);
                            let text = item.get("text").and_then(|v| v.as_str()).unwrap_or("");
                            format!("[{}] {}", if done { "x" } else { " " }, text)
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default(),
            "kanban" => card
                .get("columns")
                .and_then(|v| v.as_array())
                .map(|columns| {
                    columns
                        .iter()
                        .map(|column| {
                            let name = column.get("title").and_then(|v| v.as_str()).unwrap_or("");
                            let items = column
                                .get("items")
                                .and_then(|v| v.as_array())
                                .map(|items| {
                                    items
                                        .iter()
                                        .map(|item| {
                                            let done = item
                                                .get("done")
                                                .and_then(|v| v.as_bool())
                                                .unwrap_or(false);
                                            let text = item
                                                .get("text")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("");
                                            format!("  [{}] {}", if done { "x" } else { " " }, text)
                                        })
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                })
                                .unwrap_or_default();
                            format!("{name}\n{items}")
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default(),
            "whiteboard" => format!(
                "{} strokes",
                card.get("strokes")
                    .and_then(|v| v.as_array())
                    .map(|v| v.len())
                    .unwrap_or(0)
            ),
            "pomodoro" => format!(
                "{} phase, {}s left, {} sessions ({}m work / {}m break)",
                card.str_field("phase", "work"),
                card.get("remaining").and_then(|v| v.as_i64()).unwrap_or(0),
                card.get("sessions").and_then(|v| v.as_i64()).unwrap_or(0),
                card.get("workMinutes")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(25),
                card.get("breakMinutes")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(5)
            ),
            "timbre" => format!(
                "tone {} Hz, mix {}, volume {}",
                card.get("toneFreq")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(220.0),
                card.get("toneMix").and_then(|v| v.as_f64()).unwrap_or(0.4),
                card.get("volume").and_then(|v| v.as_f64()).unwrap_or(0.35)
            ),
            "portal" => self
                .ws
                .grids
                .iter()
                .find(|grid| grid.id == card.str_field("targetGridId", ""))
                .map(|grid| format!("target grid: {}", grid.name))
                .unwrap_or_default(),
            "mpris" => format!("media player: {}", card.str_field("playerName", "")),
            "file" => format!(
                "{} file: {}",
                card.str_field("fileKind", ""),
                card.str_field("path", "")
            ),
            _ => card.text.clone(),
        };
        let body = body.trim();
        if body.is_empty() {
            format!("{title} ({})", card.kind)
        } else {
            format!("{title} ({}):\n{body}", card.kind)
        }
    }

    pub fn set_cards_group(&mut self, ids: &[String], group: &str) -> bool {
        self.checkpoint();
        for id in ids {
            if let Some(card) = self.ws.card_mut(id) {
                if group.is_empty() {
                    card.payload.remove("group");
                } else {
                    card.set("group", json!(group));
                }
            }
        }
        self.save_quiet();
        true
    }

    pub fn create_template(
        &mut self,
        name: &str,
        ids: &[String],
    ) -> Result<crate::model::Template> {
        let mut cards = Vec::new();
        for id in ids {
            if let Some((_, card)) = self.ws.card(id) {
                cards.push(card.clone());
            }
        }
        if cards.is_empty() {
            return Err(anyhow::anyhow!("select at least one card"));
        }
        let min_x = cards.iter().map(|c| c.x).fold(f64::INFINITY, f64::min);
        let min_y = cards.iter().map(|c| c.y).fold(f64::INFINITY, f64::min);
        for card in &mut cards {
            card.x -= min_x;
            card.y -= min_y;
        }
        let template = crate::model::Template {
            id: uuid::Uuid::new_v4().to_string(),
            name: if name.trim().is_empty() {
                "Template".into()
            } else {
                name.trim().to_string()
            },
            cards,
        };
        self.ws.templates.push(template.clone());
        self.save_quiet();
        Ok(template)
    }

    pub fn insert_template(
        &mut self,
        id: &str,
        x: f64,
        y: f64,
        grid_id: Option<&str>,
    ) -> Result<Vec<Card>> {
        let template = self
            .ws
            .templates
            .iter()
            .find(|t| t.id == id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("template not found"))?;
        let target = grid_id
            .map(str::to_string)
            .unwrap_or_else(|| self.ws.active_grid_id.clone().unwrap_or_default());
        let Some(grid) = self.ws.grid_mut(&target) else {
            return Err(anyhow::anyhow!("grid not found"));
        };
        let mut inserted = Vec::new();
        for source in template.cards {
            let mut card = source;
            card.id = uuid::Uuid::new_v4().to_string();
            card.x += x;
            card.y += y;
            grid.cards.push(card.clone());
            inserted.push(card);
        }
        self.save_quiet();
        Ok(inserted)
    }

    pub fn delete_template(&mut self, id: &str) -> bool {
        let before = self.ws.templates.len();
        self.ws.templates.retain(|t| t.id != id);
        let changed = before != self.ws.templates.len();
        if changed {
            self.save_quiet();
        }
        changed
    }

    pub fn read_pinned_file(&self, path: &str) -> Result<String> {
        let canonical = std::fs::canonicalize(path)?;
        let ext = canonical
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext != "md" && ext != "markdown" {
            return Err(anyhow::anyhow!("only Markdown files can be read"));
        }
        let meta = std::fs::metadata(&canonical)?;
        if meta.len() > 2 * 1024 * 1024 {
            return Err(anyhow::anyhow!("Markdown file is too large"));
        }
        Ok(std::fs::read_to_string(canonical)?)
    }

    pub fn list_pinned_folder(&self, path: &str) -> Result<Value> {
        let canonical = std::fs::canonicalize(path)?;
        if !canonical.is_dir() {
            return Err(anyhow::anyhow!("not a folder"));
        }
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&canonical)?.flatten() {
            let entry_path = entry.path();
            let kind = entry_kind(&entry_path);
            let name = entry.file_name().to_string_lossy().to_string();
            entries.push(json!({
                "name": name,
                "path": entry_path.to_string_lossy(),
                "kind": kind,
                "isDir": entry_path.is_dir()
            }));
            if entries.len() >= 200 {
                break;
            }
        }
        entries.sort_by(|a, b| {
            let ad = a["isDir"].as_bool().unwrap_or(false);
            let bd = b["isDir"].as_bool().unwrap_or(false);
            bd.cmp(&ad).then_with(|| {
                a["name"]
                    .as_str()
                    .unwrap_or("")
                    .to_lowercase()
                    .cmp(&b["name"].as_str().unwrap_or("").to_lowercase())
            })
        });
        Ok(json!({
            "path": canonical.to_string_lossy(),
            "name": canonical.file_name().and_then(|n| n.to_str()).unwrap_or("Folder"),
            "entries": entries
        }))
    }
}

fn populate_defaults(card: &mut Card, ws: &Workspace) {
    match card.kind.as_str() {
        "website" => {
            card.set("url", json!(""));
            card.set("websiteMode", json!("webview"));
        }
        "bookmark" => {
            card.set("url", json!(""));
            card.set("websiteMode", json!("bookmark"));
        }
        "api" => {
            card.set("apiUrl", json!(""));
            card.set("method", json!("GET"));
            card.set("fields", json!([]));
            card.set("refreshSeconds", json!(0));
            card.set("lastBody", json!(""));
        }
        "ai" => {
            card.set("prompt", json!(""));
            card.set("provider", json!(ws.settings.ai_provider));
            card.set("model", json!(""));
            card.set("lastResult", json!(""));
            card.set("webSearch", json!(ws.settings.ai_web_search));
        }
        "script" => {
            card.set("script", json!(""));
            card.set("lastOutput", json!(""));
        }
        "calendar" => {
            card.set("calendarIds", json!([]));
        }
        "timbre" => {
            card.set("volume", json!(0.35));
            card.set("toneFreq", json!(220.0));
            card.set("toneMix", json!(0.4));
        }
        "portal" => {
            card.set("targetGridId", json!(""));
        }
        "checklist" => {
            card.set("items", json!([]));
        }
        "kanban" => {
            card.set(
                "columns",
                json!([
                    {"id":"todo","title":"To do","items":[]},
                    {"id":"doing","title":"In progress","items":[]},
                    {"id":"done","title":"Done","items":[]}
                ]),
            );
        }
        "whiteboard" => {
            card.set("strokes", json!([]));
            card.set("penColor", json!("#e8e8ea"));
            card.set("penWidth", json!(3));
        }
        "pomodoro" => {
            card.set("workMinutes", json!(25));
            card.set("breakMinutes", json!(5));
            card.set("sessions", json!(0));
            card.set("phase", json!("work"));
            card.set("remaining", json!(25 * 60));
        }
        "mpris" => {
            card.set("playerName", json!(""));
        }
        _ => {}
    }
}

#[allow(dead_code)]
fn find_item(card: &Card, item_id: &str) -> Option<(String, bool)> {
    if let Some(items) = card.payload.get("items").and_then(|v| v.as_array()) {
        for item in items {
            if item.get("id").and_then(|v| v.as_str()) == Some(item_id) {
                let text = item
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let done = item.get("done").and_then(|v| v.as_bool()).unwrap_or(false);
                return Some((text, done));
            }
        }
    }
    if let Some(columns) = card.payload.get("columns").and_then(|v| v.as_array()) {
        for column in columns {
            let column_done = column.get("id").and_then(|v| v.as_str()) == Some("done");
            if let Some(items) = column.get("items").and_then(|v| v.as_array()) {
                for item in items {
                    if item.get("id").and_then(|v| v.as_str()) == Some(item_id) {
                        let text = item
                            .get("text")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        return Some((text, column_done));
                    }
                }
            }
        }
    }
    None
}

pub fn valid_date(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();
    if parts.len() != 3 || parts[0].len() != 4 || parts[1].len() != 2 || parts[2].len() != 2 {
        return false;
    }
    let (Ok(year), Ok(month), Ok(day)) = (
        parts[0].parse::<i32>(),
        parts[1].parse::<u8>(),
        parts[2].parse::<u8>(),
    ) else {
        return false;
    };
    let Ok(month) = time::Month::try_from(month) else {
        return false;
    };
    time::Date::from_calendar_date(year, month, day).is_ok()
}

pub fn iroh_config(settings: &crate::model::Settings) -> crate::irohsync::IrohConfig {
    crate::irohsync::IrohConfig {
        enabled: settings.iroh_sync_enabled,
        include_attachments: settings.iroh_include_attachments,
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        self.iroh.shutdown();
    }
}

fn snapshot_before_remote(data_dir: &std::path::Path, ws: &Workspace) {
    let dir = storage::backups_dir(data_dir);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let stamp = time::OffsetDateTime::now_utc().unix_timestamp_nanos();
    let path = dir.join(format!("iroh-before-remote-{stamp}.json"));
    if let Ok(bytes) = serde_json::to_vec_pretty(ws) {
        let _ = std::fs::write(&path, bytes);
    }
    let mut snapshots: Vec<_> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("iroh-before-remote-")
        })
        .collect();
    snapshots.sort_by_key(|entry| entry.metadata().and_then(|meta| meta.modified()).ok());
    while snapshots.len() > 20 {
        if let Some(entry) = snapshots.first() {
            let _ = std::fs::remove_file(entry.path());
        }
        snapshots.remove(0);
    }
}

fn add_conflict_cards(
    ws: &mut Workspace,
    grid_names: &std::collections::HashMap<String, String>,
    conflicts: &[(String, Value, Value)],
) {
    for (grid_id, local, remote) in conflicts {
        let name = grid_names
            .get(grid_id)
            .cloned()
            .unwrap_or_else(|| "Conflicts".into());
        let conflict_name = if name.ends_with("-conflict") {
            name
        } else {
            format!("{name}-conflict")
        };
        let conflict_id = format!("{grid_id}-conflict");
        let index = match ws
            .grids
            .iter()
            .position(|grid| grid.id == conflict_id || grid.name == conflict_name)
        {
            Some(index) => index,
            None => {
                ws.grids.push(Grid {
                    id: conflict_id,
                    name: conflict_name,
                    icon: "layout-grid".into(),
                    ..Default::default()
                });
                ws.grids.len() - 1
            }
        };
        let grid = &mut ws.grids[index];
        let mut slot = grid.cards.len();
        for (value, label) in [(local, "local"), (remote, "remote")] {
            let Some(card_value) = value.get("card") else {
                continue;
            };
            let Ok(mut card) = serde_json::from_value::<Card>(card_value.clone()) else {
                continue;
            };
            card.id = uuid::Uuid::new_v4().to_string();
            card.x = 64.0 + (slot % 3) as f64 * (card.width + 32.0);
            card.y = 64.0 + (slot / 3) as f64 * (card.height + 32.0);
            slot += 1;
            let title = if card.title.is_empty() {
                card.kind.clone()
            } else {
                card.title.clone()
            };
            card.title = format!("{title} ({label})");
            grid.cards.push(card);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflicting_cards_are_copied_into_a_conflict_grid() {
        let mut ws = crate::model::Workspace::default();
        let grid_id = ws.grids[0].id.clone();
        let mut names = std::collections::HashMap::new();
        names.insert(grid_id.clone(), ws.grids[0].name.clone());
        let mut local = Card::new("note", 10.0, 10.0);
        local.title = "local".into();
        let mut remote = local.clone();
        remote.id = uuid::Uuid::new_v4().to_string();
        remote.title = "remote".into();
        let conflicts = vec![(
            grid_id,
            json!({"grid": ws.grids[0].id, "card": local}),
            json!({"grid": ws.grids[0].id, "card": remote}),
        )];
        add_conflict_cards(&mut ws, &names, &conflicts);
        let conflict = ws
            .grids
            .iter()
            .find(|grid| grid.name.ends_with("-conflict"))
            .expect("conflict grid");
        assert_eq!(conflict.cards.len(), 2);
    }

    #[test]
    fn folder_listing_includes_unsupported_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("notes.md"), "# hi").unwrap();
        std::fs::write(dir.path().join("script.py"), "print(1)").unwrap();
        std::fs::write(dir.path().join("image.png"), b"not really a png").unwrap();

        let core = Core::fallback();
        let listing = core
            .list_pinned_folder(dir.path().to_str().unwrap())
            .unwrap();
        let entries = listing["entries"].as_array().unwrap();
        let kinds: Vec<(&str, &str)> = entries
            .iter()
            .map(|e| {
                (
                    e["name"].as_str().unwrap_or(""),
                    e["kind"].as_str().unwrap_or(""),
                )
            })
            .collect();
        assert!(kinds.contains(&("sub", "folder")));
        assert!(kinds.contains(&("notes.md", "markdown")));
        assert!(kinds.contains(&("image.png", "image")));
        assert!(kinds.contains(&("script.py", "other")));
    }

    #[test]
    fn synced_cards_mirror_content() {
        let mut core = Core::fallback();
        let a = core.create_card("note", 0.0, 0.0).unwrap();
        let b = core.create_card("note", 400.0, 0.0).unwrap();
        assert!(core.sync_cards(&a.id, &b.id));
        core.update_card_json(&format!(
            "{{\"id\":\"{}\",\"text\":\"shared text\",\"title\":\"Shared\"}}",
            a.id
        ));
        let (_, other) = core.ws.card(&b.id).unwrap();
        assert_eq!(other.text, "shared text");
        assert_eq!(other.title, "Shared");
        assert!(core.unsync_card(&a.id));
        let (_, other) = core.ws.card(&b.id).unwrap();
        assert!(other.get("syncId").is_none());
    }

    #[test]
    fn ai_links_build_context_and_toggle() {
        let mut core = Core::fallback();
        let ai = core.create_card("ai", 0.0, 0.0).unwrap();
        let note = core.create_card("note", 100.0, 0.0).unwrap();
        assert!(core.toggle_ai_link(&ai.id, &note.id));
        let (_, ai_now) = core.ws.card(&ai.id).unwrap();
        assert_eq!(ai_now.get("linkedIds").unwrap(), &json!([note.id]));

        core.update_card_json(&format!(
            "{{\"id\":\"{}\",\"text\":\"remember the milk\"}}",
            note.id
        ));
        let context = core.ai_context(std::slice::from_ref(&note.id));
        assert!(context.contains("remember the milk"));

        assert!(core.toggle_ai_link(&ai.id, &note.id));
        let (_, ai_now) = core.ws.card(&ai.id).unwrap();
        assert!(ai_now
            .get("linkedIds")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn large_payloads_are_truncated() {
        let mut core = Core::fallback();
        let card = core.create_card("api", 0.0, 0.0).unwrap();
        let body = "x".repeat(PAYLOAD_CAP + 1024);
        let patch = serde_json::json!({"id": card.id, "payload": {"lastBody": body}});
        assert!(core.update_card_json(&patch.to_string()));
        let (_, stored) = core.ws.card(&card.id).unwrap();
        let text = stored.str_field("lastBody", "");
        assert!(text.len() <= PAYLOAD_CAP + 32);
        assert!(text.ends_with("(truncated)"));
    }

    #[test]
    fn move_cards_skips_pinned_cards() {
        let mut core = Core::fallback();
        let pinned = core.create_card("note", 10.0, 10.0).unwrap();
        let loose = core.create_card("note", 100.0, 100.0).unwrap();
        core.update_card_json(&format!("{{\"id\":\"{}\",\"locked\":true}}", pinned.id));

        assert!(core.move_cards(&[pinned.id.clone(), loose.id.clone()], 10.0, 5.0));

        let (_, pinned_now) = core.ws.card(&pinned.id).unwrap();
        assert_eq!((pinned_now.x, pinned_now.y), (10.0, 10.0));
        let (_, loose_now) = core.ws.card(&loose.id).unwrap();
        assert_eq!((loose_now.x, loose_now.y), (110.0, 105.0));
    }

    #[test]
    fn pinning_rejects_unsupported_files_but_accepts_folders() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("data")).unwrap();
        let folder = dir.path().join("sub");
        std::fs::create_dir(&folder).unwrap();
        let script = dir.path().join("script.py");
        std::fs::write(&script, "print(1)").unwrap();
        let markdown = dir.path().join("notes.md");
        std::fs::write(&markdown, "# hi").unwrap();

        let mut core = Core::fallback();
        core.data_dir = dir.path().join("data");
        assert!(core
            .pin_path(script.to_str().unwrap(), 0.0, 0.0, None)
            .is_err());
        assert_eq!(
            core.pin_path(folder.to_str().unwrap(), 0.0, 0.0, None)
                .unwrap()
                .str_field("fileKind", ""),
            "folder"
        );
        assert_eq!(
            core.pin_path(markdown.to_str().unwrap(), 0.0, 0.0, None)
                .unwrap()
                .str_field("fileKind", ""),
            "markdown"
        );
    }

    #[test]
    fn pasted_text_creates_a_note_card() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = Core::fallback();
        core.data_dir = dir.path().to_path_buf();

        let card = core.create_text_card("hello\nworld", 10.4, 20.6).unwrap();
        assert_eq!(card.kind, "note");
        assert_eq!(card.text, "hello\nworld");
        assert_eq!(card.title, "hello");
        assert_eq!((card.x, card.y), (10.0, 21.0));
        assert!(core.ws.card(&card.id).is_some());

        assert!(core.create_text_card("   \n", 0.0, 0.0).is_none());
    }

    #[test]
    fn pasted_image_creates_a_file_card_and_attachment() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = Core::fallback();
        core.data_dir = dir.path().to_path_buf();

        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        let card = core.create_image_card(&png, 800, 600, 0.0, 0.0).unwrap();
        assert_eq!(card.kind, "file");
        assert_eq!(card.str_field("fileKind", ""), "image");
        assert_eq!(card.str_field("name", ""), "Pasted image.png");
        assert_eq!(card.title, "Pasted image");
        assert_eq!((card.width, card.height), (420.0, 315.0));
        let path = card.str_field("path", "");
        assert_eq!(std::fs::read(&path).unwrap(), png);

        let second = core.create_image_card(&png, 800, 600, 0.0, 0.0).unwrap();
        assert_eq!(second.str_field("name", ""), "Pasted image 2.png");
    }

    #[test]
    fn pasted_svg_creates_an_image_card() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = Core::fallback();
        core.data_dir = dir.path().to_path_buf();

        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10"/></svg>"#;
        let card = core.create_svg_card(svg, 1.2, 2.8).unwrap();
        assert_eq!(card.kind, "file");
        assert_eq!(card.str_field("fileKind", ""), "image");
        assert_eq!(card.str_field("name", ""), "Pasted drawing.svg");
        assert_eq!(card.title, "Pasted drawing");
        assert_eq!((card.x, card.y), (1.0, 3.0));
        let path = card.str_field("path", "");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), svg);

        let second = core.create_svg_card(svg, 0.0, 0.0).unwrap();
        assert_eq!(second.str_field("name", ""), "Pasted drawing 2.svg");
    }

    #[test]
    fn whiteboard_exports_svg_and_stroke_json() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = Core::fallback();
        core.data_dir = dir.path().to_path_buf();
        let card = core.create_card("whiteboard", 0.0, 0.0).unwrap();
        let strokes = json!([{
            "color": "#d98e5c",
            "width": 3,
            "points": [{"x": 4, "y": 5}, {"x": 40, "y": 50}]
        }]);
        assert!(core.update_card_json(
            &json!({"id": card.id, "payload": {"strokes": strokes}}).to_string()
        ));

        let svg = core.whiteboard_svg(&card.id, 300.0, 200.0).unwrap();
        assert!(svg.contains("viewBox=\"0 0 300 200\""));
        assert!(svg.contains("d=\"M 4 5 L 40 50\""));
        assert!(svg.contains("stroke=\"#d98e5c\""));

        let raw = core.whiteboard_strokes_json(&card.id).unwrap();
        let parsed: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed, strokes);

        assert!(core.whiteboard_svg("missing", 100.0, 100.0).is_none());
        assert!(core.whiteboard_strokes_json("missing").is_none());
    }

    #[test]
    fn whiteboard_stroke_changes_are_undoable() {
        let mut core = Core::fallback();
        let card = core.create_card("whiteboard", 0.0, 0.0).unwrap();
        let stroke = json!({
            "color": "#ffffff",
            "width": 3,
            "points": [{"x": 1, "y": 1}, {"x": 20, "y": 20}]
        });
        assert!(core.update_card_json(
            &json!({
                "id": card.id,
                "payload": {"strokes": [stroke], "penColor": "#ffffff", "penWidth": 3}
            })
            .to_string()
        ));

        fn stroke_count(core: &Core, id: &str) -> usize {
            core.ws
                .card(id)
                .and_then(|(_, c)| c.get("strokes"))
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0)
        }

        assert_eq!(stroke_count(&core, &card.id), 1);

        // Erasing removes every stroke in one checkpoint.
        assert!(
            core.update_card_json(&json!({"id": card.id, "payload": {"strokes": []}}).to_string())
        );
        assert_eq!(stroke_count(&core, &card.id), 0);

        assert!(core.undo());
        assert_eq!(stroke_count(&core, &card.id), 1);

        assert!(core.redo());
        assert_eq!(stroke_count(&core, &card.id), 0);
    }
}
