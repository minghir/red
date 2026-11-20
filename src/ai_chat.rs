use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use eframe::egui::{self, Color32, FontId, RichText, Stroke};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    User,
    Assistant,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatBackendMode {
    CortexServer,
    OpenAiCompatible,
}

#[derive(Debug, Clone)]
pub struct TokenUsageInfo {
    pub prompt_tokens: Option<u64>,
    pub candidates_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

impl TokenUsageInfo {
    fn from_json(v: &Value) -> Option<Self> {
        let u = v.get("usage")?;
        if u.is_null() {
            return None;
        }
        let prompt = u.get("prompt_tokens").and_then(|x| x.as_u64());
        let cand = u.get("candidates_tokens").and_then(|x| x.as_u64());
        let total = u.get("total_tokens").and_then(|x| x.as_u64());
        if prompt.is_none() && cand.is_none() && total.is_none() {
            return None;
        }
        Some(Self {
            prompt_tokens: prompt,
            candidates_tokens: cand,
            total_tokens: total,
        })
    }

    fn format_short(&self) -> String {
        format!(
            "tokeni: prompt={}, răspuns={}, total={}",
            self.prompt_tokens.unwrap_or(0),
            self.candidates_tokens.unwrap_or(0),
            self.total_tokens.unwrap_or(0),
        )
    }
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
    pub attached_file: Option<String>,
    pub mentioned_files: Vec<String>,
    pub usage: Option<TokenUsageInfo>,
}

#[derive(Clone)]
pub struct ActiveFileContext {
    pub file_name: String,
    pub relative_path: String,
    pub extension: Option<String>,
    pub content: String,
}

pub enum AiAction {
    None,
    InsertCode(String),
    ReplaceFile(String),
}

#[derive(Clone)]
struct CortexProjectItem {
    id: i64,
    name: String,
    project_type: String,
}

#[derive(Clone)]
struct AiConfig {
    mode: ChatBackendMode,
    // Setări Cortex HTTP Server
    cortex_url: String,
    cortex_token: String,
    cortex_provider: String, // "google" sau "ollama"
    // Setări fallback OpenAI / Ollama direct
    api_url: String,
    model: String,
    api_key: String,
}

impl Default for AiConfig {
    fn default() -> Self {
        let cortex_url = std::env::var("CORTEX_URL")
            .unwrap_or_else(|_| "http://localhost:7878".to_string());
        let cortex_token = std::env::var("CORTEX_API_KEY").unwrap_or_default();
        let cortex_provider = std::env::var("CORTEX_PROVIDER")
            .unwrap_or_else(|_| "google".to_string());

        let env_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
        let (api_url, model, api_key) = if env_key.is_empty() {
            (
                "http://localhost:11434/v1/chat/completions".to_string(),
                "qwen2.5-coder:7b".to_string(),
                String::new(),
            )
        } else {
            (
                "https://api.openai.com/v1/chat/completions".to_string(),
                "gpt-4o-mini".to_string(),
                env_key,
            )
        };

        Self {
            mode: ChatBackendMode::CortexServer,
            cortex_url,
            cortex_token,
            cortex_provider,
            api_url,
            model,
            api_key,
        }
    }
}

enum ChatWorkerEvent {
    ProjectsLoaded(Result<Vec<CortexProjectItem>, String>),
    HistoryLoaded(Result<Vec<ChatMessage>, String>),
    ChatReply(Result<ChatMessage, String>),
    IndexFinished(Result<String, String>),
}

enum MessageBlock {
    Text(String),
    Code { lang: String, code: String },
}

fn parse_message_blocks(content: &str) -> Vec<MessageBlock> {
    let mut blocks = Vec::new();
    let mut text_buf = String::new();
    let mut code_buf = String::new();
    let mut in_code = false;
    let mut code_lang = String::new();

    for line in content.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("```") {
            if in_code {
                blocks.push(MessageBlock::Code {
                    lang: std::mem::take(&mut code_lang),
                    code: code_buf.trim_end_matches('\n').to_string(),
                });
                code_buf.clear();
                in_code = false;
            } else {
                if !text_buf.trim().is_empty() {
                    blocks.push(MessageBlock::Text(text_buf.trim().to_string()));
                }
                text_buf.clear();
                code_lang = rest.trim().to_string();
                in_code = true;
            }
        } else if in_code {
            if !code_buf.is_empty() {
                code_buf.push('\n');
            }
            code_buf.push_str(line);
        } else {
            if !text_buf.is_empty() {
                text_buf.push('\n');
            }
            text_buf.push_str(line);
        }
    }

    if in_code {
        blocks.push(MessageBlock::Code {
            lang: code_lang,
            code: code_buf,
        });
    } else if !text_buf.trim().is_empty() {
        blocks.push(MessageBlock::Text(text_buf.trim().to_string()));
    }

    if blocks.is_empty() && !content.is_empty() {
        blocks.push(MessageBlock::Text(content.to_string()));
    }

    blocks
}

fn compute_sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for b in digest {
        let _ = write!(&mut hex, "{:02x}", b);
    }
    hex
}

fn to_relative_path(project_root: Option<&Path>, file_path: &Path) -> String {
    if let Some(root) = project_root {
        if let Ok(rel) = file_path.strip_prefix(root) {
            let s = rel.to_string_lossy().replace('\\', "/");
            if !s.is_empty() {
                return s;
            }
        }
    }
    file_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Untitled")
        .to_string()
}

/// Extrage mențiunile de tip `@fisier.ext` sau `@cale/fisier.ext` și le citește din `project_root`
fn collect_mentioned_files(
    prompt: &str,
    project_root: Option<&Path>,
) -> Vec<(String, String)> {
    let mut targets: Vec<String> = Vec::new();
    let mut seen = HashSet::new();

    for raw in prompt.split_whitespace() {
        let cleaned = raw.trim_matches(|c: char| {
            matches!(c, ',' | ';' | ':' | '!' | '?' | '(' | ')' | '[' | ']' | '"' | '\'')
        });
        if let Some(rest) = cleaned.strip_prefix('@') {
            let target = rest.trim();
            if !target.is_empty() {
                let key = target.to_ascii_lowercase();
                if seen.insert(key) {
                    targets.push(target.to_string());
                }
            }
        }
    }

    if targets.is_empty() {
        return Vec::new();
    }

    let mut found = Vec::new();
    let mut found_keys = HashSet::new();

    for target in targets {
        let target_norm = target.replace('\\', "/");
        let target_lower = target_norm.to_ascii_lowercase();

        // 1. Încercare directă în project_root
        if let Some(root) = project_root {
            let direct = root.join(&target_norm);
            if direct.is_file() {
                if let Ok(content) = std::fs::read_to_string(&direct) {
                    if content.len() <= 65_000 && found_keys.insert(target_lower.clone()) {
                        found.push((target_norm, content));
                        continue;
                    }
                }
            }

            // 2. Căutare recursivă după nume de fișier sau sufix de cale
            let mut stack = vec![root.to_path_buf()];
            while let Some(dir) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().to_string();
                    if path.is_dir() {
                        if matches!(name.as_str(), ".git" | "target" | "node_modules" | ".idea" | ".vscode") {
                            continue;
                        }
                        stack.push(path);
                    } else if path.is_file() {
                        let rel = path
                            .strip_prefix(root)
                            .unwrap_or(&path)
                            .to_string_lossy()
                            .replace('\\', "/");
                        let rel_lower = rel.to_ascii_lowercase();
                        let fname_lower = name.to_ascii_lowercase();

                        if fname_lower == target_lower
                            || rel_lower == target_lower
                            || rel_lower.ends_with(&format!("/{}", target_lower))
                        {
                            if let Ok(content) = std::fs::read_to_string(&path) {
                                if content.len() <= 65_000 && found_keys.insert(rel_lower.clone()) {
                                    found.push((rel, content));
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    found
}

fn apply_cortex_auth(req: ureq::Request, token: &str) -> ureq::Request {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        req
    } else {
        req.set("Authorization", &format!("Bearer {}", trimmed))
    }
}

pub struct AiChatPanel {
    messages: Vec<ChatMessage>,
    input: String,
    include_active_file: bool,
    show_settings: bool,
    config: AiConfig,
    projects: Vec<CortexProjectItem>,
    selected_project_id: Option<i64>,
    last_matched_root: Option<PathBuf>,
    project_sync_started: bool,
    pub status_note: String,
    rx: Option<Receiver<ChatWorkerEvent>>,
    is_loading: bool,
    input_focused: bool,
    focus_input_next_frame: bool,
}

impl Default for AiChatPanel {
    fn default() -> Self {
        Self {
            messages: vec![ChatMessage {
                role: ChatRole::Assistant,
                content: "Salut! Sunt conectat la **Cortex Server** (RAG `pgvector` + istoric persistent). Poți folosi `@nume_fisier` pentru a atașa fișiere din proiect și comuta între **Google** și **Ollama**.\n\nApasă **⚙** pentru URL-ul serverului (`http://localhost:7878` sau URL-ul tău HTTPS) și Bearer token.".to_string(),
                attached_file: None,
                mentioned_files: Vec::new(),
                usage: None,
            }],
            input: String::new(),
            include_active_file: true,
            show_settings: false,
            config: AiConfig::default(),
            projects: Vec::new(),
            selected_project_id: None,
            last_matched_root: None,
            project_sync_started: false,
            status_note: "Cortex: neconectat încă".to_string(),
            rx: None,
            is_loading: false,
            input_focused: false,
            focus_input_next_frame: false,
        }
    }
}

impl AiChatPanel {
    pub fn request_focus(&mut self) {
        self.focus_input_next_frame = true;
    }

    pub fn clear_history(&mut self) {
        self.messages.clear();
        if self.config.mode == ChatBackendMode::CortexServer {
            if let Some(pid) = self.selected_project_id {
                let base_url = self.config.cortex_url.trim_end_matches('/').to_string();
                let token = self.config.cortex_token.clone();
                thread::spawn(move || {
                    let url = format!("{base_url}/api/history?project_id={pid}");
                    let _ = apply_cortex_auth(ureq::delete(&url), &token).call();
                });
            }
        }
    }

    fn ensure_worker_channel(&mut self, ctx: &egui::Context) -> mpsc::Sender<ChatWorkerEvent> {
        if self.rx.is_none() {
            let (tx, rx) = mpsc::channel();
            self.rx = Some(rx);
            let repaint_ctx = ctx.clone();
            let orig_tx = tx.clone();
            let (wrap_tx, wrap_rx) = mpsc::channel::<ChatWorkerEvent>();
            let _ = orig_tx;
            let _ = tx;
            // Folosim un singur canal prin înlocuirea rx-ului de mai jos dacă e nevoie
            let _ = wrap_tx;
            let _ = wrap_rx;
            let _ = repaint_ctx;
        }
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        tx
    }

    /// Încarcă proiectele de pe Cortex Server și potrivește folderul curent din RED
    pub fn sync_project_with_root(&mut self, project_root: Option<&Path>, ctx: &egui::Context) {
        if self.config.mode != ChatBackendMode::CortexServer {
            return;
        }

        let matched_changed = project_root.map(|p| p.to_path_buf()) != self.last_matched_root;
        if !matched_changed && self.project_sync_started {
            return;
        }
        self.project_sync_started = true;
        self.last_matched_root = project_root.map(|p| p.to_path_buf());

        let folder_name = project_root
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(|s| s.to_ascii_lowercase());

        let base_url = self.config.cortex_url.trim_end_matches('/').to_string();
        let token = self.config.cortex_token.clone();
        let repaint_ctx = ctx.clone();
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);

        thread::spawn(move || {
            let url = format!("{base_url}/api/projects");
            let res = match apply_cortex_auth(ureq::get(&url), &token).call() {
                Ok(resp) => match resp.into_json::<Value>() {
                    Ok(arr) => {
                        let mut items = Vec::new();
                        if let Some(list) = arr.as_array() {
                            for p in list {
                                if let (Some(id), Some(name)) = (
                                    p.get("id").and_then(|v| v.as_i64()),
                                    p.get("name").and_then(|v| v.as_str()),
                                ) {
                                    let ptype = p
                                        .get("project_type")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("cpp_code")
                                        .to_string();
                                    items.push(CortexProjectItem {
                                        id,
                                        name: name.to_string(),
                                        project_type: ptype,
                                    });
                                }
                            }
                        }
                        Ok(items)
                    }
                    Err(e) => Err(format!("Eroare JSON /api/projects: {e}")),
                },
                Err(e) => Err(format!("Cortex /api/projects indisponibil: {e}")),
            };

            let _ = tx.send(ChatWorkerEvent::ProjectsLoaded(res));
            repaint_ctx.request_repaint();
            let _ = folder_name;
        });
    }

    pub fn load_history_for_project(&mut self, project_id: i64, ctx: &egui::Context) {
        if self.config.mode != ChatBackendMode::CortexServer {
            return;
        }
        let base_url = self.config.cortex_url.trim_end_matches('/').to_string();
        let token = self.config.cortex_token.clone();
        let repaint_ctx = ctx.clone();
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);

        thread::spawn(move || {
            let url = format!("{base_url}/api/history?project_id={project_id}&limit=20");
            let res = match apply_cortex_auth(ureq::get(&url), &token).call() {
                Ok(resp) => match resp.into_json::<Value>() {
                    Ok(val) => {
                        let mut msgs = Vec::new();
                        if let Some(arr) = val.as_array() {
                            for item in arr {
                                let role_str = item
                                    .get("role")
                                    .and_then(|r| r.as_str())
                                    .unwrap_or("assistant");
                                let content = item
                                    .get("content")
                                    .and_then(|c| c.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                if content.is_empty() {
                                    continue;
                                }
                                let role = if role_str == "user" {
                                    ChatRole::User
                                } else {
                                    ChatRole::Assistant
                                };
                                msgs.push(ChatMessage {
                                    role,
                                    content,
                                    attached_file: None,
                                    mentioned_files: Vec::new(),
                                    usage: None,
                                });
                            }
                        }
                        Ok(msgs)
                    }
                    Err(e) => Err(format!("Eroare JSON /api/history: {e}")),
                },
                Err(e) => Err(format!("Eroare la încărcarea istoricului: {e}")),
            };

            let _ = tx.send(ChatWorkerEvent::HistoryLoaded(res));
            repaint_ctx.request_repaint();
        });
    }

    /// Declanșează indexarea incrementală SHA-256 pe Cortex Server la salvarea unui fișier (`Ctrl+S`)
    pub fn notify_file_saved(
        &mut self,
        project_root: Option<&Path>,
        file_path: &Path,
        content: &str,
        ctx: &egui::Context,
    ) {
        if self.config.mode != ChatBackendMode::CortexServer {
            return;
        }
        let Some(project_id) = self.selected_project_id else {
            return;
        };

        let rel_path = to_relative_path(project_root, file_path);
        let sha256 = compute_sha256_hex(content);
        let base_url = self.config.cortex_url.trim_end_matches('/').to_string();
        let token = self.config.cortex_token.clone();
        let content_owned = content.to_string();
        let repaint_ctx = ctx.clone();
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);

        thread::spawn(move || {
            let url = format!("{base_url}/api/index");
            let payload = json!({
                "project_id": project_id,
                "files": [{
                    "relative_path": rel_path,
                    "content": content_owned,
                    "sha256": sha256
                }]
            });

            let res = match apply_cortex_auth(
                ureq::post(&url).set("Content-Type", "application/json"),
                &token,
            )
            .send_json(payload)
            {
                Ok(resp) => match resp.into_json::<Value>() {
                    Ok(body) => {
                        let idx = body
                            .get("indexed_count")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let skip = body
                            .get("skipped_unchanged")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        Ok(format!("Cortex index: {idx} actualizat, {skip} neschimbat"))
                    }
                    Err(e) => Err(format!("Eroare JSON /api/index: {e}")),
                },
                Err(e) => Err(format!("Eroare /api/index: {e}")),
            };

            let _ = tx.send(ChatWorkerEvent::IndexFinished(res));
            repaint_ctx.request_repaint();
        });
    }

    fn poll_events(&mut self, ctx: &egui::Context) {
        let mut drained = Vec::new();
        let mut disconnected = false;

        if let Some(rx) = &self.rx {
            loop {
                match rx.try_recv() {
                    Ok(ev) => drained.push(ev),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }

        let mut should_load_history_for: Option<i64> = None;

        for ev in drained {
            match ev {
                ChatWorkerEvent::ProjectsLoaded(Ok(list)) => {
                    let target_name = self
                        .last_matched_root
                        .as_ref()
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str())
                        .map(|s| s.to_ascii_lowercase());

                    if let Some(ref want) = target_name {
                        if let Some(found) = list
                            .iter()
                            .find(|p| p.name.to_ascii_lowercase() == *want)
                        {
                            self.selected_project_id = Some(found.id);
                        }
                    }

                    if self.selected_project_id.is_none() {
                        if let Some(first) = list.first() {
                            self.selected_project_id = Some(first.id);
                        }
                    }

                    self.projects = list;
                    self.status_note = match self.selected_project_id {
                        Some(id) => format!("Cortex conectat • proiect #{id}"),
                        None => "Cortex conectat (fără proiecte în DB)".to_string(),
                    };

                    if let Some(pid) = self.selected_project_id {
                        should_load_history_for = Some(pid);
                    }
                }
                ChatWorkerEvent::ProjectsLoaded(Err(err)) => {
                    self.status_note = err;
                }
                ChatWorkerEvent::HistoryLoaded(Ok(msgs)) => {
                    if !msgs.is_empty() {
                        self.messages = msgs;
                    }
                }
                ChatWorkerEvent::HistoryLoaded(Err(err)) => {
                    self.status_note = err;
                }
                ChatWorkerEvent::ChatReply(Ok(reply_msg)) => {
                    if let Some(u) = &reply_msg.usage {
                        self.status_note = u.format_short();
                    }
                    self.messages.push(reply_msg);
                    self.is_loading = false;
                }
                ChatWorkerEvent::ChatReply(Err(err)) => {
                    self.messages.push(ChatMessage {
                        role: ChatRole::Error,
                        content: err,
                        attached_file: None,
                        mentioned_files: Vec::new(),
                        usage: None,
                    });
                    self.is_loading = false;
                }
                ChatWorkerEvent::IndexFinished(Ok(note)) => {
                    self.status_note = note;
                }
                ChatWorkerEvent::IndexFinished(Err(err)) => {
                    self.status_note = err;
                }
            }
        }

        if disconnected && self.is_loading {
            self.is_loading = false;
        }

        let _ = ensure_worker_channel_noop(ctx);
        if let Some(pid) = should_load_history_for {
            self.load_history_for_project(pid, ctx);
        }
    }

    fn send_prompt(
        &mut self,
        active_file: Option<&ActiveFileContext>,
        project_root: Option<&Path>,
        ctx: &egui::Context,
    ) {
        let prompt = self.input.trim().to_string();
        if prompt.is_empty() || self.is_loading {
            return;
        }
        self.input.clear();

        let mentioned = collect_mentioned_files(&prompt, project_root);
        let mentioned_names: Vec<String> = mentioned.iter().map(|(p, _)| p.clone()).collect();

        let attached_name = if self.include_active_file {
            active_file
                .filter(|f| !f.content.trim().is_empty())
                .map(|f| f.file_name.clone())
        } else {
            None
        };

        self.messages.push(ChatMessage {
            role: ChatRole::User,
            content: prompt.clone(),
            attached_file: attached_name.clone(),
            mentioned_files: mentioned_names.clone(),
            usage: None,
        });

        let config = self.config.clone();
        let selected_pid = self.selected_project_id;
        let active_payload = if self.include_active_file {
            active_file
                .filter(|f| !f.content.trim().is_empty())
                .map(|f| {
                    json!({
                        "relative_path": f.relative_path,
                        "content": f.content,
                        "sha256": compute_sha256_hex(&f.content)
                    })
                })
        } else {
            None
        };

        let mentioned_payload: Vec<Value> = mentioned
            .into_iter()
            .map(|(rel_path, content)| {
                json!({
                    "relative_path": rel_path,
                    "content": content,
                    "sha256": compute_sha256_hex(&content)
                })
            })
            .collect();

        // Fallback history pentru modul OpenAI-compatibil
        let mut api_messages = vec![json!({
            "role": "system",
            "content": "You are an expert Rust and software engineering assistant embedded inside the RED code editor. Be concise, accurate, and wrap code snippets in fenced markdown code blocks with the language identifier."
        })];

        let start_idx = self.messages.len().saturating_sub(12);
        let last_idx = self.messages.len() - 1;
        for (idx, msg) in self.messages.iter().enumerate().skip(start_idx) {
            match msg.role {
                ChatRole::User => {
                    let content = if idx == last_idx {
                        if let Some(file) = active_file.filter(|_| self.include_active_file) {
                            if !file.content.trim().is_empty() {
                                let lang = file.extension.as_deref().unwrap_or("");
                                format!(
                                    "Fișier activ: `{}`\n```{}\n{}\n```\n\nCerință:\n{}",
                                    file.file_name, lang, file.content, msg.content
                                )
                            } else {
                                msg.content.clone()
                            }
                        } else {
                            msg.content.clone()
                        }
                    } else {
                        msg.content.clone()
                    };
                    api_messages.push(json!({ "role": "user", "content": content }));
                }
                ChatRole::Assistant => {
                    api_messages.push(json!({ "role": "assistant", "content": msg.content }));
                }
                ChatRole::Error => {}
            }
        }

        let (tx, rx) = mpsc::channel();
        let repaint_ctx = ctx.clone();
        self.rx = Some(rx);
        self.is_loading = true;

        thread::spawn(move || {
            let event = match config.mode {
                ChatBackendMode::CortexServer => {
                    if let Some(project_id) = selected_pid {
                        let url = format!(
                            "{}/api/chat",
                            config.cortex_url.trim_end_matches('/')
                        );
                        let payload = json!({
                            "project_id": project_id,
                            "message": prompt,
                            "llm_provider": config.cortex_provider,
                            "active_file": active_payload,
                            "mentioned_files": mentioned_payload
                        });

                        let res = match apply_cortex_auth(
                            ureq::post(&url).set("Content-Type", "application/json"),
                            &config.cortex_token,
                        )
                        .send_json(payload)
                        {
                            Ok(resp) => match resp.into_json::<Value>() {
                                Ok(body) => {
                                    let usage = TokenUsageInfo::from_json(&body);
                                    body.get("answer")
                                        .and_then(|a| a.as_str())
                                        .map(|s| ChatMessage {
                                            role: ChatRole::Assistant,
                                            content: s.trim().to_string(),
                                            attached_file: None,
                                            mentioned_files: Vec::new(),
                                            usage,
                                        })
                                        .ok_or_else(|| {
                                            format!("Răspuns neașteptat de la Cortex: {body}")
                                        })
                                }
                                Err(e) => Err(format!("Eroare JSON de la Cortex: {e}")),
                            },
                            Err(ureq::Error::Status(code, resp)) => {
                                let detail = resp.into_string().unwrap_or_default();
                                Err(format!("Cortex HTTP {code}: {detail}"))
                            }
                            Err(ureq::Error::Transport(err)) => Err(format!(
                                "Nu mă pot conecta la Cortex ({err}). Verifică URL-ul din ⚙ Settings."
                            )),
                        };
                        ChatWorkerEvent::ChatReply(res)
                    } else {
                        ChatWorkerEvent::ChatReply(Err(
                            "Nu este selectat niciun proiect Cortex. Verifică conexiunea în ⚙ Settings.".to_string(),
                        ))
                    }
                }
                ChatBackendMode::OpenAiCompatible => {
                    let payload = json!({
                        "model": config.model,
                        "messages": api_messages,
                        "temperature": 0.2
                    });

                    let mut req =
                        ureq::post(&config.api_url).set("Content-Type", "application/json");
                    if !config.api_key.trim().is_empty() {
                        req = req.set(
                            "Authorization",
                            &format!("Bearer {}", config.api_key.trim()),
                        );
                    }

                    let res = match req.send_json(payload) {
                        Ok(resp) => match resp.into_json::<Value>() {
                            Ok(body) => body["choices"][0]["message"]["content"]
                                .as_str()
                                .map(|s| ChatMessage {
                                    role: ChatRole::Assistant,
                                    content: s.trim().to_string(),
                                    attached_file: None,
                                    mentioned_files: Vec::new(),
                                    usage: None,
                                })
                                .ok_or_else(|| {
                                    format!("Răspuns neașteptat de la server: {body}")
                                }),
                            Err(e) => Err(format!("Eroare la parsarea JSON: {e}")),
                        },
                        Err(ureq::Error::Status(code, resp)) => {
                            let detail = resp.into_string().unwrap_or_default();
                            Err(format!("Eroare HTTP {code}: {detail}"))
                        }
                        Err(ureq::Error::Transport(err)) => Err(format!(
                            "Eroare de conexiune ({err}). Verifică URL-ul din ⚙ Settings."
                        )),
                    };
                    ChatWorkerEvent::ChatReply(res)
                }
            };

            let _ = tx.send(event);
            repaint_ctx.request_repaint();
        });
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        active_file: Option<&ActiveFileContext>,
        project_root: Option<&Path>,
    ) -> (bool, AiAction) {
        self.sync_project_with_root(project_root, ctx);
        self.poll_events(ctx);
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);

        let mut close_panel = false;
        let mut action = AiAction::None;
        let dark_mode = ui.visuals().dark_mode;

        let (user_bg, assistant_bg, code_bg, border_color, accent_color) = if dark_mode {
            (
                Color32::from_rgb(28, 44, 68),
                Color32::from_rgb(24, 28, 36),
                Color32::from_rgb(13, 17, 23),
                Color32::from_gray(55),
                Color32::from_rgb(88, 166, 255),
            )
        } else {
            (
                Color32::from_rgb(225, 238, 255),
                Color32::from_rgb(243, 244, 246),
                Color32::from_rgb(250, 250, 250),
                Color32::from_gray(205),
                Color32::from_rgb(9, 105, 218),
            )
        };

        // 1. Header bar
        ui.horizontal(|ui| {
            ui.strong("AI CHAT");
            if self.config.mode == ChatBackendMode::CortexServer {
                if ui
                    .selectable_label(self.config.cortex_provider == "google", "Gemini")
                    .on_hover_text("Folosește Google Gemini")
                    .clicked()
                {
                    self.config.cortex_provider = "google".to_string();
                }
                if ui
                    .selectable_label(self.config.cortex_provider == "ollama", "Ollama")
                    .on_hover_text("Folosește Ollama local")
                    .clicked()
                {
                    self.config.cortex_provider = "ollama".to_string();
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("×")
                    .on_hover_text("Ascunde panoul AI (Ctrl+Shift+I)")
                    .clicked()
                {
                    close_panel = true;
                }
                if ui
                    .selectable_label(self.show_settings, "⚙")
                    .on_hover_text("Configurare Cortex Server / API")
                    .clicked()
                {
                    self.show_settings = !self.show_settings;
                }
                if ui
                    .small_button("↻")
                    .on_hover_text("Reîncarcă proiectele și istoricul din Cortex")
                    .clicked()
                {
                    self.last_matched_root = None;
                    self.project_sync_started = false;
                    self.sync_project_with_root(project_root, ctx);
                }
                if ui
                    .small_button("+")
                    .on_hover_text("Șterge istoricul conversației")
                    .clicked()
                {
                    self.clear_history();
                }
            });
        });

        ui.separator();

        // 2. Panou setări Cortex Server / Fallback OpenAI
        if self.show_settings {
            egui::Frame::NONE
                .fill(assistant_bg)
                .stroke(Stroke::new(1.0_f32, border_color))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.small("Mod:");
                        if ui
                            .selectable_label(
                                self.config.mode == ChatBackendMode::CortexServer,
                                "Cortex Server",
                            )
                            .clicked()
                        {
                            self.config.mode = ChatBackendMode::CortexServer;
                            self.last_matched_root = None;
                            self.project_sync_started = false;
                            self.sync_project_with_root(project_root, ctx);
                        }
                        if ui
                            .selectable_label(
                                self.config.mode == ChatBackendMode::OpenAiCompatible,
                                "Direct API",
                            )
                            .clicked()
                        {
                            self.config.mode = ChatBackendMode::OpenAiCompatible;
                        }
                    });

                    ui.add_space(4.0);

                    if self.config.mode == ChatBackendMode::CortexServer {
                        ui.small("Cortex URL (local sau HTTPS):");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.config.cortex_url)
                                .desired_width(f32::INFINITY),
                        );

                        ui.small("Bearer Token (CORTEX_API_KEY):");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.config.cortex_token)
                                .password(true)
                                .desired_width(f32::INFINITY),
                        );

                        if !self.projects.is_empty() {
                            ui.small("Proiect Cortex:");
                            let current_label = self
                                .projects
                                .iter()
                                .find(|p| Some(p.id) == self.selected_project_id)
                                .map(|p| format!("#{} — {} [{}]", p.id, p.name, p.project_type))
                                .unwrap_or_else(|| "Selectează".to_string());

                            let mut project_to_load = None;
                            egui::ComboBox::from_id_salt("cortex_project_combo")
                                .selected_text(current_label)
                                .width(ui.available_width())
                                .show_ui(ui, |ui| {
                                    for p in &self.projects {
                                        let label =
                                            format!("#{} — {} [{}]", p.id, p.name, p.project_type);
                                        if ui
                                            .selectable_value(
                                                &mut self.selected_project_id,
                                                Some(p.id),
                                                label,
                                            )
                                            .clicked()
                                        {
                                            project_to_load = Some(p.id);
                                        }
                                    }
                                });
                            if let Some(project_id) = project_to_load {
                                self.load_history_for_project(project_id, ctx);
                            }
                        }
                    } else {
                        ui.small("Endpoint URL:");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.config.api_url)
                                .desired_width(f32::INFINITY),
                        );
                        ui.small("Model:");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.config.model)
                                .desired_width(f32::INFINITY),
                        );
                        ui.small("API Key:");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.config.api_key)
                                .password(true)
                                .desired_width(f32::INFINITY),
                        );
                    }
                });

            ui.add_space(4.0);
            ui.separator();
        }

        // 3. Mesaje
        let composer_reserved_height = 118.0;
        let scroll_height = (ui.available_height() - composer_reserved_height).max(90.0);

        egui::ScrollArea::vertical()
            .id_salt("ai_chat_messages_scroll")
            .min_scrolled_height(scroll_height)
            .max_height(scroll_height)
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());

                for msg in &self.messages {
                    let (sender, bubble_bg) = match msg.role {
                        ChatRole::User => ("Tu", user_bg),
                        ChatRole::Assistant => ("Cortex", assistant_bg),
                        ChatRole::Error => ("Eroare", Color32::from_rgb(120, 35, 35)),
                    };

                    egui::Frame::NONE
                        .fill(bubble_bg)
                        .stroke(Stroke::new(1.0_f32, border_color))
                        .corner_radius(6.0)
                        .inner_margin(egui::Margin::same(8))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());

                            ui.horizontal_wrapped(|ui| {
                                ui.strong(sender);
                                if let Some(file_name) = &msg.attached_file {
                                    ui.weak(format!("• 📄 {file_name}"));
                                }
                                for mf in &msg.mentioned_files {
                                    ui.weak(format!("• @{mf}"));
                                }
                            });

                            ui.add_space(4.0);

                            if msg.role == ChatRole::Error {
                                ui.colored_label(Color32::from_rgb(255, 190, 190), &msg.content);
                                return;
                            }

                            for block in parse_message_blocks(&msg.content) {
                                match block {
                                    MessageBlock::Text(text) => {
                                        ui.add(egui::Label::new(text).selectable(true));
                                        ui.add_space(4.0);
                                    }
                                    MessageBlock::Code { lang, code } => {
                                        egui::Frame::NONE
                                            .fill(code_bg)
                                            .stroke(Stroke::new(1.0_f32, border_color))
                                            .corner_radius(4.0)
                                            .inner_margin(egui::Margin::same(6))
                                            .show(ui, |ui| {
                                                ui.set_width(ui.available_width());

                                                ui.horizontal(|ui| {
                                                    let title = if lang.is_empty() {
                                                        "code"
                                                    } else {
                                                        lang.as_str()
                                                    };
                                                    ui.small(
                                                        RichText::new(title).color(accent_color),
                                                    );

                                                    ui.with_layout(
                                                        egui::Layout::right_to_left(
                                                            egui::Align::Center,
                                                        ),
                                                        |ui| {
                                                            if ui
                                                                .small_button("Copy")
                                                                .on_hover_text("Copiază codul")
                                                                .clicked()
                                                            {
                                                                ctx.copy_text(code.clone());
                                                            }
                                                            if ui
                                                                .small_button("Insert")
                                                                .on_hover_text(
                                                                    "Inserează în tab-ul activ",
                                                                )
                                                                .clicked()
                                                            {
                                                                action = AiAction::InsertCode(
                                                                    code.clone(),
                                                                );
                                                            }
                                                            if ui
                                                                .small_button("Apply")
                                                                .on_hover_text(
                                                                    "Înlocuiește conținutul fișierului activ",
                                                                )
                                                                .clicked()
                                                            {
                                                                action = AiAction::ReplaceFile(
                                                                    code.clone(),
                                                                );
                                                            }
                                                        },
                                                    );
                                                });

                                                ui.separator();

                                                egui::ScrollArea::horizontal()
                                                    .id_salt((&code, msg.content.len()))
                                                    .show(ui, |ui| {
                                                        ui.add(
                                                            egui::Label::new(
                                                                RichText::new(&code).font(
                                                                    FontId::monospace(12.5),
                                                                ),
                                                            )
                                                            .selectable(true),
                                                        );
                                                    });
                                            });
                                        ui.add_space(4.0);
                                    }
                                }
                            }

                            if let Some(usage) = &msg.usage {
                                ui.add_space(2.0);
                                ui.small(RichText::new(usage.format_short()).weak());
                            }
                        });

                    ui.add_space(6.0);
                }

                if self.is_loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.weak("Cortex procesează cererea...");
                    });
                }
            });

        ui.separator();

        // 4. Composer
        let context_label = match active_file {
            Some(f) => format!("Atașează fișierul activ ({})", f.file_name),
            None => "Atașează fișierul activ".to_string(),
        };
        ui.checkbox(&mut self.include_active_file, context_label);

        let mut send_requested = false;
        if self.input_focused
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
        {
            send_requested = true;
        }

        let input_resp = ui.add(
            egui::TextEdit::multiline(&mut self.input)
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .hint_text("Întreabă Cortex... (folosește @fisier; Enter = trimite)"),
        );

        if self.focus_input_next_frame {
            input_resp.request_focus();
            self.focus_input_next_frame = false;
        }
        self.input_focused = input_resp.has_focus();

        ui.horizontal(|ui| {
            let active_info = if self.config.mode == ChatBackendMode::CortexServer {
                format!(
                    "{} • {}",
                    self.config.cortex_provider, self.status_note
                )
            } else {
                self.config.model.clone()
            };
            ui.small(RichText::new(active_info).weak());

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let can_send = !self.is_loading && !self.input.trim().is_empty();
                if ui
                    .add_enabled(can_send, egui::Button::new("Send"))
                    .clicked()
                {
                    send_requested = true;
                }
            });
        });

        if send_requested {
            self.send_prompt(active_file, project_root, ctx);
            input_resp.request_focus();
        }

        (close_panel, action)
    }
}

fn ensure_worker_channel_noop(_ctx: &egui::Context) {}

fn build_http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(5))
        .timeout_read(std::time::Duration::from_secs(90))
        .build()
}