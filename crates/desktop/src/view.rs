use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Generation {
    pub system_prompt: String,
    pub temperature: f32,
    pub top_p: f32,
    pub max_tokens: u32,
    pub effort: Option<String>,
}

impl Generation {
    pub fn initial() -> Self {
        Self {
            temperature: 0.7,
            top_p: 0.95,
            max_tokens: 2048,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct MessageView {
    pub id: String,
    pub role: String,
    pub content: String,
    pub reasoning: String,
    pub status: String,
    pub model: String,
    pub usage: String,
    pub error: Option<String>,
    pub alternatives: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default)]
pub struct SessionView {
    pub id: String,
    pub title: String,
    pub draft: String,
    pub model: String,
    pub generation: Generation,
    pub messages: Vec<MessageView>,
    pub usage: String,
    pub unresolved: bool,
    pub tools: ToolSettings,
    pub selected_tools: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolSettings {
    pub directory: String,
    pub enabled: bool,
    pub writes: bool,
    pub commands: bool,
    pub executables: String,
}

#[derive(Clone, Debug, Default)]
pub struct ModelView {
    pub id: String,
    pub name: String,
    pub repo: String,
    pub revision: String,
    pub path: PathBuf,
    pub kind: String,
    pub size: String,
    pub pulled: bool,
    pub compatible: bool,
    pub detail: String,
    pub catalog: bool,
    pub served: bool,
}

#[derive(Clone, Debug, Default)]
pub struct RuntimeView {
    pub state: String,
    pub model: String,
    pub external: bool,
    pub logs: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub executable: String,
    pub chat_endpoint: String,
    pub asr_endpoint: String,
    pub cache: String,
    pub model_roots: Vec<PathBuf>,
    pub asr_model: String,
    pub retain_audio: bool,
    pub tool_directory: String,
    pub tools_enabled: bool,
    pub allow_writes: bool,
    pub allow_commands: bool,
    pub window_size: Option<[f32; 2]>,
    pub asr_language: String,
}

impl Preferences {
    pub fn initial() -> Self {
        Self {
            executable: "rapid-mlx".into(),
            chat_endpoint: "http://127.0.0.1:8000".into(),
            asr_endpoint: "http://127.0.0.1:8001".into(),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct McpForm {
    pub id: String,
    pub name: String,
    pub transport: String,
    pub target: String,
    pub arguments: String,
    pub credential_env: String,
    pub environment: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, Default)]
pub struct McpView {
    pub registration: McpForm,
    pub state: String,
    pub tools: Vec<(String, String, bool)>,
    pub resources: Vec<(String, String)>,
    pub prompts: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default)]
pub struct TranscriptView {
    pub id: String,
    pub text: String,
    pub model: String,
    pub status: String,
    pub duration: String,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub sessions: Vec<SessionView>,
    pub models: Vec<ModelView>,
    pub catalog_source: String,
    pub chat: RuntimeView,
    pub asr: RuntimeView,
    pub registrations: Vec<McpView>,
    pub transcripts: Vec<TranscriptView>,
    pub preferences: Preferences,
    pub cpu: Option<f32>,
    pub memory: Option<(u64, u64)>,
    pub processes: Vec<(String, Option<f32>, Option<u64>)>,
    pub cpu_history: Vec<f32>,
    pub metrics_stale: bool,
    pub active: Option<String>,
    pub operations: Vec<String>,
    pub status: String,
    pub error: Option<String>,
    pub state_root: String,
    pub ready: bool,
    pub can_send: bool,
    pub sample_time: String,
    pub focused_session: String,
    pub recording: bool,
    pub effort_levels: Vec<String>,
    pub effort_note: String,
    pub deletion_preview: Option<(String, String)>,
}

#[derive(Debug)]
pub enum Command {
    NewSession,
    FocusSession(String),
    WindowSize([f32; 2]),
    Rename {
        id: String,
        title: String,
    },
    DeleteSession(String),
    SaveSession {
        id: String,
        draft: String,
        model: String,
        generation: Generation,
    },
    Send {
        id: String,
        text: String,
    },
    Retry {
        id: String,
        message: String,
    },
    Edit {
        id: String,
        message: String,
        text: String,
    },
    Branch {
        id: String,
        head: String,
    },
    Cancel,
    CancelOperation(String),
    RefreshModels,
    Pull(String),
    PreviewDeletion(String),
    DeleteModel(String),
    Runtime {
        role: String,
        action: String,
        model: String,
    },
    Preferences(Preferences),
    Credential {
        role: String,
        value: String,
    },
    SaveMcp(McpForm),
    Tools {
        session: String,
        policy: ToolSettings,
    },
    Mcp {
        id: String,
        action: String,
    },
    SelectTool {
        session: String,
        server: String,
        tool: String,
        enabled: bool,
    },
    McpContext {
        session: String,
        server: String,
        kind: String,
        name: String,
        arguments: serde_json::Value,
    },
    Transcribe(PathBuf),
    Record,
    StopRecording,
    SaveTranscript {
        id: String,
        text: String,
    },
    LinkTranscript {
        session: String,
        transcript: String,
    },
    Shutdown,
}
