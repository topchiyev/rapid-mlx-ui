//! Durable conversation trees. No runtime calls or side effects occur here.
use anyhow::{Result, bail, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GenerationSettings {
    pub system_prompt: String,
    pub temperature: f64,
    pub top_p: f64,
    pub max_tokens: u32,
    pub effort: Option<String>,
}
impl Default for GenerationSettings {
    fn default() -> Self {
        Self {
            system_prompt: String::new(),
            temperature: 0.7,
            top_p: 1.0,
            max_tokens: 2048,
            effort: None,
        }
    }
}
impl GenerationSettings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.temperature.is_finite() && self.temperature >= 0.0,
            "temperature must be finite and nonnegative"
        );
        ensure!(
            self.top_p.is_finite() && self.top_p > 0.0 && self.top_p <= 1.0,
            "top_p must be in (0, 1]"
        );
        ensure!(self.max_tokens > 0, "max_tokens must be positive");
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}
impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    #[default]
    Pending,
    Streaming,
    Completed,
    Cancelled,
    Failed,
    Interrupted,
}
impl MessageStatus {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Pending | Self::Streaming)
    }
    pub fn is_terminal(self) -> bool {
        !self.is_active()
    }
}
/// Missing values remain unknown; reasoning is a subset, never an extra total.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Usage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub source: UsageSource,
    pub raw: Value,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    #[default]
    RuntimeReported,
    Estimated,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRequest {
    pub id: String,
    pub model: String,
    pub settings: GenerationSettings,
    pub status: MessageStatus,
    #[serde(default)]
    pub request: Value,
    #[serde(default)]
    pub usage: Usage,
    #[serde(default)]
    pub client_elapsed_ms: Option<u64>,
    #[serde(default)]
    pub client_first_output_ms: Option<u64>,
    #[serde(default)]
    pub server_timing: Value,
}
impl ModelRequest {
    pub fn new(model: impl Into<String>, settings: GenerationSettings) -> Self {
        Self {
            id: new_id(),
            model: model.into(),
            settings,
            status: MessageStatus::Pending,
            request: Value::Null,
            usage: Usage::default(),
            client_elapsed_ms: None,
            client_first_output_ms: None,
            server_timing: Value::Null,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Pending,
    Running,
    Completed,
    Denied,
    Failed,
    Cancelled,
    Interrupted,
    OutcomeUnknown,
}
impl ToolStatus {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Pending | Self::Running)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolRecord {
    pub id: String,
    pub provider: String,
    pub name: String,
    pub arguments: Value,
    pub status: ToolStatus,
    #[serde(default)]
    pub result: Value,
    #[serde(default)]
    pub policy: Value,
    #[serde(default)]
    pub model_request_id: Option<String>,
}
impl ToolRecord {
    pub fn new(
        id: impl Into<String>,
        provider: impl Into<String>,
        name: impl Into<String>,
        arguments: Value,
    ) -> Self {
        Self {
            id: id.into(),
            provider: provider.into(),
            name: name.into(),
            arguments,
            status: ToolStatus::Pending,
            result: Value::Null,
            policy: Value::Null,
            model_request_id: None,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub parent_id: Option<String>,
    pub role: Role,
    pub content: String,
    #[serde(default)]
    pub reasoning: String,
    pub status: MessageStatus,
    pub model: String,
    pub settings: GenerationSettings,
    #[serde(default)]
    pub model_revision: Option<String>,
    #[serde(default)]
    pub model_requests: Vec<ModelRequest>,
    #[serde(default)]
    pub tools: Vec<ToolRecord>,
    #[serde(default)]
    pub metadata: Value,
}
impl Message {
    pub fn new(
        role: Role,
        content: impl Into<String>,
        parent_id: Option<String>,
        model: impl Into<String>,
        settings: GenerationSettings,
    ) -> Self {
        Self {
            id: new_id(),
            parent_id,
            role,
            content: content.into(),
            reasoning: String::new(),
            status: if role == Role::Assistant {
                MessageStatus::Pending
            } else {
                MessageStatus::Completed
            },
            model: model.into(),
            settings,
            model_revision: None,
            model_requests: Vec::new(),
            tools: Vec::new(),
            metadata: Value::Null,
        }
    }
    /// Repeated cumulative usage updates replace the request with the same identity.
    pub fn upsert_model_request(&mut self, request: ModelRequest) {
        if let Some(old) = self.model_requests.iter_mut().find(|r| r.id == request.id) {
            *old = request;
        } else {
            self.model_requests.push(request);
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub draft: String,
    pub model: String,
    pub settings: GenerationSettings,
    pub active_head: Option<String>,
    pub messages: Vec<Message>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub model_revision: Option<String>,
    #[serde(default)]
    pub tool_policy: Value,
    #[serde(default)]
    pub selected_mcp_servers: Vec<String>,
    #[serde(default)]
    pub transcript_ids: Vec<String>,
    #[serde(default)]
    pub metadata: Value,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryMessage {
    pub role: Role,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}
impl Session {
    pub fn new(title: impl Into<String>, model: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: new_id(),
            title: title.into(),
            draft: String::new(),
            model: model.into(),
            settings: GenerationSettings::default(),
            active_head: None,
            messages: Vec::new(),
            created_at: now,
            updated_at: now,
            model_revision: None,
            tool_policy: Value::Null,
            selected_mcp_servers: Vec::new(),
            transcript_ids: Vec::new(),
            metadata: Value::Null,
        }
    }
    pub fn message(&self, id: &str) -> Result<&Message> {
        self.messages
            .iter()
            .find(|m| m.id == id)
            .ok_or_else(|| anyhow::anyhow!("message not found"))
    }
    pub fn message_mut(&mut self, id: &str) -> Result<&mut Message> {
        self.messages
            .iter_mut()
            .find(|m| m.id == id)
            .ok_or_else(|| anyhow::anyhow!("message not found"))
    }
    pub fn has_active_generation(&self) -> bool {
        self.messages
            .iter()
            .any(|m| m.role == Role::Assistant && m.status.is_active())
    }
    pub fn selected_branch(&self) -> Result<Vec<&Message>> {
        let mut path = Vec::new();
        let mut seen = HashSet::new();
        let mut next = self.active_head.as_deref();
        while let Some(id) = next {
            ensure!(seen.insert(id), "conversation contains a cycle");
            let m = self.message(id)?;
            path.push(m);
            next = m.parent_id.as_deref();
        }
        path.reverse();
        Ok(path)
    }
    pub fn select_branch(&mut self, head: Option<String>) -> Result<()> {
        if let Some(id) = &head {
            self.message(id)?;
        }
        self.active_head = head;
        Ok(())
    }
    /// Fails for unresolved turns. Reasoning and failed/partial output never enter context.
    pub fn eligible_history(&self) -> Result<Vec<HistoryMessage>> {
        let path = self.selected_branch()?;
        if let Some(last) = path.last() {
            ensure!(
                last.role == Role::Assistant && last.status == MessageStatus::Completed,
                "unresolved turn: retry, edit, or select an earlier completed branch"
            );
        }
        let mut history = Vec::new();
        for m in path {
            ensure!(
                m.status == MessageStatus::Completed,
                "selected branch contains an unresolved turn"
            );
            // Keep calls from one model request in one envelope, followed by matched results.
            let mut groups: Vec<Vec<&ToolRecord>> = Vec::new();
            for tool in &m.tools {
                ensure!(
                    matches!(
                        tool.status,
                        ToolStatus::Completed | ToolStatus::Denied | ToolStatus::Failed
                    ),
                    "tool outcome is unresolved"
                );
                if let Some(group) = groups.last_mut()
                    && tool.model_request_id.is_some()
                    && group[0].model_request_id == tool.model_request_id
                {
                    group.push(tool);
                } else {
                    groups.push(vec![tool]);
                }
            }
            for group in groups {
                let call = HistoryMessage {
                    role: Role::Assistant,
                    content: String::new(),
                    tool_calls: group.iter().map(|tool| (*tool).clone()).collect(),
                    tool_call_id: None,
                };
                history.push(call);
                for tool in group {
                    history.push(HistoryMessage {
                        role: Role::Tool,
                        content: serde_json::to_string(&tool.result)?,
                        tool_calls: Vec::new(),
                        tool_call_id: Some(tool.id.clone()),
                    });
                }
            }
            history.push(HistoryMessage {
                role: m.role,
                content: m.content.clone(),
                tool_calls: Vec::new(),
                tool_call_id: None,
            });
        }
        Ok(history)
    }
    /// Context for a committed attempt, even while its head is pending. Never includes
    /// the attempt's partial answer, reasoning, or earlier sibling attempts/tools.
    pub fn request_history(&self, assistant_id: &str) -> Result<Vec<HistoryMessage>> {
        let attempt = self.message(assistant_id)?;
        ensure!(
            attempt.role == Role::Assistant,
            "request requires an assistant attempt"
        );
        let prompt = self.message(
            attempt
                .parent_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("attempt has no prompt"))?,
        )?;
        ensure!(
            prompt.role == Role::User && prompt.status == MessageStatus::Completed,
            "attempt requires a completed user prompt"
        );
        let mut preceding = self.clone();
        preceding.active_head = prompt.parent_id.clone();
        let mut history = preceding.eligible_history()?;
        history.push(HistoryMessage {
            role: Role::User,
            content: prompt.content.clone(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
        Ok(history)
    }
    pub fn begin_turn(&mut self, prompt: impl Into<String>) -> Result<String> {
        ensure!(
            !self.has_active_generation(),
            "a generation is already active"
        );
        self.eligible_history()?;
        self.settings.validate()?;
        let prompt = prompt.into();
        ensure!(!prompt.trim().is_empty(), "prompt is empty");
        let mut user = Message::new(
            Role::User,
            prompt,
            self.active_head.clone(),
            &self.model,
            self.settings.clone(),
        );
        user.model_revision = self.model_revision.clone();
        let parent = user.id.clone();
        self.messages.push(user);
        self.draft.clear();
        self.append_attempt(parent)
    }
    fn append_attempt(&mut self, parent: String) -> Result<String> {
        self.settings.validate()?;
        let mut assistant = Message::new(
            Role::Assistant,
            "",
            Some(parent),
            &self.model,
            self.settings.clone(),
        );
        assistant.model_revision = self.model_revision.clone();
        assistant
            .model_requests
            .push(ModelRequest::new(&self.model, self.settings.clone()));
        let id = assistant.id.clone();
        self.messages.push(assistant);
        self.active_head = Some(id.clone());
        self.updated_at = Utc::now();
        Ok(id)
    }
    /// Creates a sibling assistant for an existing prompt, without copying tool calls.
    pub fn retry(&mut self, assistant_id: &str) -> Result<String> {
        ensure!(
            !self.has_active_generation(),
            "a generation is already active"
        );
        self.settings.validate()?;
        let old = self.message(assistant_id)?;
        ensure!(
            old.role == Role::Assistant,
            "retry requires an assistant attempt"
        );
        let parent = old
            .parent_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("attempt has no prompt"))?;
        ensure!(
            self.message(&parent)?.role == Role::User,
            "attempt parent is not a prompt"
        );
        self.append_attempt(parent)
    }
    /// Creates a sibling user prompt and pending answer, preserving all old descendants.
    pub fn edit_prompt(&mut self, prompt_id: &str, content: impl Into<String>) -> Result<String> {
        ensure!(
            !self.has_active_generation(),
            "a generation is already active"
        );
        self.settings.validate()?;
        let old = self.message(prompt_id)?;
        ensure!(old.role == Role::User, "edit requires a user prompt");
        let parent = old.parent_id.clone();
        let content = content.into();
        ensure!(!content.trim().is_empty(), "prompt is empty");
        let mut prompt = Message::new(
            Role::User,
            content,
            parent,
            &self.model,
            self.settings.clone(),
        );
        prompt.model_revision = self.model_revision.clone();
        let id = prompt.id.clone();
        self.messages.push(prompt);
        self.append_attempt(id)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.id.is_empty(), "empty session ID");
        self.settings.validate()?;
        let ids: HashSet<_> = self.messages.iter().map(|m| m.id.as_str()).collect();
        ensure!(ids.len() == self.messages.len(), "duplicate message ID");
        let mut requests = HashSet::new();
        for m in &self.messages {
            ensure!(!m.id.is_empty(), "empty message ID");
            m.settings.validate()?;
            if let Some(parent) = &m.parent_id {
                ensure!(ids.contains(parent.as_str()), "missing parent");
            }
            for r in &m.model_requests {
                ensure!(requests.insert(&r.id), "duplicate model request ID");
                r.settings.validate()?;
            }
            let mut calls = HashSet::new();
            for tool in &m.tools {
                ensure!(
                    !tool.id.is_empty() && calls.insert(&tool.id),
                    "empty or duplicate tool-call ID"
                );
                if let Some(request) = &tool.model_request_id {
                    ensure!(
                        m.model_requests.iter().any(|r| &r.id == request),
                        "tool references a missing model request"
                    );
                }
            }
            let mut visited = HashSet::new();
            let mut node = Some(m);
            while let Some(n) = node {
                ensure!(visited.insert(&n.id), "conversation contains a cycle");
                node = n
                    .parent_id
                    .as_deref()
                    .map(|id| self.message(id))
                    .transpose()?;
            }
        }
        self.selected_branch()?;
        Ok(())
    }
    /// Pure state recovery: no network, execution, or dispatch is performed.
    pub fn recover_interrupted(&mut self) -> bool {
        let mut changed = false;
        for m in &mut self.messages {
            if m.status.is_active() {
                m.status = MessageStatus::Interrupted;
                changed = true;
            }
            for r in &mut m.model_requests {
                if r.status.is_active() {
                    r.status = MessageStatus::Interrupted;
                    changed = true;
                }
            }
            for t in &mut m.tools {
                if t.status.is_active() {
                    t.status = ToolStatus::OutcomeUnknown;
                    changed = true;
                }
            }
        }
        changed
    }
    pub fn usage_totals(&self) -> UsageTotals {
        let mut out = UsageTotals::default();
        let mut seen = HashSet::new();
        for r in self.messages.iter().flat_map(|m| &m.model_requests) {
            if !seen.insert(&r.id) {
                continue;
            }
            out.requests += 1;
            if r.usage.source != UsageSource::RuntimeReported {
                out.unknown_requests += 1;
                out.unknown_prompt_requests += 1;
                out.unknown_completion_requests += 1;
                out.unknown_total_requests += 1;
                continue;
            }
            if let Some(n) = r.usage.prompt_tokens {
                out.prompt_tokens = out.prompt_tokens.saturating_add(n);
            } else {
                out.unknown_prompt_requests += 1;
            }
            if let Some(n) = r.usage.completion_tokens {
                out.completion_tokens = out.completion_tokens.saturating_add(n);
            } else {
                out.unknown_completion_requests += 1;
            }
            if let Some(n) = r.usage.total_tokens {
                out.total_tokens = out.total_tokens.saturating_add(n);
            } else {
                out.unknown_total_requests += 1;
            }
            if r.usage.prompt_tokens.is_none()
                || r.usage.completion_tokens.is_none()
                || r.usage.total_tokens.is_none()
            {
                out.unknown_requests += 1;
            }
        }
        out
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageTotals {
    pub requests: usize,
    pub unknown_requests: usize,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub unknown_prompt_requests: usize,
    pub unknown_completion_requests: usize,
    pub unknown_total_requests: usize,
}
impl UsageTotals {
    pub fn is_partial(&self) -> bool {
        self.unknown_requests > 0
    }
}

/// Provider-specific non-secret metadata. Unfinished executions become uncertain on recovery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub namespace: String,
    pub id: String,
    pub session_id: Option<String>,
    pub value: Value,
    #[serde(default)]
    pub status: Option<RecordStatus>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    OutcomeUnknown,
}
impl Record {
    pub fn new(namespace: impl Into<String>, id: impl Into<String>, value: Value) -> Self {
        Self {
            namespace: namespace.into(),
            id: id.into(),
            session_id: None,
            value,
            status: None,
        }
    }
}

/// Reject overwriting sent history, including removing branches. Active output can checkpoint.
pub(crate) fn validate_update(old: &Session, new: &Session) -> Result<()> {
    let by_id: HashMap<_, _> = new.messages.iter().map(|m| (&m.id, m)).collect();
    for before in &old.messages {
        let Some(after) = by_id.get(&before.id) else {
            bail!("saving cannot remove message history");
        };
        if before.role != Role::Assistant || before.status.is_terminal() {
            ensure!(
                before == *after,
                "sent messages and terminal attempts are immutable"
            );
        } else {
            ensure!(
                before.parent_id == after.parent_id
                    && before.role == after.role
                    && before.model == after.model
                    && before.settings == after.settings
                    && before.model_revision == after.model_revision,
                "attempt identity and settings are immutable"
            );
            ensure!(
                after.content.starts_with(&before.content)
                    && after.reasoning.starts_with(&before.reasoning),
                "checkpoints cannot discard received content"
            );
            ensure!(
                before.status != MessageStatus::Streaming || after.status != MessageStatus::Pending,
                "streaming attempts cannot return to pending"
            );
            for request in &before.model_requests {
                let saved = after
                    .model_requests
                    .iter()
                    .find(|r| r.id == request.id)
                    .ok_or_else(|| anyhow::anyhow!("checkpoint cannot remove a model request"))?;
                ensure!(
                    request.model == saved.model && request.settings == saved.settings,
                    "model request identity/settings are immutable"
                );
                if request.status.is_terminal() {
                    ensure!(request == saved, "completed model requests are immutable");
                }
            }
            for tool in &before.tools {
                let saved = after
                    .tools
                    .iter()
                    .find(|t| t.id == tool.id)
                    .ok_or_else(|| anyhow::anyhow!("checkpoint cannot remove a tool call"))?;
                ensure!(
                    tool.provider == saved.provider
                        && tool.name == saved.name
                        && tool.arguments == saved.arguments
                        && tool.model_request_id == saved.model_request_id,
                    "tool call identity and arguments are immutable"
                );
                if !tool.status.is_active() {
                    ensure!(tool == saved, "terminal tool outcomes are immutable");
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn complete(s: &mut Session, id: &str, content: &str) -> Result<()> {
        let m = s.message_mut(id)?;
        m.content = content.into();
        m.status = MessageStatus::Completed;
        for r in &mut m.model_requests {
            r.status = MessageStatus::Completed;
        }
        Ok(())
    }
    #[test]
    fn edit_retry_preserve_descendants_and_selected_context() -> Result<()> {
        let mut s = Session::new("branch", "model");
        let first = s.begin_turn("original")?;
        complete(&mut s, &first, "answer")?;
        s.message_mut(&first)?.reasoning = "hidden".into();
        let prompt_id = s.message(&first)?.parent_id.clone().unwrap();
        let later = s.begin_turn("later question")?;
        complete(&mut s, &later, "later answer")?;
        let old_len = s.messages.len();
        s.settings.temperature = 0.2;
        let retry = s.retry(&first)?;
        assert_eq!(s.messages.len(), old_len + 1);
        assert_eq!(
            s.message(&retry)?.parent_id.as_deref(),
            Some(prompt_id.as_str())
        );
        assert_eq!(s.message(&retry)?.settings.temperature, 0.2);
        complete(&mut s, &retry, "retry answer")?;
        assert_eq!(
            s.eligible_history()?
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            ["original", "retry answer"]
        );
        let edited = s.edit_prompt(&prompt_id, "edited")?;
        complete(&mut s, &edited, "edited answer")?;
        assert_eq!(
            s.eligible_history()?
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            ["edited", "edited answer"]
        );
        s.select_branch(Some(later))?;
        assert_eq!(
            s.eligible_history()?
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            ["original", "answer", "later question", "later answer"]
        );
        assert!(s.messages.iter().any(|m| m.id == retry));
        Ok(())
    }
    #[test]
    fn unresolved_turn_requires_explicit_recovery() -> Result<()> {
        let mut s = Session::new("s", "m");
        let id = s.begin_turn("q")?;
        assert!(s.begin_turn("another").is_err());
        s.message_mut(&id)?.content = "partial".into();
        s.message_mut(&id)?.status = MessageStatus::Cancelled;
        let before = s.clone();
        assert!(s.begin_turn("another").is_err());
        assert_eq!(s, before);
        assert!(s.eligible_history().is_err());
        s.select_branch(None)?;
        let fresh = s.begin_turn("fresh")?;
        let prompt = s.message(s.message(&fresh)?.parent_id.as_deref().unwrap())?;
        assert_eq!(prompt.parent_id, None);
        assert_eq!(
            s.request_history(&fresh)?
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            ["fresh"]
        );
        Ok(())
    }
    #[test]
    fn usage_counts_all_requests_once_and_unknowns_stay_unknown() -> Result<()> {
        let mut s = Session::new("s", "m");
        let first = s.begin_turn("q")?;
        let a = s.message_mut(&first)?;
        let mut request = a.model_requests[0].clone();
        request.usage = Usage {
            prompt_tokens: Some(10),
            completion_tokens: Some(5),
            total_tokens: Some(15),
            reasoning_tokens: Some(3),
            ..Usage::default()
        };
        a.upsert_model_request(request.clone());
        request.usage.completion_tokens = Some(8);
        request.usage.total_tokens = Some(18);
        a.upsert_model_request(request);
        // Separate continuation step; missing counts do not become reported zeros.
        a.model_requests
            .push(ModelRequest::new("m", GenerationSettings::default()));
        complete(&mut s, &first, "done")?;
        let retry = s.retry(&first)?;
        s.message_mut(&retry)?.status = MessageStatus::Cancelled;
        let totals = s.usage_totals();
        assert_eq!(totals.requests, 3);
        assert_eq!(totals.unknown_requests, 2);
        assert_eq!(totals.total_tokens, 18);
        assert_eq!(totals.completion_tokens, 8);
        assert!(totals.is_partial());
        Ok(())
    }
    #[test]
    fn invalid_cycles_and_duplicate_request_ids_are_rejected() -> Result<()> {
        let mut s = Session::new("s", "m");
        let a = s.begin_turn("q")?;
        let request = s.message(&a)?.model_requests[0].clone();
        s.message_mut(&a)?.model_requests.push(request);
        assert!(s.validate().is_err());
        s.message_mut(&a)?.model_requests.pop();
        s.message_mut(&a)?.parent_id = Some(a.clone());
        assert!(s.validate().is_err());
        Ok(())
    }
    #[test]
    fn pending_retry_context_excludes_partial_output_and_old_tools() -> Result<()> {
        let mut s = Session::new("s", "m");
        let first = s.begin_turn("question")?;
        let a = s.message_mut(&first)?;
        a.content = "failed partial".into();
        a.reasoning = "private".into();
        a.status = MessageStatus::Failed;
        let mut tool = ToolRecord::new("side-effect", "os", "write", Value::Null);
        tool.status = ToolStatus::OutcomeUnknown;
        a.tools.push(tool);
        let retry = s.retry(&first)?;
        let history = s.request_history(&retry)?;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].content, "question");
        assert!(history[0].tool_calls.is_empty());
        assert!(s.message(&retry)?.tools.is_empty());
        Ok(())
    }
    #[test]
    fn completed_tool_exchanges_remain_matched_before_final_answer() -> Result<()> {
        let mut s = Session::new("s", "m");
        let id = s.begin_turn("read both")?;
        let a = s.message_mut(&id)?;
        let request_id = a.model_requests[0].id.clone();
        for call_id in ["call-a", "call-b"] {
            let mut call = ToolRecord::new(call_id, "os", "read", Value::Null);
            call.model_request_id = Some(request_id.clone());
            call.status = ToolStatus::Completed;
            call.result = Value::String(format!("result {call_id}"));
            a.tools.push(call);
        }
        complete(&mut s, &id, "final answer")?;
        let h = s.eligible_history()?;
        assert_eq!(
            h.iter().map(|m| m.role).collect::<Vec<_>>(),
            [
                Role::User,
                Role::Assistant,
                Role::Tool,
                Role::Tool,
                Role::Assistant
            ]
        );
        assert_eq!(h[1].tool_calls.len(), 2);
        assert_eq!(h[2].tool_call_id.as_deref(), Some("call-a"));
        assert_eq!(h[3].tool_call_id.as_deref(), Some("call-b"));
        assert_eq!(h[4].content, "final answer");
        Ok(())
    }
}
