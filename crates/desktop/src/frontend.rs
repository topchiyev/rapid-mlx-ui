use crate::view::*;
use eframe::egui::{self, Color32, RichText};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use std::sync::mpsc::{Receiver, SyncSender};
use std::time::{Duration, Instant};

const ACCENT: Color32 = Color32::from_rgb(91, 210, 193);
const MUTED: Color32 = Color32::from_rgb(148, 163, 184);

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Chat,
    Models,
    Tools,
    Mcp,
    Audio,
    Settings,
    Stats,
}

pub struct Frontend {
    tx: SyncSender<Command>,
    rx: Receiver<Snapshot>,
    state: Snapshot,
    page: Page,
    selected: String,
    draft: String,
    model: String,
    generation: Generation,
    tool_policy: ToolSettings,
    preferences: Preferences,
    preferences_loaded: bool,
    markdown: CommonMarkCache,
    dirty: Option<Instant>,
    search: String,
    pulled_only: bool,
    modality: String,
    mcp_form: McpForm,
    credential: String,
    confirmation: Option<(String, String, String)>,
    edit: Option<(String, String)>,
    title_edit: String,
    transcript_id: String,
    transcript_text: String,
    root_input: String,
    local_error: Option<String>,
    demo: bool,
    prompt_request: Option<(String, String, String)>,
    #[cfg(test)]
    composer_id: Option<egui::Id>,
}

impl Frontend {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        tx: SyncSender<Command>,
        rx: Receiver<Snapshot>,
        demo: bool,
    ) -> Self {
        Self::from_context(&cc.egui_ctx, tx, rx, demo)
    }

    fn from_context(
        ctx: &egui::Context,
        tx: SyncSender<Command>,
        rx: Receiver<Snapshot>,
        demo: bool,
    ) -> Self {
        let mut style = egui::Style {
            visuals: egui::Visuals::dark(),
            ..Default::default()
        };
        style.visuals.panel_fill = Color32::from_rgb(18, 24, 32);
        style.visuals.window_fill = Color32::from_rgb(24, 32, 43);
        style.visuals.extreme_bg_color = Color32::from_rgb(12, 18, 25);
        style.visuals.selection.bg_fill = Color32::from_rgb(35, 87, 87);
        style.visuals.hyperlink_color = ACCENT;
        style.visuals.widgets.active.bg_fill = Color32::from_rgb(43, 105, 99);
        style.spacing.item_spacing = egui::vec2(10.0, 10.0);
        style.spacing.button_padding = egui::vec2(12.0, 8.0);
        style.spacing.interact_size.y = 30.0;
        ctx.set_theme(egui::Theme::Dark);
        ctx.set_style_of(egui::Theme::Dark, style);
        Self {
            tx,
            rx,
            state: Snapshot::default(),
            page: Page::Chat,
            selected: String::new(),
            draft: String::new(),
            model: String::new(),
            generation: Generation::initial(),
            tool_policy: ToolSettings::default(),
            preferences: Preferences::initial(),
            preferences_loaded: false,
            markdown: CommonMarkCache::default(),
            dirty: None,
            search: String::new(),
            pulled_only: true,
            modality: "all".into(),
            mcp_form: McpForm {
                transport: "stdio".into(),
                environment: "{}".into(),
                ..McpForm::default()
            },
            credential: String::new(),
            confirmation: None,
            edit: None,
            title_edit: String::new(),
            transcript_id: String::new(),
            transcript_text: String::new(),
            root_input: String::new(),
            local_error: None,
            demo,
            prompt_request: None,
            #[cfg(test)]
            composer_id: None,
        }
    }

    fn send(&mut self, command: Command) {
        if let Err(error) = self.tx.try_send(command) {
            self.local_error = Some(format!(
                "Could not queue action: {error}. Please try again."
            ));
        }
    }

    fn save_current(&mut self) {
        if !self.selected.is_empty() {
            self.send(Command::SaveSession {
                id: self.selected.clone(),
                draft: self.draft.clone(),
                model: self.model.clone(),
                generation: self.generation.clone(),
            });
        }
        self.dirty = None;
    }

    fn select_session(&mut self, id: String) {
        self.save_current();
        self.selected = id;
        self.send(Command::FocusSession(self.selected.clone()));
        if let Some(session) = self
            .state
            .sessions
            .iter()
            .find(|session| session.id == self.selected)
        {
            self.draft = session.draft.clone();
            self.model = session.model.clone();
            self.generation = session.generation.clone();
            self.title_edit = session.title.clone();
            self.tool_policy = session.tools.clone();
        }
        self.page = Page::Chat;
    }

    fn poll(&mut self) {
        for _ in 0..32 {
            let Ok(state) = self.rx.try_recv() else { break };
            let old_draft = self
                .state
                .sessions
                .iter()
                .find(|s| s.id == self.selected)
                .map(|s| s.draft.clone());
            let old_transcript = self
                .state
                .transcripts
                .iter()
                .find(|t| t.id == self.transcript_id)
                .map(|t| t.text.clone());
            self.state = state;
            if self.dirty.is_none()
                && let Some(session) = self.state.sessions.iter().find(|s| s.id == self.selected)
                && old_draft.as_ref().is_some_and(|old| {
                    old != &session.draft && (old == &self.draft || self.draft.is_empty())
                })
            {
                self.draft = session.draft.clone();
            }
            if let Some(old) = old_transcript
                && self.transcript_text == old
                && let Some(transcript) = self
                    .state
                    .transcripts
                    .iter()
                    .find(|t| t.id == self.transcript_id)
            {
                self.transcript_text = transcript.text.clone();
            }
            if let Some((repo, message)) = self.state.deletion_preview.take() {
                self.confirmation = Some(("model".into(), repo, message));
            }
            if self.state.ready && !self.preferences_loaded {
                self.preferences = self.state.preferences.clone();
                self.preferences_loaded = true;
            }
            if !self.state.sessions.iter().any(|s| s.id == self.selected) {
                self.selected.clear();
                if let Some(session) = self
                    .state
                    .sessions
                    .iter()
                    .find(|s| s.id == self.state.focused_session)
                    .or_else(|| self.state.sessions.first())
                    .cloned()
                {
                    self.select_session(session.id);
                }
            }
            if self.transcript_id.is_empty()
                && let Some(transcript) = self.state.transcripts.first()
            {
                self.transcript_id = transcript.id.clone();
                self.transcript_text = transcript.text.clone();
            }
        }
        if self
            .dirty
            .is_some_and(|when| when.elapsed() >= Duration::from_millis(250))
        {
            self.save_current();
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(12.0);
        ui.label(
            RichText::new("RAPID / MLX")
                .color(ACCENT)
                .size(20.0)
                .strong(),
        );
        ui.label(RichText::new("LOCAL INTELLIGENCE").size(10.0).color(MUTED));
        ui.add_space(18.0);
        if ui
            .add_sized(
                [ui.available_width(), 36.0],
                egui::Button::new("+  New session").sense(if self.state.ready {
                    egui::Sense::click()
                } else {
                    egui::Sense::hover()
                }),
            )
            .clicked()
        {
            self.save_current();
            self.selected.clear();
            self.send(Command::NewSession);
        }
        ui.add_space(10.0);
        for (page, label) in [
            (Page::Chat, "Chat"),
            (Page::Models, "Models"),
            (Page::Audio, "Transcription"),
            (Page::Stats, "Statistics"),
            (Page::Tools, "OS tools"),
            (Page::Mcp, "MCP servers"),
            (Page::Settings, "Settings"),
        ] {
            if ui.selectable_label(self.page == page, label).clicked() {
                self.save_current();
                self.page = page;
            }
        }
        ui.separator();
        ui.label(RichText::new("SESSIONS").small().color(MUTED));
        let sessions = self.state.sessions.clone();
        egui::ScrollArea::vertical()
            .id_salt("sessions")
            .max_height((ui.available_height() - 110.0).max(80.0))
            .show(ui, |ui| {
                for session in sessions {
                    if ui
                        .selectable_label(self.selected == session.id, &session.title)
                        .clicked()
                    {
                        self.select_session(session.id);
                    }
                }
                if self.state.sessions.is_empty() {
                    ui.label(
                        RichText::new("Your conversations will appear here.")
                            .small()
                            .color(MUTED),
                    );
                }
            });
        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.label(RichText::new("Stored on this Mac").small().color(MUTED));
            ui.label(
                RichText::new(if self.demo {
                    "DEVELOPMENT FIXTURE"
                } else {
                    "Rust desktop · Local by default"
                })
                .small()
                .color(ACCENT),
            );
        });
    }

    fn inspector(&mut self, ui: &mut egui::Ui) {
        ui.add_space(16.0);
        ui.label(RichText::new("RUNTIME").small().color(MUTED));
        let runtime = self.state.chat.clone();
        ui.label(
            RichText::new(if runtime.state.is_empty() {
                "Disconnected"
            } else {
                &runtime.state
            })
            .strong(),
        );
        ui.label(if runtime.external {
            "External server"
        } else {
            "Managed by app"
        });
        if !runtime.model.is_empty() {
            ui.label(RichText::new(&runtime.model).small());
        }
        ui.horizontal_wrapped(|ui| {
            if ui.button("Connect").clicked() {
                self.send(Command::Runtime {
                    role: "chat".into(),
                    action: "connect".into(),
                    model: self.model.clone(),
                });
            }
            if ui
                .add_enabled(
                    !self.model.is_empty() && self.state.active.is_none(),
                    egui::Button::new("Load"),
                )
                .clicked()
            {
                self.save_current();
                self.send(Command::Runtime {
                    role: "chat".into(),
                    action: "start".into(),
                    model: self.model.clone(),
                });
            }
            if ui
                .add_enabled(
                    !runtime.external && self.state.active.is_none(),
                    egui::Button::new("Unload"),
                )
                .clicked()
            {
                self.send(Command::Runtime {
                    role: "chat".into(),
                    action: "stop".into(),
                    model: String::new(),
                });
            }
        });
        if runtime.external
            && ui
                .add_enabled(
                    self.state.active.is_none(),
                    egui::Button::new("Disconnect external server"),
                )
                .clicked()
        {
            self.send(Command::Runtime {
                role: "chat".into(),
                action: "stop".into(),
                model: String::new(),
            });
        }
        ui.add_space(20.0);
        ui.separator();
        ui.label(RichText::new("SYSTEM LOAD").small().color(MUTED));
        if let Some(cpu) = self.state.cpu {
            ui.label(RichText::new(format!("{cpu:.1}%")).size(28.0).color(ACCENT));
            ui.label(
                RichText::new("System CPU · sampled every 1s")
                    .small()
                    .color(MUTED),
            );
            sparkline(ui, &self.state.cpu_history);
        } else {
            ui.label("CPU · unavailable");
        }
        if let Some((used, total)) = self.state.memory {
            ui.label(format!("{:.1} / {:.1} GiB", gib(used), gib(total)));
            ui.add(
                egui::ProgressBar::new(if total > 0 {
                    used as f32 / total as f32
                } else {
                    0.0
                })
                .fill(ACCENT),
            );
            ui.label(RichText::new("System memory").small().color(MUTED));
        } else {
            ui.label("Memory · unavailable");
        }
        if !self.state.sample_time.is_empty() {
            ui.label(RichText::new(&self.state.sample_time).small().color(MUTED));
        }
        if self.state.metrics_stale {
            ui.colored_label(Color32::YELLOW, "Readings are stale");
        }
        ui.label(
            RichText::new("GPU utilization · unavailable")
                .small()
                .color(MUTED),
        );
        ui.label(
            RichText::new("No verified utilization provider.")
                .small()
                .color(MUTED),
        );
        for (name, cpu, bytes) in &self.state.processes {
            ui.label(
                RichText::new(format!(
                    "{name}: {} · {}",
                    cpu.map(|n| format!("{n:.1}%"))
                        .unwrap_or_else(|| "CPU unavailable".into()),
                    bytes
                        .map(|n| format!("{:.2} GiB", gib(n)))
                        .unwrap_or_else(|| "memory unavailable".into())
                ))
                .small(),
            );
        }
        ui.add_space(14.0);
        ui.separator();
        ui.label(RichText::new("SESSION USAGE").small().color(MUTED));
        let usage = self
            .state
            .sessions
            .iter()
            .find(|s| s.id == self.selected)
            .map(|s| s.usage.as_str())
            .unwrap_or("No reported usage yet");
        ui.label(usage);
        ui.label(
            RichText::new("Reported tokens include all attempts and tool continuations.")
                .small()
                .color(MUTED),
        );
        egui::CollapsingHeader::new("Runtime diagnostics").show(ui, |ui| {
            egui::ScrollArea::vertical()
                .max_height(200.0)
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("Third-party runtime output")
                            .small()
                            .color(MUTED),
                    );
                    for line in &runtime.logs {
                        ui.add(
                            egui::Label::new(RichText::new(line).monospace().size(10.0))
                                .selectable(true),
                        );
                    }
                });
        });
    }

    fn chat(&mut self, ui: &mut egui::Ui) {
        let Some(session) = self
            .state
            .sessions
            .iter()
            .find(|s| s.id == self.selected)
            .cloned()
        else {
            empty(
                ui,
                "A workspace for your local models",
                "Create a session, choose a pulled model, and start a conversation.",
            );
            if ui.button("Create your first session").clicked() {
                self.send(Command::NewSession);
            }
            return;
        };
        ui.horizontal(|ui| {
            if ui.text_edit_singleline(&mut self.title_edit).lost_focus() && self.title_edit != session.title && !self.title_edit.trim().is_empty() {
                self.send(Command::Rename { id: session.id.clone(), title: self.title_edit.clone() });
            }
            if ui.small_button("Delete session").clicked() { self.confirmation = Some(("session".into(), session.id.clone(), format!("Delete ‘{}’ and its local conversation history? Model files will be retained.", session.title))); }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Model");
            let before = self.model.clone();
            egui::ComboBox::from_id_salt("chat-model")
                .selected_text(if self.model.is_empty() {
                    "Choose a pulled model"
                } else {
                    &self.model
                })
                .width(240.0)
                .show_ui(ui, |ui| {
                    for model in self
                        .state
                        .models
                        .iter()
                        .filter(|m| (m.pulled && !m.catalog || m.served) && m.kind == "text")
                    {
                        ui.selectable_value(&mut self.model, model.id.clone(), &model.name);
                    }
                    if !self.model.is_empty()
                        && !self.state.models.iter().any(|m| m.id == self.model)
                    {
                        ui.label("Saved model is unavailable");
                    }
                });
            if before != self.model {
                self.save_current();
            }
            ui.label("Effort");
            let before = self.generation.effort.clone();
            let enabled =
                !self.state.effort_levels.is_empty() && self.model == self.state.chat.model;
            ui.add_enabled_ui(enabled, |ui| {
                egui::ComboBox::from_id_salt("effort")
                    .selected_text(self.generation.effort.as_deref().unwrap_or("Default"))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.generation.effort, None, "Default");
                        for level in &self.state.effort_levels {
                            ui.selectable_value(
                                &mut self.generation.effort,
                                Some(level.clone()),
                                level,
                            );
                        }
                    });
            })
            .response
            .on_disabled_hover_text("Effort needs verified runtime and model support.");
            if before != self.generation.effort {
                self.save_current();
            }
        });
        if !self.model.is_empty() && !self.state.models.iter().any(|m| m.id == self.model) {
            ui.colored_label(
                Color32::YELLOW,
                "The saved model is unavailable. Pull it or explicitly select another model.",
            );
        }
        if self.generation.effort.as_ref().is_some_and(|value| {
            !self.state.effort_levels.contains(value) || self.model != self.state.chat.model
        }) {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(
                    Color32::YELLOW,
                    "Saved effort is unavailable for the selected runtime/model.",
                );
                if ui.button("Use default effort").clicked() {
                    self.generation.effort = None;
                    self.save_current();
                }
            });
        }
        if !self.state.effort_note.is_empty() {
            ui.label(RichText::new(&self.state.effort_note).small().color(MUTED));
        }
        egui::CollapsingHeader::new("Generation settings").show(ui, |ui| {
            let mut changed = false;
            ui.horizontal_wrapped(|ui| {
                ui.label("Temperature");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut self.generation.temperature)
                            .range(0.0..=2.0)
                            .speed(0.01),
                    )
                    .changed();
                ui.label("Top-p");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut self.generation.top_p)
                            .range(0.01..=1.0)
                            .speed(0.01),
                    )
                    .changed();
                ui.label("Max output tokens");
                changed |= ui
                    .add(egui::DragValue::new(&mut self.generation.max_tokens).range(1..=131072))
                    .changed();
            });
            ui.label("System prompt");
            changed |= ui
                .add(
                    egui::TextEdit::multiline(&mut self.generation.system_prompt)
                        .desired_width(f32::INFINITY)
                        .desired_rows(2),
                )
                .changed();
            if changed {
                self.save_current();
            }
        });
        ui.separator();
        egui::Panel::bottom("composer").show(ui, |ui| {
            if session.unresolved { ui.colored_label(Color32::YELLOW, "Resolve the last turn with Retry, Edit, or an earlier branch before continuing."); }
            let response = ui.add(egui::TextEdit::multiline(&mut self.draft).id_salt("composer-input").return_key(egui::KeyboardShortcut::new(egui::Modifiers::SHIFT, egui::Key::Enter)).hint_text("Message your local model…").desired_width(f32::INFINITY).desired_rows(3));
            #[cfg(test)] { self.composer_id = Some(response.id); }
            if response.changed() { self.dirty = Some(Instant::now()); }
            let enter = response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter) && !input.modifiers.shift);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Enter to send · Shift+Enter for a new line").small().color(MUTED));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.state.active.is_some() {
                        if ui.button("■  Stop").clicked() { self.send(Command::Cancel); }
                        ui.spinner();
                    } else {
                        let enabled = !self.draft.trim().is_empty() && !self.model.is_empty() && !session.unresolved && self.state.chat.state.starts_with("Ready") && self.state.can_send && self.generation.effort.as_ref().is_none_or(|value| self.state.effort_levels.contains(value) && self.model == self.state.chat.model);
                        let clicked = ui.add_enabled(enabled, egui::Button::new(RichText::new("Send  ↗").color(ACCENT))).clicked();
                        if enabled && (clicked || enter) { self.save_current(); self.send(Command::Send { id: session.id.clone(), text: self.draft.trim().to_owned() }); self.draft.clear(); }
                    }
                });
            });
        });
        egui::ScrollArea::vertical()
            .id_salt(("conversation", &session.id))
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if session.messages.is_empty() {
                    empty(
                        ui,
                        "Start a conversation",
                        "Your messages and model responses stay on this Mac.",
                    );
                }
                for message in &session.messages {
                    ui.push_id(&message.id, |ui| {
                        egui::Frame::new()
                            .fill(if message.role == "user" {
                                Color32::from_rgb(26, 39, 48)
                            } else {
                                Color32::from_rgb(22, 29, 39)
                            })
                            .inner_margin(16.0)
                            .corner_radius(8.0)
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(if message.role == "user" {
                                            "YOU"
                                        } else if message.role == "tool" {
                                            "TOOL"
                                        } else {
                                            "ASSISTANT"
                                        })
                                        .color(ACCENT)
                                        .strong()
                                        .small(),
                                    );
                                    ui.label(RichText::new(&message.status).small().color(MUTED));
                                    if !message.model.is_empty() {
                                        ui.label(
                                            RichText::new(&message.model).small().color(MUTED),
                                        );
                                    }
                                    if ui.small_button("Copy").clicked() {
                                        ui.ctx().copy_text(message.content.clone());
                                    }
                                });
                                if !message.reasoning.is_empty() {
                                    egui::CollapsingHeader::new("Reasoning")
                                        .id_salt("reasoning")
                                        .show(ui, |ui| {
                                            ui.add(
                                                egui::Label::new(&message.reasoning)
                                                    .selectable(true),
                                            );
                                        });
                                }
                                CommonMarkViewer::new().show(
                                    ui,
                                    &mut self.markdown,
                                    &message.content,
                                );
                                if let Some(error) = &message.error {
                                    ui.colored_label(Color32::LIGHT_RED, error);
                                }
                                if !message.usage.is_empty() {
                                    ui.label(RichText::new(&message.usage).small().color(MUTED));
                                }
                                ui.horizontal_wrapped(|ui| {
                                    if message.role == "user"
                                        && ui.small_button("Edit prompt").clicked()
                                    {
                                        self.edit =
                                            Some((message.id.clone(), message.content.clone()));
                                    }
                                    if message.role == "assistant"
                                        && ui
                                            .add_enabled(
                                                self.state.active.is_none(),
                                                egui::Button::new("Retry").small(),
                                            )
                                            .clicked()
                                    {
                                        self.save_current();
                                        self.send(Command::Retry {
                                            id: session.id.clone(),
                                            message: message.id.clone(),
                                        });
                                    }
                                    if ui.small_button("Branch here").clicked() {
                                        self.send(Command::Branch {
                                            id: session.id.clone(),
                                            head: message.id.clone(),
                                        });
                                    }
                                    for (head, label) in &message.alternatives {
                                        if ui.small_button(label).clicked() {
                                            self.send(Command::Branch {
                                                id: session.id.clone(),
                                                head: head.clone(),
                                            });
                                        }
                                    }
                                });
                            });
                        ui.add_space(6.0);
                    });
                }
            });
    }

    fn models(&mut self, ui: &mut egui::Ui) {
        heading(
            ui,
            "Model library",
            "Browse the runtime catalog and models already on this Mac.",
        );
        if !self.state.catalog_source.is_empty() {
            ui.label(
                RichText::new(&self.state.catalog_source)
                    .small()
                    .color(MUTED),
            );
        }
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.pulled_only, true, "Pulled");
            ui.selectable_value(&mut self.pulled_only, false, "Available");
            ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Search models"));
            egui::ComboBox::from_id_salt("model-kind")
                .selected_text(&self.modality)
                .show_ui(ui, |ui| {
                    for kind in [
                        "all",
                        "text",
                        "stt",
                        "tts",
                        "image-gen",
                        "video-gen",
                        "aligner",
                    ] {
                        ui.selectable_value(&mut self.modality, kind.into(), kind);
                    }
                });
            if ui.button("Refresh").clicked() {
                self.send(Command::RefreshModels);
            }
        });
        for key in self.state.operations.clone() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("Transfer in progress · {key}"));
                if ui.button("Cancel transfer").clicked() {
                    self.send(Command::CancelOperation(key));
                }
            });
        }
        ui.separator();
        let models: Vec<_> = self
            .state
            .models
            .iter()
            .filter(|m| {
                (if self.pulled_only {
                    m.pulled && !m.catalog
                } else {
                    m.catalog
                }) && (self.modality == "all" || m.kind == self.modality)
                    && format!("{} {}", m.name, m.repo)
                        .to_lowercase()
                        .contains(&self.search.to_lowercase())
            })
            .cloned()
            .collect();
        egui::ScrollArea::vertical().show(ui, |ui| {
            if models.is_empty() {
                empty(
                    ui,
                    "No matching models",
                    "Refresh the catalog or add a local model directory in Settings.",
                );
            }
            for model in models {
                egui::Frame::group(ui.style())
                    .inner_margin(14.0)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(&model.name).strong().size(16.0));
                        ui.label(
                            RichText::new(format!(
                                "{} · {} · {}",
                                model.kind,
                                model.size,
                                if model.catalog && model.pulled {
                                    "Available · repository cached; verify exact variant"
                                } else if model.pulled {
                                    "Pulled"
                                } else {
                                    "Available to pull"
                                }
                            ))
                            .color(MUTED),
                        );
                        ui.add(egui::Label::new(&model.repo).selectable(true));
                        if !model.revision.is_empty() {
                            ui.label(RichText::new(format!("Revision {}", model.revision)).small());
                        }
                        if !model.path.as_os_str().is_empty() {
                            ui.add(
                                egui::Label::new(model.path.display().to_string()).selectable(true),
                            );
                        }
                        if !model.detail.is_empty() {
                            ui.label(RichText::new(&model.detail).small().color(MUTED));
                        }
                        ui.horizontal_wrapped(|ui| {
                            if model.catalog {
                                if ui.button("Pull selected variant").clicked() {
                                    self.send(Command::Pull(model.id.clone()));
                                }
                            } else if model.pulled {
                                if ui
                                    .add_enabled(
                                        model.compatible
                                            && !model.path.as_os_str().is_empty()
                                            && model.kind == "text",
                                        egui::Button::new("Select for chat"),
                                    )
                                    .clicked()
                                {
                                    self.model = model.id.clone();
                                    self.save_current();
                                    self.page = Page::Chat;
                                }
                                if model.kind == "stt"
                                    && ui.button("Use for transcription").clicked()
                                {
                                    self.preferences.asr_model = model.id.clone();
                                    self.send(Command::Preferences(self.preferences.clone()));
                                    self.page = Page::Audio;
                                }
                                if ui.button("Delete pulled model").clicked() {
                                    self.send(Command::PreviewDeletion(model.repo.clone()));
                                }
                            } else if ui.button("Pull model").clicked() {
                                self.send(Command::Pull(model.id.clone()));
                            }
                        });
                    });
            }
        });
    }

    fn tools(&mut self, ui: &mut egui::Ui) {
        heading(
            ui,
            "OS tools",
            "Give this session a working directory and explicit execution capabilities.",
        );
        if self.selected.is_empty() {
            ui.label("Create or select a session first.");
            return;
        }
        let mut changed = ui
            .checkbox(
                &mut self.tool_policy.enabled,
                "Enable filesystem tools for this session",
            )
            .changed();
        ui.label("Working directory / allowed filesystem root");
        changed |= ui
            .text_edit_singleline(&mut self.tool_policy.directory)
            .changed();
        ui.horizontal(|ui| {
            if ui.button("Choose directory…").clicked()
                && let Some(path) = rfd::FileDialog::new().pick_folder()
            {
                self.tool_policy.directory = path.display().to_string();
                changed = true;
            }
        });
        changed |= ui
            .checkbox(
                &mut self.tool_policy.writes,
                "Allow file creation and writes within the selected root",
            )
            .changed();
        changed |= ui
            .checkbox(
                &mut self.tool_policy.commands,
                "Allow explicit commands in this working directory",
            )
            .changed();
        if self.tool_policy.commands {
            ui.label("Allowed executables (absolute paths, one per line; commands can have side effects)");
            changed |= ui
                .add(
                    egui::TextEdit::multiline(&mut self.tool_policy.executables)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                )
                .changed();
        }
        if changed {
            self.send(Command::Tools {
                session: self.selected.clone(),
                policy: self.tool_policy.clone(),
            });
        }
        ui.separator();
        for (name, description) in [
            ("List directory", "Inspect names and file types."),
            ("Read file", "Read bounded text from an allowed file."),
            (
                "Search",
                "Find filenames and text within the selected root.",
            ),
            (
                "Write file",
                "Create or replace a file when writes are enabled.",
            ),
            (
                "Run command",
                "Explicit executable and arguments, bounded time and output.",
            ),
        ] {
            ui.label(RichText::new(name).strong());
            ui.label(RichText::new(description).color(MUTED));
        }
        ui.add_space(16.0);
        ui.label("Tools run only from complete structured model calls. Calls and results appear in the conversation. A command working directory is not an OS sandbox.");
    }

    fn mcp(&mut self, ui: &mut egui::Ui) {
        heading(
            ui,
            "MCP servers",
            "Connect local or remote servers and select tools for the current session.",
        );
        egui::ScrollArea::vertical().show(ui, |ui| {
            for server in self.state.registrations.clone() {
                let id = server.registration.id.clone();
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.label(RichText::new(&server.registration.name).strong());
                    ui.label(&server.state);
                    ui.label(
                        RichText::new(&server.registration.target)
                            .small()
                            .color(MUTED),
                    );
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Connect").clicked() {
                            self.send(Command::Mcp {
                                id: id.clone(),
                                action: "connect".into(),
                            });
                        }
                        if ui.button("Disconnect").clicked() {
                            self.send(Command::Mcp {
                                id: id.clone(),
                                action: "disconnect".into(),
                            });
                        }
                        if ui.button("Refresh discovery").clicked() {
                            self.send(Command::Mcp {
                                id: id.clone(),
                                action: "refresh".into(),
                            });
                        }
                        if ui.button("Edit").clicked() {
                            self.mcp_form = server.registration.clone();
                        }
                        if ui.button("Remove").clicked() {
                            self.send(Command::Mcp {
                                id: id.clone(),
                                action: "remove".into(),
                            });
                        }
                    });
                    if server.registration.transport == "http" {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.credential)
                                    .password(true)
                                    .hint_text("Current-run bearer token (optional)"),
                            );
                            if ui.button("Set token").clicked() {
                                let value = std::mem::take(&mut self.credential);
                                self.send(Command::Credential {
                                    role: id.clone(),
                                    value,
                                });
                            }
                        });
                    }
                    for (name, description, _) in &server.tools {
                        let mut enabled = self
                            .state
                            .sessions
                            .iter()
                            .find(|s| s.id == self.selected)
                            .is_some_and(|s| s.selected_tools.contains(name));
                        if ui
                            .checkbox(&mut enabled, format!("{name} — {description}"))
                            .changed()
                        {
                            self.send(Command::SelectTool {
                                session: self.selected.clone(),
                                server: id.clone(),
                                tool: name.clone(),
                                enabled,
                            });
                        }
                    }
                    for (kind, items) in
                        [("resource", &server.resources), ("prompt", &server.prompts)]
                    {
                        for (name, label) in items {
                            if ui.button(format!("Insert {kind}: {label}")).clicked() {
                                if kind == "prompt" {
                                    self.prompt_request =
                                        Some((id.clone(), name.clone(), "{}".into()));
                                } else {
                                    self.send(Command::McpContext {
                                        session: self.selected.clone(),
                                        server: id.clone(),
                                        kind: kind.into(),
                                        name: name.clone(),
                                        arguments: serde_json::json!({}),
                                    });
                                }
                            }
                        }
                    }
                });
            }
            ui.separator();
            ui.heading(if self.mcp_form.id.is_empty() {
                "Register a server"
            } else {
                "Edit registration"
            });
            ui.label("Name");
            ui.text_edit_singleline(&mut self.mcp_form.name);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.mcp_form.transport, "stdio".into(), "Local stdio");
                ui.selectable_value(
                    &mut self.mcp_form.transport,
                    "http".into(),
                    "Streamable HTTP",
                );
            });
            ui.label(if self.mcp_form.transport == "stdio" {
                "Executable"
            } else {
                "Endpoint URL"
            });
            ui.text_edit_singleline(&mut self.mcp_form.target);
            if self.mcp_form.transport == "stdio" {
                ui.label("Arguments (JSON array)");
                ui.text_edit_singleline(&mut self.mcp_form.arguments);
                ui.label("Environment references (JSON: variable → source variable)");
                ui.text_edit_singleline(&mut self.mcp_form.environment);
            } else {
                ui.label("Bearer token environment reference (optional)");
                ui.text_edit_singleline(&mut self.mcp_form.credential_env);
            }
            ui.checkbox(&mut self.mcp_form.enabled, "Enabled");
            if ui.button("Save registration").clicked() {
                self.send(Command::SaveMcp(self.mcp_form.clone()));
                self.mcp_form = McpForm {
                    transport: "stdio".into(),
                    environment: "{}".into(),
                    ..McpForm::default()
                };
            }
        });
    }

    fn audio(&mut self, ui: &mut egui::Ui) {
        heading(
            ui,
            "Transcription",
            "Turn audio into editable text using a local speech-to-text model.",
        );
        ui.horizontal_wrapped(|ui| {
            let previous = self.preferences.asr_model.clone();
            egui::ComboBox::from_id_salt("asr-model")
                .selected_text(if self.preferences.asr_model.is_empty() {
                    "Choose a transcription model"
                } else {
                    &self.preferences.asr_model
                })
                .show_ui(ui, |ui| {
                    for model in self.state.models.iter().filter(|m| m.kind == "stt") {
                        ui.selectable_value(
                            &mut self.preferences.asr_model,
                            model.id.clone(),
                            format!(
                                "{} {}",
                                model.name,
                                if model.pulled {
                                    "(pulled)"
                                } else {
                                    "(available)"
                                }
                            ),
                        );
                    }
                });
            if previous != self.preferences.asr_model {
                self.send(Command::Preferences(self.preferences.clone()));
            }
            if ui.button("Pull").clicked() {
                self.send(Command::Pull(self.preferences.asr_model.clone()));
            }
            if ui.button("Load").clicked() {
                self.send(Command::Runtime {
                    role: "asr".into(),
                    action: "start".into(),
                    model: self.preferences.asr_model.clone(),
                });
            }
            if ui.button("Unload").clicked() {
                self.send(Command::Runtime {
                    role: "asr".into(),
                    action: "stop".into(),
                    model: String::new(),
                });
            }
            if ui.button("Connect external").clicked() {
                self.send(Command::Runtime {
                    role: "asr".into(),
                    action: "connect".into(),
                    model: self.preferences.asr_model.clone(),
                });
            }
            if self.state.asr.external && ui.button("Disconnect external").clicked() {
                self.send(Command::Runtime {
                    role: "asr".into(),
                    action: "stop".into(),
                    model: String::new(),
                });
            }
            if let Some(model) = self
                .state
                .models
                .iter()
                .find(|m| m.id == self.preferences.asr_model)
                && model.pulled
                && !model.catalog
                && !model.repo.is_empty()
                && ui.button("Delete pulled model…").clicked()
            {
                self.send(Command::PreviewDeletion(model.repo.clone()));
            }
        });
        ui.label(format!("Transcription runtime: {}", self.state.asr.state));
        ui.horizontal(|ui| {
            ui.label("Language code (blank = automatic)");
            if ui
                .text_edit_singleline(&mut self.preferences.asr_language)
                .changed()
            {
                self.send(Command::Preferences(self.preferences.clone()));
            }
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.state.active.is_none(),
                    egui::Button::new("Import WAV audio…"),
                )
                .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("WAV audio", &["wav"])
                    .pick_file()
            {
                self.send(Command::Transcribe(path));
            }
            if self.state.recording {
                if ui.button("■ Stop recording").clicked() {
                    self.send(Command::StopRecording);
                }
                ui.colored_label(Color32::LIGHT_RED, "Recording microphone");
            } else if ui
                .add_enabled(
                    self.state.active.is_none(),
                    egui::Button::new("● Record microphone"),
                )
                .clicked()
            {
                self.send(Command::Record);
            }
            if self.state.active.is_some() && ui.button("Cancel job").clicked() {
                self.send(Command::Cancel);
            }
        });
        ui.separator();
        for transcript in self.state.transcripts.clone() {
            if ui
                .selectable_label(
                    self.transcript_id == transcript.id,
                    format!(
                        "{} · {} · {}",
                        transcript.model, transcript.status, transcript.duration
                    ),
                )
                .clicked()
            {
                self.transcript_id = transcript.id;
                self.transcript_text = transcript.text;
            }
        }
        if self.state.transcripts.is_empty() {
            empty(
                ui,
                "Your transcript will appear here",
                "Import a recording or record from the microphone. Audio stays local.",
            );
        } else {
            ui.add(
                egui::TextEdit::multiline(&mut self.transcript_text)
                    .desired_rows(14)
                    .desired_width(f32::INFINITY),
            );
            ui.horizontal_wrapped(|ui| {
                if ui.button("Save transcript edits").clicked() { self.send(Command::SaveTranscript { id: self.transcript_id.clone(), text: self.transcript_text.clone() }); }
                if ui.button("Copy transcript").clicked() { ui.ctx().copy_text(self.transcript_text.clone()); }
                if ui.add_enabled(!self.selected.is_empty(), egui::Button::new("Append to chat draft")).clicked() { if !self.draft.is_empty() { self.draft.push('\n'); } self.draft.push_str(&self.transcript_text); self.save_current(); self.send(Command::LinkTranscript { session: self.selected.clone(), transcript: self.transcript_id.clone() }); self.page = Page::Chat; }
                if ui.add_enabled(!self.selected.is_empty(), egui::Button::new("Replace chat draft")).clicked() { self.confirmation = Some(("draft".into(), self.transcript_id.clone(), "Replace the current chat draft with this transcript? It will not be sent automatically.".into())); }
            });
        }
    }

    fn settings(&mut self, ui: &mut egui::Ui) {
        heading(
            ui,
            "Settings",
            "Local services, model locations, and application storage.",
        );
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.label("rapid-mlx executable"); ui.text_edit_singleline(&mut self.preferences.executable);
            ui.label("Chat endpoint (loopback only)"); ui.text_edit_singleline(&mut self.preferences.chat_endpoint);
            ui.label("Transcription endpoint (loopback only)"); ui.text_edit_singleline(&mut self.preferences.asr_endpoint);
            ui.label("Hugging Face cache override (blank uses environment/default)"); ui.text_edit_singleline(&mut self.preferences.cache);
            ui.label("Additional local model folders");
            for root in &self.preferences.model_roots { ui.label(root.display().to_string()); }
            ui.horizontal(|ui| { ui.text_edit_singleline(&mut self.root_input); if ui.button("Add folder").clicked() && !self.root_input.trim().is_empty() { self.preferences.model_roots.push(self.root_input.trim().into()); self.root_input.clear(); } });
            if ui.button("Clear additional folders").clicked() { self.preferences.model_roots.clear(); }
            ui.checkbox(&mut self.preferences.retain_audio, "Retain app-recorded audio after transcription");
            if ui.button("Save preferences").clicked() { self.send(Command::Preferences(self.preferences.clone())); }
            ui.separator(); ui.label("API credential (current app run only)");
            ui.add(egui::TextEdit::singleline(&mut self.credential).password(true));
            ui.horizontal(|ui| { for role in ["chat", "asr"] { if ui.button(format!("Set {role} credential")).clicked() { let value = std::mem::take(&mut self.credential); self.send(Command::Credential { role: role.into(), value }); } } });
            ui.separator(); ui.label(RichText::new("APPLICATION STATE").small().color(MUTED));
            ui.add(egui::Label::new(&self.state.state_root).selectable(true));
            ui.label("Sessions, drafts, registrations, transcripts, staging, and backups use this fixed home-relative location. Model caches retain their configured locations.");
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some((server, name, mut arguments)) = self.prompt_request.clone() {
            let mut close = false;
            egui::Window::new("MCP prompt arguments")
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(&name);
                    ui.label("Arguments as a JSON object");
                    ui.add(egui::TextEdit::multiline(&mut arguments).desired_rows(4));
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ui.button("Insert selected prompt").clicked() {
                            match serde_json::from_str::<serde_json::Value>(&arguments) {
                                Ok(arguments) if arguments.is_object() => {
                                    self.send(Command::McpContext {
                                        session: self.selected.clone(),
                                        server: server.clone(),
                                        kind: "prompt".into(),
                                        name: name.clone(),
                                        arguments,
                                    });
                                    close = true;
                                }
                                _ => {
                                    self.local_error =
                                        Some("Prompt arguments must be a JSON object.".into())
                                }
                            }
                        }
                    });
                });
            self.prompt_request = if close {
                None
            } else {
                Some((server, name, arguments))
            };
        }
        if let Some((kind, id, message)) = self.confirmation.clone() {
            egui::Window::new("Confirm action")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.set_max_width(430.0);
                    ui.label(message);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.confirmation = None;
                        }
                        if ui.button("Confirm").clicked() {
                            match kind.as_str() {
                                "session" => self.send(Command::DeleteSession(id)),
                                "model" => self.send(Command::DeleteModel(id)),
                                "draft" => {
                                    self.draft = self.transcript_text.clone();
                                    self.save_current();
                                    self.send(Command::LinkTranscript {
                                        session: self.selected.clone(),
                                        transcript: id,
                                    });
                                    self.page = Page::Chat;
                                }
                                _ => {}
                            }
                            self.confirmation = None;
                        }
                    });
                });
        }
        if let Some((id, mut text)) = self.edit.clone() {
            let mut close = false;
            egui::Window::new("Edit prompt · new branch")
                .resizable(true)
                .show(ctx, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut text)
                            .desired_rows(6)
                            .desired_width(480.0),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ui.button("Create branch and send").clicked() {
                            self.send(Command::Edit {
                                id: self.selected.clone(),
                                message: id.clone(),
                                text: text.clone(),
                            });
                            close = true;
                        }
                    });
                });
            self.edit = if close { None } else { Some((id, text)) };
        }
    }
}

impl Frontend {
    fn render(&mut self, ui: &mut egui::Ui) {
        let first = !self.preferences_loaded;
        self.poll();
        if first
            && self.preferences_loaded
            && let Some([width, height]) = self.preferences.window_size
        {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                    width.max(960.0),
                    height.max(640.0),
                )));
        }
        if ui.input(|i| i.viewport().close_requested()) {
            self.save_current();
            self.preferences.window_size = ui.input(|i| {
                i.viewport()
                    .inner_rect
                    .map(|rect| [rect.width(), rect.height()])
            });
            if let Some(size) = self.preferences.window_size {
                self.send(Command::WindowSize(size));
            }
            self.send(Command::Shutdown);
        }
        egui::Panel::bottom("status").show(ui, |ui| {
            if let Some(error) = self
                .local_error
                .clone()
                .or_else(|| self.state.error.clone())
            {
                ui.horizontal(|ui| {
                    ui.colored_label(Color32::LIGHT_RED, error);
                    if ui.small_button("Dismiss").clicked() {
                        self.local_error = None;
                        self.state.error = None;
                    }
                });
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new(&self.state.status).small().color(MUTED));
                if self.demo {
                    ui.colored_label(ACCENT, "Demo fixtures · no real operations");
                }
            });
        });
        egui::Panel::left("navigation")
            .default_size(205.0)
            .min_size(170.0)
            .max_size(280.0)
            .show(ui, |ui| self.sidebar(ui));
        if ui.available_width() >= 850.0 {
            egui::Panel::right("inspector")
                .default_size(245.0)
                .min_size(220.0)
                .max_size(320.0)
                .show(ui, |ui| self.inspector(ui));
        }
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(10.0);
            if !self.state.ready {
                if self.state.error.is_some() {
                    heading(
                        ui,
                        "Application state is unavailable",
                        "Resolve the storage error below. No alternate data location was used.",
                    );
                } else {
                    ui.spinner();
                    heading(
                        ui,
                        "Restoring local state…",
                        "Loading saved sessions and preferences.",
                    );
                }
                return;
            }
            match self.page {
                Page::Chat => self.chat(ui),
                Page::Models => self.models(ui),
                Page::Tools => self.tools(ui),
                Page::Mcp => self.mcp(ui),
                Page::Audio => self.audio(ui),
                Page::Settings => self.settings(ui),
                Page::Stats => {
                    heading(
                        ui,
                        "Statistics",
                        "System and owned-process measurements, plus session usage.",
                    );
                    egui::ScrollArea::vertical().show(ui, |ui| self.inspector(ui));
                }
            }
        });
        self.dialogs(ui.ctx());
        ui.ctx().request_repaint_after(
            if self.state.active.is_some() || self.dirty.is_some() || self.state.recording {
                Duration::from_millis(100)
            } else {
                Duration::from_secs(1)
            },
        );
    }
}

fn heading(ui: &mut egui::Ui, title: &str, detail: &str) {
    ui.heading(title);
    ui.label(RichText::new(detail).color(MUTED));
    ui.add_space(12.0);
}
fn empty(ui: &mut egui::Ui, title: &str, detail: &str) {
    ui.add_space(30.0);
    ui.label(RichText::new(title).size(23.0).strong());
    ui.label(RichText::new(detail).color(MUTED));
    ui.add_space(20.0);
}
fn gib(bytes: u64) -> f64 {
    bytes as f64 / 1_073_741_824.0
}
fn sparkline(ui: &mut egui::Ui, samples: &[f32]) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 55.0), egui::Sense::hover());
    if samples.len() >= 2 {
        let points = samples
            .iter()
            .enumerate()
            .map(|(i, v)| {
                egui::pos2(
                    rect.left() + rect.width() * i as f32 / (samples.len() - 1) as f32,
                    rect.bottom() - rect.height() * (v / 100.0).clamp(0.0, 1.0),
                )
            })
            .collect::<Vec<_>>();
        ui.painter()
            .add(egui::Shape::line(points, egui::Stroke::new(1.7, ACCENT)));
    }
}

impl eframe::App for Frontend {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render(ui);
    }
}

#[cfg(test)]
#[path = "frontend_tests.rs"]
mod tests;
