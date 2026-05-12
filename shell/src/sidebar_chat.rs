use crate::deps::*;
use futures::channel::mpsc::{self, UnboundedSender};
use futures::StreamExt;
use std::cell::Cell;
use std::io::BufRead;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

const OLLAMA_URL: &str = "http://localhost:11434";
const MODEL: &str = "gemma4";
const MAX_CONTEXT_CHARS: usize = 16_000;

const SYSTEM_PROMPT: &str = "\
You are a helpful AI assistant embedded in a PDF document viewer.\n\
You have two modes depending on the question:\n\
\n\
1. Document questions: If the question is about the document content provided below, \
answer using that content and cite the page number(s), e.g. \"According to page 3\".\n\
\n\
2. General questions: If the question is general knowledge (not about this specific document), \
answer from your own knowledge freely and helpfully. Do not force a connection to the document.\n\
\n\
Always:\n\
- When the user says \"this page\" or \"here\", focus on the Current Page section.\n\
- Write in plain, readable sentences. Do NOT use markdown: no # headers, no ** or * symbols, no backticks.\n\
- Use plain numbered lists (1. 2. 3.) or dashes (- item) if needed.\n\
- Be concise and accurate.\n";

// ── Streaming events ──────────────────────────────────────────────────────────

enum ChatEvent {
    Status(String),
    Delta(String),
    Done,
    Error(String),
}

// ── Ollama helpers ────────────────────────────────────────────────────────────

fn ensure_ollama_running() -> Result<(), String> {
    if ureq::get(&format!("{OLLAMA_URL}/api/tags")).call().is_ok() {
        return Ok(());
    }
    std::process::Command::new("ollama")
        .arg("serve")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("Could not start Ollama: {e}.\nInstall it from https://ollama.com"))?;

    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if ureq::get(&format!("{OLLAMA_URL}/api/tags")).call().is_ok() {
            return Ok(());
        }
    }
    Err("Ollama started but did not become ready in time.".to_string())
}

const GEMMA_PREFERENCE: &[&str] = &["gemma4", "gemma3", "gemma2", "gemma"];

fn find_or_pull_model(event_tx: &UnboundedSender<ChatEvent>) -> Result<String, String> {
    let json: serde_json::Value = ureq::get(&format!("{OLLAMA_URL}/api/tags"))
        .call()
        .map_err(|e| format!("Cannot reach Ollama: {e}"))?
        .into_json()
        .map_err(|e| format!("Failed to parse model list: {e}"))?;

    let installed: Vec<String> = json["models"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    for preferred in GEMMA_PREFERENCE {
        if let Some(found) = installed.iter().find(|n| n.starts_with(preferred)) {
            return Ok(found.clone());
        }
    }

    let _ = event_tx.unbounded_send(ChatEvent::Status(format!(
        "Downloading {MODEL}… (this may take a while)"
    )));
    ureq::post(&format!("{OLLAMA_URL}/api/pull"))
        .set("Content-Type", "application/json")
        .send_json(serde_json::json!({ "name": MODEL, "stream": false }))
        .map_err(|e| format!("Failed to download {MODEL}: {e}"))?;

    Ok(MODEL.to_string())
}

fn call_ollama_streaming(
    context: &str,
    messages: &[(String, String)],
    event_tx: UnboundedSender<ChatEvent>,
    cancel: Arc<AtomicBool>,
) {
    let _ = event_tx.unbounded_send(ChatEvent::Status("Starting Ollama…".into()));
    if let Err(e) = ensure_ollama_running() {
        let _ = event_tx.unbounded_send(ChatEvent::Error(e));
        return;
    }

    let _ = event_tx.unbounded_send(ChatEvent::Status("Checking model…".into()));
    let model = match find_or_pull_model(&event_tx) {
        Ok(m) => m,
        Err(e) => {
            let _ = event_tx.unbounded_send(ChatEvent::Error(e));
            return;
        }
    };

    let _ = event_tx.unbounded_send(ChatEvent::Status("Thinking…".into()));

    let system = format!("{SYSTEM_PROMPT}\n{context}");
    let mut json_messages =
        vec![serde_json::json!({ "role": "system", "content": system })];
    for (role, content) in messages {
        json_messages.push(serde_json::json!({ "role": role, "content": content }));
    }

    let body = serde_json::json!({
        "model": model,
        "messages": json_messages,
        "stream": true
    });

    let response = match ureq::post(&format!("{OLLAMA_URL}/api/chat"))
        .set("Content-Type", "application/json")
        .send_json(body)
    {
        Ok(r) => r,
        Err(e) => {
            let _ = event_tx.unbounded_send(ChatEvent::Error(format!("Request failed: {e}")));
            return;
        }
    };

    let reader = std::io::BufReader::new(response.into_reader());
    for line in reader.lines() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                let _ = event_tx.unbounded_send(ChatEvent::Error(format!("Read error: {e}")));
                return;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let json: serde_json::Value = match serde_json::from_str(&line) {
            Ok(j) => j,
            Err(_) => continue,
        };
        let delta = json["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let done = json["done"].as_bool().unwrap_or(false);
        if !delta.is_empty() {
            let _ = event_tx.unbounded_send(ChatEvent::Delta(delta));
        }
        if done {
            break;
        }
    }
    let _ = event_tx.unbounded_send(ChatEvent::Done);
}

// ── Context building ──────────────────────────────────────────────────────────

fn relevance_score(chunk_text: &str, query: &str, history: &[(String, String)]) -> usize {
    let haystack = chunk_text.to_lowercase();
    let mut words: Vec<String> = query
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .collect();
    let recent: Vec<String> = history
        .iter()
        .rev()
        .take(4)
        .flat_map(|(_, c)| c.split_whitespace().map(|w| w.to_lowercase()).collect::<Vec<_>>())
        .collect();
    words.extend(recent);
    words
        .iter()
        .filter(|w| w.len() > 3 && haystack.contains(w.as_str()))
        .count()
}

fn build_context(
    query: &str,
    history: &[(String, String)],
    chunks: &[(i32, String)],
    current_page: i32,
) -> String {
    if chunks.is_empty() {
        return String::new();
    }

    let mut scored: Vec<(usize, &(i32, String))> = chunks
        .iter()
        .map(|c| {
            let mut score = relevance_score(&c.1, query, history);
            if c.0 == current_page {
                score += 1000;
            }
            (score, c)
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));

    let mut selected: Vec<&(i32, String)> = Vec::new();
    let mut total = 0usize;

    if let Some(cp) = chunks.iter().find(|c| c.0 == current_page) {
        selected.push(cp);
        total += cp.1.len();
    }
    for (_, chunk) in &scored {
        if chunk.0 == current_page {
            continue;
        }
        if total + chunk.1.len() > MAX_CONTEXT_CHARS {
            break;
        }
        selected.push(chunk);
        total += chunk.1.len();
    }
    selected.sort_by_key(|c| c.0);

    selected
        .iter()
        .map(|c| {
            if c.0 == current_page {
                format!(
                    "=== Page {} (CURRENT PAGE — user is viewing this) ===\n{}",
                    c.0, c.1
                )
            } else {
                format!("=== Page {} ===\n{}", c.0, c.1)
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

// ── Response cleanup ──────────────────────────────────────────────────────────

fn strip_markdown(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for line in input.lines() {
        let line = line
            .trim_start_matches("### ")
            .trim_start_matches("## ")
            .trim_start_matches("# ");
        let line = if line.starts_with("* ") {
            format!("- {}", &line[2..])
        } else {
            line.to_string()
        };
        let line = remove_inline_markers(&line);
        out.push_str(&line);
        out.push('\n');
    }
    out.trim_end().to_string()
}

fn remove_inline_markers(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'*' {
            i += 2;
            continue;
        }
        if bytes[i] == b'*' || bytes[i] == b'`' {
            i += 1;
            continue;
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

// ── Page citation parsing ─────────────────────────────────────────────────────

fn parse_page_citations(text: &str) -> Vec<i32> {
    let mut pages = std::collections::BTreeSet::new();
    let words: Vec<&str> = text.split_whitespace().collect();
    for i in 0..words.len() {
        let w = words[i].to_lowercase();
        let w = w.trim_matches(|c: char| !c.is_alphabetic() && c != '.');
        if w == "page" || w == "pages" || w == "p." {
            if let Some(next) = words.get(i + 1) {
                let digits: String = next.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(n) = digits.parse::<i32>() {
                    if n > 0 && n < 10_000 {
                        pages.insert(n);
                    }
                }
            }
        }
    }
    pages.into_iter().collect()
}

// ── Widget ────────────────────────────────────────────────────────────────────

mod imp {
    use super::*;

    #[derive(CompositeTemplate, Debug, Default, Properties)]
    #[properties(wrapper_type = super::PpsSidebarChat)]
    #[template(resource = "/org/gnome/papers/ui/sidebar-chat.ui")]
    pub struct PpsSidebarChat {
        #[template_child]
        pub(super) message_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) input_entry: TemplateChild<gtk::Entry>,
        #[template_child]
        pub(super) send_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(super) stop_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(super) clear_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(super) summarize_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(super) empty_status: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub(super) scroll_window: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub(super) loading_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub(super) loading_label: TemplateChild<gtk::Label>,

        pub(super) messages: RefCell<Vec<(String, String)>>,
        pub(super) is_loading: Cell<bool>,
        pub(super) doc_chunks_cache: RefCell<Option<Vec<(i32, String)>>>,
        pub(super) cancel_flag: RefCell<Option<Arc<AtomicBool>>>,
        // Label being updated during the current streaming response.
        pub(super) streaming_label: RefCell<Option<gtk::Label>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PpsSidebarChat {
        const NAME: &'static str = "PpsSidebarChat";
        type Type = super::PpsSidebarChat;
        type ParentType = PpsSidebarPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.bind_template_callbacks();
        }

        fn instance_init(obj: &InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for PpsSidebarChat {
        fn constructed(&self) {
            self.parent_constructed();
            // Invalidate chunk cache when the document changes.
            if let Some(model) = self.obj().document_model() {
                model.connect_notify_local(
                    Some("document"),
                    glib::clone!(
                        #[weak(rename_to = obj)]
                        self,
                        move |_, _| {
                            obj.doc_chunks_cache.borrow_mut().take();
                        }
                    ),
                );
            }
        }
    }

    impl WidgetImpl for PpsSidebarChat {}
    impl BinImpl for PpsSidebarChat {}

    impl PpsSidebarPageImpl for PpsSidebarChat {
        fn support_document(&self, _document: &papers_document::Document) -> bool {
            true
        }
    }

    #[gtk::template_callbacks]
    impl PpsSidebarChat {
        // ── Callbacks ─────────────────────────────────────────────────────────

        #[template_callback]
        fn on_send(&self) {
            let text = self.input_entry.text().trim().to_string();
            if text.is_empty() || self.is_loading.get() {
                return;
            }
            self.input_entry.set_text("");
            self.send_message(text);
        }

        #[template_callback]
        fn on_stop(&self) {
            if let Some(flag) = self.cancel_flag.borrow().as_ref() {
                flag.store(true, Ordering::Relaxed);
            }
        }

        #[template_callback]
        fn on_clear(&self) {
            // Remove all dynamic message rows, keep empty_status in the box.
            while let Some(child) = self.message_box.first_child() {
                self.message_box.remove(&child);
            }
            self.message_box.append(&*self.empty_status);
            self.empty_status.set_visible(true);
            self.messages.borrow_mut().clear();
        }

        #[template_callback]
        fn on_summarize(&self) {
            if self.is_loading.get() {
                return;
            }
            let page = self.current_page();
            let query = format!("Please summarize the content of page {page} in a few sentences.");
            self.input_entry.set_text(&query);
            self.send_message(query);
        }

        // ── Helpers ───────────────────────────────────────────────────────────

        fn current_page(&self) -> i32 {
            self.obj()
                .document_model()
                .map(|m| m.page() + 1)
                .unwrap_or(1)
        }

        // Extract every page as (1-based page number, text). Cached.
        fn page_chunks(&self) -> Vec<(i32, String)> {
            if let Some(cached) = self.doc_chunks_cache.borrow().as_ref() {
                return cached.clone();
            }
            let chunks = self
                .obj()
                .document_model()
                .and_then(|m| m.document())
                .map(|doc| {
                    let n = doc.n_pages();
                    let mut result = Vec::with_capacity(n as usize);
                    let Some(dt) = doc.dynamic_cast_ref::<papers_document::DocumentText>() else {
                        return result;
                    };
                    for i in 0..n {
                        if let Some(page) = doc.page(i) {
                            if let Some(t) = dt.text(&page) {
                                let text = t.to_string();
                                if !text.trim().is_empty() {
                                    result.push((i + 1, text));
                                }
                            }
                        }
                    }
                    result
                })
                .unwrap_or_default();
            *self.doc_chunks_cache.borrow_mut() = Some(chunks.clone());
            chunks
        }

        fn scroll_to_bottom(&self) {
            glib::idle_add_local_once(glib::clone!(
                #[weak(rename_to = scroll)]
                self.scroll_window,
                move || {
                    let adj = scroll.vadjustment();
                    adj.set_value(adj.upper() - adj.page_size());
                }
            ));
        }

        // Add a user message bubble.
        fn add_user_row(&self, text: &str) {
            self.empty_status.set_visible(false);

            let label = gtk::Label::builder()
                .label(text)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .xalign(0.0)
                .selectable(true)
                .max_width_chars(38)
                .build();

            let bubble = gtk::Box::new(gtk::Orientation::Vertical, 0);
            bubble.set_margin_top(6);
            bubble.set_margin_bottom(6);
            bubble.set_margin_start(10);
            bubble.set_margin_end(10);
            bubble.add_css_class("chat-bubble-user");
            bubble.append(&label);

            let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            row.set_hexpand(true);
            row.set_halign(gtk::Align::End);
            row.append(&bubble);
            self.message_box.append(&row);
            self.scroll_to_bottom();
        }

        // Create an empty assistant bubble that will be filled during streaming.
        // Returns (streaming label, outer container for finalization).
        fn create_assistant_row(&self) -> (gtk::Label, gtk::Box) {
            self.empty_status.set_visible(false);

            let label = gtk::Label::builder()
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .xalign(0.0)
                .selectable(true)
                .max_width_chars(38)
                .build();

            let bubble = gtk::Box::new(gtk::Orientation::Vertical, 0);
            bubble.set_margin_top(6);
            bubble.set_margin_bottom(6);
            bubble.set_margin_start(10);
            bubble.set_margin_end(10);
            bubble.add_css_class("chat-bubble-assistant");
            bubble.append(&label);

            let msg_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            msg_row.set_hexpand(true);
            msg_row.set_halign(gtk::Align::Start);
            msg_row.append(&bubble);

            // Outer container holds the bubble row + action row appended after Done.
            let outer = gtk::Box::new(gtk::Orientation::Vertical, 2);
            outer.append(&msg_row);
            self.message_box.append(&outer);
            self.scroll_to_bottom();

            (label, outer)
        }

        // After streaming finishes: add copy button and page-citation buttons.
        fn finalize_assistant_row(&self, outer: &gtk::Box, text: &str) {
            let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            actions.set_margin_start(14);
            actions.set_margin_bottom(4);

            // Copy button
            let copy_btn = gtk::Button::builder()
                .icon_name("edit-copy-symbolic")
                .tooltip_text(gettext("Copy response"))
                .valign(gtk::Align::Center)
                .build();
            copy_btn.add_css_class("flat");
            let text_copy = text.to_string();
            copy_btn.connect_clicked(move |_| {
                if let Some(display) = gdk::Display::default() {
                    display.clipboard().set_text(&text_copy);
                }
            });
            actions.append(&copy_btn);

            // Page citation buttons
            let n_pages = self
                .obj()
                .document_model()
                .and_then(|m| m.document())
                .map(|d| d.n_pages())
                .unwrap_or(0);

            for page in parse_page_citations(text) {
                if page < 1 || page > n_pages {
                    continue;
                }
                let btn = gtk::Button::builder()
                    .label(format!("Go to page {page}"))
                    .tooltip_text(gettext("Jump to this page in the document"))
                    .valign(gtk::Align::Center)
                    .build();
                btn.add_css_class("flat");
                btn.add_css_class("pill");
                let page_idx = page - 1;
                btn.connect_clicked(glib::clone!(
                    #[weak(rename_to = obj)]
                    self,
                    move |_| {
                        if let Some(model) = obj.obj().document_model() {
                            model.set_page(page_idx);
                            obj.obj().navigate_to_view();
                        }
                    }
                ));
                actions.append(&btn);
            }

            outer.append(&actions);
        }

        fn set_loading(&self, loading: bool) {
            self.is_loading.set(loading);
            self.loading_revealer.set_reveal_child(loading);
            self.send_button.set_visible(!loading);
            self.stop_button.set_visible(loading);
            self.input_entry.set_sensitive(!loading);
            self.summarize_button.set_sensitive(!loading);
        }

        fn send_message(&self, user_text: String) {
            self.set_loading(true);
            self.add_user_row(&user_text);
            self.messages
                .borrow_mut()
                .push(("user".to_string(), user_text.clone()));

            let chunks = self.page_chunks();
            let current_page = self.current_page();
            let history = self.messages.borrow().clone();
            let context = build_context(&user_text, &history, &chunks, current_page);
            let messages_snapshot = history;

            let cancel = Arc::new(AtomicBool::new(false));
            *self.cancel_flag.borrow_mut() = Some(cancel.clone());

            let (stream_label, stream_outer) = self.create_assistant_row();
            *self.streaming_label.borrow_mut() = Some(stream_label.clone());

            let (event_tx, mut event_rx) = mpsc::unbounded::<ChatEvent>();

            // Process events on the UI thread.
            glib::spawn_future_local(glib::clone!(
                #[weak(rename_to = obj)]
                self,
                async move {
                    let mut full_text = String::new();
                    let mut had_error = false;

                    while let Some(event) = event_rx.next().await {
                        match event {
                            ChatEvent::Status(s) => {
                                obj.loading_label.set_label(&s);
                            }
                            ChatEvent::Delta(d) => {
                                full_text.push_str(&d);
                                // Show raw text while streaming; cleanup applied on Done.
                                stream_label.set_text(&full_text);
                                obj.scroll_to_bottom();
                            }
                            ChatEvent::Done => break,
                            ChatEvent::Error(e) => {
                                full_text = e;
                                had_error = true;
                                break;
                            }
                        }
                    }

                    // Apply final cleanup.
                    let clean = strip_markdown(&full_text);
                    stream_label.set_text(&clean);

                    if !had_error && !clean.is_empty() {
                        obj.finalize_assistant_row(&stream_outer, &clean);
                        obj.messages
                            .borrow_mut()
                            .push(("assistant".to_string(), full_text));
                    } else if had_error {
                        // Remove the failed user message from history.
                        obj.messages.borrow_mut().pop();
                    }

                    obj.loading_label.set_label("Thinking…");
                    obj.cancel_flag.borrow_mut().take();
                    obj.streaming_label.borrow_mut().take();
                    obj.set_loading(false);
                    obj.input_entry.grab_focus();
                }
            ));

            // Run the blocking Ollama call on a background thread.
            std::thread::spawn(move || {
                call_ollama_streaming(&context, &messages_snapshot, event_tx, cancel);
            });
        }
    }
}

glib::wrapper! {
    pub struct PpsSidebarChat(ObjectSubclass<imp::PpsSidebarChat>)
        @extends gtk::Widget, adw::Bin, PpsSidebarPage,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for PpsSidebarChat {
    fn default() -> Self {
        Self::new()
    }
}

impl PpsSidebarChat {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }
}
