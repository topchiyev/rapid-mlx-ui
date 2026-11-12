//! Headless egui input tests; no native input injection or application data writes.
use super::*;
use std::sync::mpsc::{self, Receiver};

fn fixture() -> (egui::Context, Frontend, Receiver<Command>) {
    let ctx = egui::Context::default();
    let (tx, commands) = mpsc::sync_channel(128);
    let (updates, rx) = mpsc::sync_channel(16);
    updates.send(crate::demo::snapshot()).unwrap();
    let app = Frontend::from_context(&ctx, tx, rx, true);
    (ctx, app, commands)
}

fn frame(
    ctx: &egui::Context,
    app: &mut Frontend,
    size: [f32; 2],
    mut events: Vec<egui::Event>,
    modifiers: egui::Modifiers,
) -> egui::FullOutput {
    events.insert(0, egui::Event::ModifiersChanged(modifiers));
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(size[0], size[1]),
        )),
        events,
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |ui| app.render(ui));
    output.textures_delta.clear(); // Headless tests intentionally have no GPU texture sink.
    output
}
fn enter(modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: Some(egui::Key::Enter),
        pressed: true,
        repeat: false,
        modifiers,
    }
}

#[test]
fn keyboard_composer_distinguishes_send_from_multiline() {
    let (ctx, mut app, commands) = fixture();
    frame(
        &ctx,
        &mut app,
        [1320.0, 860.0],
        vec![],
        egui::Modifiers::NONE,
    );
    ctx.memory_mut(|memory| memory.request_focus(app.composer_id.unwrap()));
    frame(
        &ctx,
        &mut app,
        [1320.0, 860.0],
        vec![egui::Event::Text("First line".into())],
        egui::Modifiers::NONE,
    );
    assert_eq!(app.draft, "First line");
    frame(
        &ctx,
        &mut app,
        [1320.0, 860.0],
        vec![enter(egui::Modifiers::SHIFT)],
        egui::Modifiers::SHIFT,
    );
    frame(
        &ctx,
        &mut app,
        [1320.0, 860.0],
        vec![egui::Event::Text("Second line".into())],
        egui::Modifiers::NONE,
    );
    assert_eq!(app.draft, "First line\nSecond line");
    assert!(
        !commands
            .try_iter()
            .any(|command| matches!(command, Command::Send { .. }))
    );
    frame(
        &ctx,
        &mut app,
        [1320.0, 860.0],
        vec![enter(egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
    );
    let sent = commands
        .try_iter()
        .find_map(|command| match command {
            Command::Send { id, text } => Some((id, text)),
            _ => None,
        })
        .expect("bare Enter must queue a send");
    assert_eq!(sent.0, "demo-session");
    assert_eq!(sent.1, "First line\nSecond line");
    assert!(app.draft.is_empty());
}

#[test]
fn narrow_and_wide_pages_render_empty_error_and_partial_markdown_states() {
    let (ctx, mut app, _) = fixture();
    for size in [[960.0, 640.0], [1320.0, 860.0]] {
        for page in [
            Page::Chat,
            Page::Models,
            Page::Audio,
            Page::Stats,
            Page::Tools,
            Page::Mcp,
            Page::Settings,
        ] {
            app.page = page;
            let output = frame(&ctx, &mut app, size, vec![], egui::Modifiers::NONE);
            assert!(!output.shapes.is_empty());
        }
    }
    app.page = Page::Chat;
    app.state.sessions[0].messages[1].content =
        "Incomplete code while streaming:\n```rust\nfn main() {".into();
    app.state.sessions[0].messages[1].status = "streaming".into();
    app.state.active = Some("Synthetic stream".into());
    frame(
        &ctx,
        &mut app,
        [960.0, 640.0],
        vec![],
        egui::Modifiers::NONE,
    );
    app.state.error = Some("Synthetic connection failure".into());
    frame(
        &ctx,
        &mut app,
        [960.0, 640.0],
        vec![],
        egui::Modifiers::NONE,
    );
    app.state.sessions.clear();
    frame(
        &ctx,
        &mut app,
        [960.0, 640.0],
        vec![],
        egui::Modifiers::NONE,
    );
}

#[test]
fn disconnected_runtime_never_dispatches_from_enter() {
    let (ctx, mut app, commands) = fixture();
    frame(
        &ctx,
        &mut app,
        [1320.0, 860.0],
        vec![],
        egui::Modifiers::NONE,
    );
    ctx.memory_mut(|memory| memory.request_focus(app.composer_id.unwrap()));
    app.state.chat.state = "Stopped".into();
    app.draft = "Keep this unsent".into();
    frame(
        &ctx,
        &mut app,
        [1320.0, 860.0],
        vec![enter(egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
    );
    assert_eq!(app.draft, "Keep this unsent");
    assert!(
        !commands
            .try_iter()
            .any(|command| matches!(command, Command::Send { .. }))
    );
}

fn asr_models() -> Vec<ModelView> {
    [
        (
            "local@revision",
            "Local Whisper",
            "org/whisper",
            "stt",
            true,
            false,
            false,
        ),
        (
            "catalog",
            "Whisper",
            "org/whisper",
            "stt",
            true,
            true,
            false,
        ),
        (
            "available",
            "Parakeet",
            "org/parakeet",
            "stt",
            false,
            true,
            false,
        ),
        ("external", "Served Whisper", "", "stt", false, false, true),
        (
            "chat",
            "Whisper",
            "org/whisper-chat",
            "text",
            true,
            false,
            false,
        ),
        (
            "tts",
            "Whisper",
            "org/whisper-tts",
            "tts",
            true,
            false,
            false,
        ),
    ]
    .into_iter()
    .map(
        |(id, name, repo, kind, pulled, catalog, served)| ModelView {
            id: id.into(),
            name: name.into(),
            repo: repo.into(),
            kind: kind.into(),
            pulled,
            catalog,
            served,
            ..Default::default()
        },
    )
    .collect()
}

#[test]
fn asr_search_combines_with_status_and_preserves_exact_local_identity() {
    let models = asr_models();
    for (query, availability, expected) in [
        (
            "",
            AsrAvailability::All,
            vec!["local@revision", "catalog", "available", "external"],
        ),
        (
            "  wHiSpEr  ",
            AsrAvailability::All,
            vec!["local@revision", "catalog", "external"],
        ),
        (
            "org/PARAKEET",
            AsrAvailability::Available,
            vec!["available"],
        ),
        ("REVISION", AsrAvailability::Pulled, vec!["local@revision"]),
        ("whisper", AsrAvailability::Pulled, vec!["local@revision"]),
        ("whisper", AsrAvailability::Available, vec!["catalog"]),
        ("whisper", AsrAvailability::Served, vec!["external"]),
        ("parakeet", AsrAvailability::Pulled, vec![]),
        ("missing", AsrAvailability::All, vec![]),
    ] {
        let actual: Vec<_> = matching_asr_models(&models, query, availability)
            .iter()
            .map(|model| model.id.as_str())
            .collect();
        assert_eq!(actual, expected, "query: {query}");
    }
}

fn has_text(output: &egui::FullOutput, needle: &str) -> bool {
    output.shapes.iter().any(|shape| {
        matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text().contains(needle))
    })
}

fn click_text(ctx: &egui::Context, app: &mut Frontend, label: &str) -> egui::FullOutput {
    let size = [960.0, 640.0];
    // Panels settle their measured size before controls have final hit regions.
    frame(ctx, app, size, vec![], egui::Modifiers::NONE);
    let output = frame(ctx, app, size, vec![], egui::Modifiers::NONE);
    let pos = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == label => {
                Some(text.pos + text.galley.size() / 2.0)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("Missing control: {label}"));
    frame(
        ctx,
        app,
        size,
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
        egui::Modifiers::NONE,
    );
    frame(
        ctx,
        app,
        size,
        vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
        egui::Modifiers::NONE,
    )
}

#[test]
fn asr_filters_accept_keyboard_input_without_changing_selection_or_dispatching() {
    let (ctx, mut app, commands) = fixture();
    app.poll();
    app.page = Page::Audio;
    app.state.models = asr_models();
    app.preferences.asr_model = "local@revision".into();
    app.search = "unrelated Models page search".into();
    let chat_model = app.model.clone();
    commands.try_iter().for_each(drop);
    frame(
        &ctx,
        &mut app,
        [960.0, 640.0],
        vec![],
        egui::Modifiers::NONE,
    );
    ctx.memory_mut(|memory| memory.request_focus(egui::Id::unique("asr-model-search")));
    let output = frame(
        &ctx,
        &mut app,
        [960.0, 640.0],
        vec![egui::Event::Text("  PARAKEET  ".into())],
        egui::Modifiers::NONE,
    );
    assert_eq!(app.asr_search, "  PARAKEET  ");
    assert!(has_text(&output, "1 of 4 transcription models"));
    assert!(has_text(&output, "local@revision"));

    click_text(&ctx, &mut app, "All");
    click_text(&ctx, &mut app, "Pulled");
    assert!(app.asr_availability == AsrAvailability::Pulled);
    for size in [[960.0, 640.0], [1320.0, 860.0]] {
        for (active, error) in [
            (None, None),
            (Some("Synthetic chat stream".into()), None),
            (None, Some("Synthetic transcription cancelled".into())),
        ] {
            app.state.active = active;
            app.state.error = error;
            let output = frame(&ctx, &mut app, size, vec![], egui::Modifiers::NONE);
            assert!(has_text(&output, "No matching transcription models."));
            assert!(has_text(&output, "Clear filters"));
            assert_eq!(app.preferences.asr_model, "local@revision");
        }
    }
    let output = click_text(&ctx, &mut app, "Clear filters");
    assert!(app.asr_search.is_empty());
    assert!(app.asr_availability == AsrAvailability::All);
    assert!(has_text(&output, "4 of 4 transcription models"));
    assert_eq!(app.preferences.asr_model, "local@revision");
    app.preferences.asr_model = "missing@revision".into();
    app.state.models.clear();
    let output = frame(
        &ctx,
        &mut app,
        [960.0, 640.0],
        vec![],
        egui::Modifiers::NONE,
    );
    assert!(has_text(&output, "No transcription models found."));
    assert!(has_text(&output, "missing@revision"));
    assert_eq!(app.preferences.asr_model, "missing@revision");
    assert_eq!(app.model, chat_model);
    assert_eq!(app.search, "unrelated Models page search");
    assert!(commands.try_iter().next().is_none());
}

fn voice_transcript(id: &str, session: &str) -> TranscriptView {
    TranscriptView {
        id: id.into(),
        text: "spoken words".into(),
        status: "completed".into(),
        dictation_session: Some(session.into()),
        ..Default::default()
    }
}

#[test]
fn chat_voice_controls_append_to_current_edits_once_without_sending() {
    let (ctx, mut app, commands) = fixture();
    app.poll();
    app.preferences.asr_model = "fixture-asr".into();
    app.state.asr = RuntimeView {
        state: "Ready".into(),
        model: "fixture-asr".into(),
        ..Default::default()
    };
    app.draft = "existing draft".into();
    commands.try_iter().for_each(drop);
    click_text(&ctx, &mut app, "Record voice");
    let (id, session) = commands
        .try_iter()
        .find_map(|c| match c {
            Command::RecordForChat { id, session } => Some((id, session)),
            _ => None,
        })
        .expect("Record voice queues capture with a fixed target");
    app.state.recording = true;
    app.state.active = Some("Recording microphone".into());
    let output = frame(
        &ctx,
        &mut app,
        [960.0, 640.0],
        vec![],
        egui::Modifiers::NONE,
    );
    assert!(has_text(&output, "Recording microphone"));
    ctx.memory_mut(|m| m.request_focus(app.composer_id.unwrap()));
    frame(
        &ctx,
        &mut app,
        [960.0, 640.0],
        vec![
            egui::Event::Text(" edited while recording".into()),
            enter(egui::Modifiers::NONE),
        ],
        egui::Modifiers::NONE,
    );
    let edited = app.draft.clone();
    assert!(edited.contains("edited while recording"));
    click_text(&ctx, &mut app, "Stop recording");
    let queued: Vec<_> = commands.try_iter().collect();
    assert!(queued.iter().any(|c| matches!(c, Command::StopRecording)));
    assert!(!queued.iter().any(|c| matches!(c, Command::Send { .. })));
    app.state.recording = false;
    app.state.active = Some("Transcription".into());
    let output = frame(
        &ctx,
        &mut app,
        [1320.0, 860.0],
        vec![],
        egui::Modifiers::NONE,
    );
    assert!(has_text(&output, "Transcribing voice…"));
    let mut result = app.state.clone();
    result.active = None;
    result.transcripts.push(voice_transcript(&id, &session));
    let (updates, rx) = mpsc::sync_channel(8);
    app.rx = rx;
    updates.send(result.clone()).unwrap();
    updates.send(result).unwrap();
    frame(
        &ctx,
        &mut app,
        [1320.0, 860.0],
        vec![],
        egui::Modifiers::NONE,
    );
    assert_eq!(app.draft, format!("{edited}\nspoken words"));
    assert!(app.pending_dictation.is_none());
    let inserted: Vec<_> = commands.try_iter().collect();
    assert_eq!(
        inserted
            .iter()
            .filter(|c| matches!(c, Command::InsertDictation { .. }))
            .count(),
        1
    );
    assert!(inserted.iter().any(|c| matches!(c, Command::InsertDictation { transcript, draft: Some(base) } if transcript == &id && base == &edited)));
    assert!(!inserted.iter().any(|c| matches!(c, Command::Send { .. })));
}

#[test]
fn chat_voice_setup_cancel_and_background_completion_never_redirect_text() {
    let (ctx, mut app, commands) = fixture();
    app.poll();
    commands.try_iter().for_each(drop);
    click_text(&ctx, &mut app, "Record voice");
    assert!(commands.try_iter().next().is_none());
    click_text(&ctx, &mut app, "Voice settings");
    assert!(app.page == Page::Audio);
    app.page = Page::Chat;
    app.pending_dictation = Some(PendingDictation {
        id: "voice".into(),
        session: "origin".into(),
    });
    app.draft = "other chat draft".into();
    app.state
        .transcripts
        .push(voice_transcript("voice", "origin"));
    app.receive_dictation();
    assert_eq!(app.draft, "other chat draft");
    assert!(matches!(
        commands.try_recv().unwrap(),
        Command::InsertDictation { draft: None, .. }
    ));
    app.pending_dictation = Some(PendingDictation {
        id: "cancelled".into(),
        session: app.selected.clone(),
    });
    click_text(&ctx, &mut app, "Cancel dictation");
    assert!(app.pending_dictation.is_none());
    app.state
        .transcripts
        .push(voice_transcript("cancelled", &app.selected));
    app.receive_dictation();
    assert_eq!(app.draft, "other chat draft");
    assert!(
        commands
            .try_iter()
            .all(|c| !matches!(c, Command::InsertDictation { .. } | Command::Send { .. }))
    );
    app.pending_dictation = Some(PendingDictation {
        id: "permission".into(),
        session: app.selected.clone(),
    });
    app.state.dictation_error = Some(("unrelated".into(), "stale error".into()));
    app.receive_dictation();
    assert!(app.pending_dictation.is_some());
    app.state.dictation_error = Some(("permission".into(), "Microphone permission denied".into()));
    app.receive_dictation();
    assert!(app.pending_dictation.is_none());
    assert_eq!(
        app.local_error.as_deref(),
        Some("Microphone permission denied")
    );
    assert_eq!(app.draft, "other chat draft");
}

#[test]
fn dictation_queue_backpressure_preserves_text_and_retries_without_duplicates() {
    let (ctx, mut app, _) = fixture();
    app.poll();
    let (tx, commands) = mpsc::sync_channel(1);
    tx.send(Command::RefreshModels).unwrap();
    app.tx = tx;
    app.draft = "current draft".into();
    app.pending_dictation = Some(PendingDictation {
        id: "voice".into(),
        session: app.selected.clone(),
    });
    app.state
        .transcripts
        .push(voice_transcript("voice", &app.selected));
    app.receive_dictation();
    assert_eq!(app.draft, "current draft");
    assert!(app.pending_dictation.is_some());
    commands.try_recv().unwrap();
    app.receive_dictation();
    assert_eq!(app.draft, "current draft\nspoken words");
    assert!(app.pending_dictation.is_none());
    assert!(matches!(
        commands.try_recv().unwrap(),
        Command::InsertDictation { .. }
    ));
    app.receive_dictation();
    assert!(commands.try_recv().is_err());
    frame(
        &ctx,
        &mut app,
        [960.0, 640.0],
        vec![],
        egui::Modifiers::NONE,
    );
}
