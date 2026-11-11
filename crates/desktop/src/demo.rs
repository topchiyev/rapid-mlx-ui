//! Synthetic data for UI validation. Never starts a runtime or touches disk.
use crate::view::*;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

pub fn snapshot() -> Snapshot {
    Snapshot {
        ready: true,
        can_send: true,
        state_root: "~/.rapid-mlx-ui (demo: no files written)".into(),
        status: "Synthetic UI fixtures — no model files, processes, or recordings are used.".into(),
        sessions: vec![SessionView {
            id: "demo-session".into(), title: "Rust workspace notes".into(), model: "fixture/qwen@local".into(),
            generation: Generation::initial(), usage: "Prompt 42 · Output 81 · Total 123 (synthetic)".into(),
            messages: vec![
                MessageView { id: "demo-user".into(), role: "user".into(), content: "How should we organize a Rust desktop application?".into(), status: "completed".into(), ..Default::default() },
                MessageView { id: "demo-assistant".into(), role: "assistant".into(), model: "fixture/qwen@local".into(), status: "completed".into(), reasoning: "This is synthetic reasoning content used to validate its separate presentation.".into(), content: "Use a **small workspace** with clear responsibilities:\n\n- A core library for runtime integration and durable state.\n- A desktop executable for interaction and presentation.\n\n```rust\nfn main() {\n    println!(\"Local intelligence, on your Mac.\");\n}\n```\n\nKeep network and filesystem work off the UI thread.".into(), usage: "81 generated tokens · 18.4 tokens/s end-to-end (synthetic)".into(), ..Default::default() },
            ], ..Default::default()
        }],
        models: vec![ModelView { id: "fixture/qwen@local".into(), name: "Qwen · UI fixture".into(), repo: "fixture/qwen".into(), kind: "text".into(), pulled: true, compatible: true, size: "4.8 GiB (synthetic)".into(), detail: "Synthetic model for desktop validation".into(), ..Default::default() }],
        chat: RuntimeView { state: "Ready · synthetic".into(), model: "fixture/qwen@local".into(), ..Default::default() },
        asr: RuntimeView { state: "Stopped".into(), ..Default::default() },
        preferences: Preferences::initial(), cpu: Some(24.0), memory: Some((18 * 1_073_741_824, 64 * 1_073_741_824)),
        cpu_history: (0..100).map(|i| 22.0 + (i as f32 / 5.0).sin() * 8.0).collect(),
        effort_levels: vec!["low".into(), "medium".into(), "high".into()], ..Default::default()
    }
}

pub fn spawn() -> (SyncSender<Command>, Receiver<Snapshot>) {
    let (tx, commands) = sync_channel(128);
    let (updates, rx) = sync_channel(16);
    std::thread::spawn(move || {
        let mut state = snapshot();
        let _ = updates.send(state.clone());
        while let Ok(command) = commands.recv() {
            match command {
                Command::Shutdown => break,
                Command::NewSession => {
                    state.sessions.insert(
                        0,
                        SessionView {
                            id: uuid::Uuid::new_v4().to_string(),
                            title: "New session".into(),
                            generation: Generation::initial(),
                            ..Default::default()
                        },
                    );
                }
                Command::SaveSession {
                    id,
                    draft,
                    model,
                    generation,
                } => {
                    if let Some(session) = state.sessions.iter_mut().find(|s| s.id == id) {
                        session.draft = draft;
                        session.model = model;
                        session.generation = generation;
                    }
                }
                Command::Rename { id, title } => {
                    if let Some(session) = state.sessions.iter_mut().find(|s| s.id == id) {
                        session.title = title;
                    }
                }
                Command::DeleteSession(id) => state.sessions.retain(|s| s.id != id),
                _ => {
                    state.status =
                        "This is a synthetic UI fixture. No real operation was executed.".into()
                }
            }
            if updates.send(state.clone()).is_err() {
                break;
            }
        }
    });
    (tx, rx)
}
