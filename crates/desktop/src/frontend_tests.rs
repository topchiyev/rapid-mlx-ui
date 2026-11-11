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
