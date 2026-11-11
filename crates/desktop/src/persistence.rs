//! Pure durable-domain → UI projections. No I/O, dispatch, policy changes, or recovery.
use crate::view::{Generation, MessageView, SessionView};
use rapid_mlx_core::domain::{
    GenerationSettings, Message, MessageStatus, ModelRequest, Role, Session, ToolRecord,
    ToolStatus, UsageSource,
};
use serde_json::Value;
use std::collections::HashSet;

pub fn settings(generation: &Generation) -> GenerationSettings {
    GenerationSettings {
        system_prompt: generation.system_prompt.clone(),
        temperature: f64::from(generation.temperature),
        top_p: f64::from(generation.top_p),
        max_tokens: generation.max_tokens,
        effort: generation.effort.clone(),
    }
}

/// Project only the selected path, retaining navigation to every stored alternative and
/// descendant. This never mutates persisted statuses or treats partial output as history.
pub fn session_view(session: &Session) -> SessionView {
    let mut view = SessionView {
        id: session.id.clone(),
        title: session.title.clone(),
        draft: session.draft.clone(),
        model: session.model.clone(),
        generation: Generation {
            system_prompt: session.settings.system_prompt.clone(),
            temperature: session.settings.temperature as f32,
            top_p: session.settings.top_p as f32,
            max_tokens: session.settings.max_tokens,
            effort: session.settings.effort.clone(),
        },
        // UI preferences are not execution grants. Never deserialize core tool_policy here.
        tools: session
            .metadata
            .get("tool_settings")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default(),
        selected_tools: session
            .metadata
            .get("mcp_tools")
            .cloned()
            .and_then(|v| serde_json::from_value::<Vec<String>>(v).ok())
            .unwrap_or_default(),
        usage: session_usage(session),
        unresolved: session.eligible_history().is_err(),
        ..SessionView::default()
    };
    match session.selected_branch() {
        Ok(path) => {
            for message in path {
                // Tools precede the final answer. Both retain the originating model identity.
                view.messages
                    .extend(message.tools.iter().map(|t| tool_view(message, t)));
                view.messages.push(message_view(message));
            }
        }
        Err(error) => {
            view.unresolved = true;
            view.messages.push(MessageView {
                id: session.active_head.clone().unwrap_or_default(),
                role: "system".into(),
                content: "The selected conversation branch is unavailable. Select a stored branch to inspect it.".into(),
                status: "error".into(),
                error: Some(error.to_string()),
                ..MessageView::default()
            });
        }
    }
    // All nodes, not only sibling roots or leaf answers, remain selectable. Putting these
    // controls on one real card avoids repeating the complete tree on every message.
    let mut seen = HashSet::new();
    let alternatives: Vec<_> = session
        .messages
        .iter()
        .filter(|m| Some(m.id.as_str()) != session.active_head.as_deref() && seen.insert(&m.id))
        .map(|m| (m.id.clone(), alternative_label(m)))
        .collect();
    if !alternatives.is_empty() {
        if view.messages.is_empty() {
            view.messages.push(MessageView {
                id: session.messages[0].id.clone(),
                role: "system".into(),
                content: "No branch selected. Stored conversation branches remain available below."
                    .into(),
                status: "branch selection".into(),
                ..MessageView::default()
            });
        }
        // Prefer a real node: tool cards have synthetic IDs resolved by branch_head().
        let anchor = view
            .messages
            .iter()
            .position(|m| m.role != "tool")
            .unwrap_or(0);
        view.messages[anchor].alternatives = alternatives;
    }
    view
}

/// Resolve the DTO's Branch action. Ordinary cards map to themselves; synthetic tool
/// cards map to their owning message. The worker should use this before select_branch.
pub fn branch_head<'a>(session: &'a Session, view_id: &str) -> Option<&'a str> {
    if let Some(message) = session.messages.iter().find(|m| m.id == view_id) {
        return Some(&message.id);
    }
    session
        .messages
        .iter()
        .find(|m| m.tools.iter().any(|t| tool_view_id(m, t) == view_id))
        .map(|m| m.id.as_str())
}

fn message_view(message: &Message) -> MessageView {
    MessageView {
        id: message.id.clone(),
        role: message.role.as_str().into(),
        content: message.content.clone(),
        reasoning: message.reasoning.clone(),
        status: message_status(message.status).into(),
        model: model_identity(&message.model, message.model_revision.as_deref()),
        usage: attempt_usage(message),
        error: value_error(&message.metadata).or_else(|| match message.status {
            MessageStatus::Failed => Some("Generation failed; retained output is incomplete. Retry or edit explicitly.".into()),
            MessageStatus::Interrupted => Some("Generation was interrupted; the last saved checkpoint is shown. Nothing was replayed.".into()),
            MessageStatus::Cancelled => Some("Generation was cancelled; partial output is preserved. Server termination is not implied.".into()),
            _ => None,
        }),
        alternatives: Vec::new(),
    }
}
fn tool_view_id(message: &Message, tool: &ToolRecord) -> String {
    format!("tool:{}:{}", message.id, tool.id)
}
fn tool_view(message: &Message, tool: &ToolRecord) -> MessageView {
    let result = if tool.result.is_null() {
        "Unavailable (no result recorded)".into()
    } else {
        tool.result.to_string()
    };
    let mut content = format!(
        "{} / {}\nCall ID: {}\n\nArguments:\n{}\n\nResult:\n{}",
        tool.provider, tool.name, tool.id, tool.arguments, result
    );
    if !tool.policy.is_null() {
        content.push_str(&format!("\n\nRecorded policy decision:\n{}", tool.policy));
    }
    let request = tool
        .model_request_id
        .as_ref()
        .and_then(|id| message.model_requests.iter().find(|r| &r.id == id));
    MessageView {
        id: tool_view_id(message, tool), role: "tool".into(), content,
        status: tool_status(tool.status).into(),
        model: request.map_or_else(|| model_identity(&message.model, message.model_revision.as_deref()), |r| r.model.clone()),
        usage: tool.model_request_id.as_ref().map(|id| format!("Model request {id} · tool execution duration unavailable (not model generation time)")).unwrap_or_else(|| "Tool execution duration unavailable".into()),
        error: value_error(&tool.result).or_else(|| match tool.status {
            ToolStatus::OutcomeUnknown => Some("Outcome unknown. Side effects may have occurred; no automatic replay or rollback.".into()),
            ToolStatus::Interrupted => Some("Execution interrupted; no automatic replay or rollback.".into()),
            ToolStatus::Cancelled => Some("Execution cancelled; completed side effects are not undone.".into()),
            ToolStatus::Denied => Some("Execution denied by policy.".into()),
            ToolStatus::Failed => Some("Tool execution failed; inspect the recorded result.".into()),
            _ => None,
        }),
        ..MessageView::default()
    }
}
fn model_identity(model: &str, revision: Option<&str>) -> String {
    revision.map_or_else(|| model.to_owned(), |r| format!("{model} · revision {r}"))
}
fn value_error(metadata: &Value) -> Option<String> {
    let error = metadata.get("error").filter(|e| !e.is_null())?;
    error
        .as_str()
        .map(str::to_owned)
        .or_else(|| {
            error
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| Some(error.to_string()))
}
fn alternative_label(message: &Message) -> String {
    let text: String = message
        .content
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(48)
        .collect();
    let label = if text.is_empty() { "(no text)" } else { &text };
    let id: String = message.id.chars().take(8).collect();
    format!(
        "{}: {} · {} · {}",
        message.role.as_str(),
        label,
        message_status(message.status),
        id
    )
}
fn message_status(status: MessageStatus) -> &'static str {
    match status {
        MessageStatus::Pending => "pending",
        MessageStatus::Streaming => "streaming",
        MessageStatus::Completed => "completed",
        MessageStatus::Cancelled => "cancelled",
        MessageStatus::Failed => "failed",
        MessageStatus::Interrupted => "interrupted",
    }
}
fn tool_status(status: ToolStatus) -> &'static str {
    match status {
        ToolStatus::Pending => "pending",
        ToolStatus::Running => "running",
        ToolStatus::Completed => "completed",
        ToolStatus::Denied => "denied",
        ToolStatus::Failed => "failed",
        ToolStatus::Cancelled => "cancelled",
        ToolStatus::Interrupted => "interrupted",
        ToolStatus::OutcomeUnknown => "outcome-unknown",
    }
}
fn count(value: Option<u64>) -> String {
    value.map_or_else(|| "unknown".into(), |v| v.to_string())
}
fn attempt_usage(message: &Message) -> String {
    if message.role != Role::Assistant && message.model_requests.is_empty() {
        return String::new();
    }
    if message.model_requests.is_empty() {
        return "Model request usage and timing unavailable (no request records)".into();
    }
    let mut seen = HashSet::new();
    message
        .model_requests
        .iter()
        .filter(|r| seen.insert(&r.id))
        .enumerate()
        .map(|(index, r)| request_usage(index + 1, r))
        .collect::<Vec<_>>()
        .join("\n")
}
fn request_usage(index: usize, request: &ModelRequest) -> String {
    let source = match request.usage.source {
        UsageSource::RuntimeReported => "runtime-reported",
        UsageSource::Estimated => "estimated",
    };
    let mut text = format!(
        "Request {index} [{}] · {} · {} · {source} tokens: prompt {}, completion {}, total {}",
        request.id,
        request.model,
        message_status(request.status),
        count(request.usage.prompt_tokens),
        count(request.usage.completion_tokens),
        count(request.usage.total_tokens)
    );
    if let Some(reasoning) = request.usage.reasoning_tokens {
        text.push_str(&format!(" · reasoning {reasoning} (subset of completion)"));
    }
    let elapsed = request
        .client_elapsed_ms
        .map_or_else(|| "unavailable".into(), |v| format!("{v} ms"));
    let first = request
        .client_first_output_ms
        .map_or_else(|| "unavailable".into(), |v| format!("{v} ms"));
    let elapsed_scope = if request.status == MessageStatus::Interrupted {
        "checkpointed client elapsed since dispatch (terminal timing unavailable after interruption)"
    } else if request.status.is_active() {
        "checkpointed client elapsed since dispatch"
    } else {
        "client dispatch-to-terminal elapsed"
    };
    text.push_str(&format!(
        " · {elapsed_scope}: {elapsed} · client time to first output (answer or reasoning): {first}"
    ));
    if request.usage.source == UsageSource::RuntimeReported
        && request.status.is_terminal()
        && request.status != MessageStatus::Interrupted
        && let (Some(tokens), Some(ms)) =
            (request.usage.completion_tokens, request.client_elapsed_ms)
        && ms > 0
    {
        text.push_str(&format!(
            " · {:.2} end-to-end generated tokens/s",
            tokens as f64 * 1000.0 / ms as f64
        ));
    }
    if !request.server_timing.is_null() {
        text.push_str(&format!(
            " · server-reported timing (raw fields): {}",
            request.server_timing
        ));
    }
    text
}
fn session_usage(session: &Session) -> String {
    let totals = session.usage_totals();
    let scope = if totals.is_partial() {
        "partial known subtotal"
    } else {
        "reported total"
    };
    let mut text = format!(
        "All branches/attempts · {} model requests · runtime-reported tokens ({scope}): prompt {}, completion {}, total {} · {} requests with unknown counts (prompt {}, completion {}, total {})",
        totals.requests,
        totals.prompt_tokens,
        totals.completion_tokens,
        totals.total_tokens,
        totals.unknown_requests,
        totals.unknown_prompt_requests,
        totals.unknown_completion_requests,
        totals.unknown_total_requests
    );
    let mut seen = HashSet::new();
    let estimates: Vec<_> = session
        .messages
        .iter()
        .flat_map(|m| &m.model_requests)
        .filter(|r| seen.insert(&r.id) && r.usage.source == UsageSource::Estimated)
        .collect();
    if !estimates.is_empty() {
        let sum = |field: fn(&ModelRequest) -> Option<u64>| -> (u64, usize) {
            estimates
                .iter()
                .fold((0u64, 0usize), |(total, missing), r| match field(r) {
                    Some(n) => (total.saturating_add(n), missing),
                    None => (total, missing + 1),
                })
        };
        let (prompt, p_missing) = sum(|r| r.usage.prompt_tokens);
        let (completion, c_missing) = sum(|r| r.usage.completion_tokens);
        let (total, t_missing) = sum(|r| r.usage.total_tokens);
        text.push_str(&format!(" · separate estimated subtotals ({} requests): prompt {prompt} ({p_missing} unknown), completion {completion} ({c_missing} unknown), total {total} ({t_missing} unknown)", estimates.len()));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use rapid_mlx_core::domain::{Usage, UsageSource};
    use serde_json::json;
    fn complete(session: &mut Session, id: &str, text: &str) {
        let message = session.message_mut(id).unwrap();
        message.content = text.into();
        message.status = MessageStatus::Completed;
        for request in &mut message.model_requests {
            request.status = MessageStatus::Completed;
        }
    }
    #[test]
    fn mapping_preserves_settings_draft_and_ui_metadata_without_policy_grants() {
        let generation = Generation {
            system_prompt: "system".into(),
            temperature: 0.5,
            top_p: 0.75,
            max_tokens: 17,
            effort: Some("high".into()),
        };
        let mut session = Session::new("saved", "missing-model");
        session.settings = settings(&generation);
        session.draft = "draft".into();
        session.metadata = json!({"tool_settings":{"directory":"/selected","enabled":true,"writes":false,"commands":true},"mcp_tools":["mcp_actual_id"]});
        session.tool_policy = json!({"enabled":false,"allowed_roots":["/different"]});
        session.selected_mcp_servers = vec!["server-not-tool".into()];
        let original = session.clone();
        let view = session_view(&session);
        assert_eq!(view.model, "missing-model");
        assert_eq!(view.draft, "draft");
        assert_eq!(view.generation.system_prompt, "system");
        assert_eq!(view.generation.temperature, 0.5);
        assert_eq!(view.generation.top_p, 0.75);
        assert_eq!(view.generation.max_tokens, 17);
        assert_eq!(view.generation.effort.as_deref(), Some("high"));
        assert_eq!(view.tools.directory, "/selected");
        assert!(view.tools.enabled && view.tools.commands && !view.tools.writes);
        assert_eq!(view.selected_tools, ["mcp_actual_id"]);
        assert_eq!(session, original);
        session.metadata = json!({"tool_settings":false,"mcp_tools":[1]});
        let view = session_view(&session);
        assert!(!view.tools.enabled);
        assert!(view.selected_tools.is_empty());
    }
    #[test]
    fn selected_path_keeps_every_alternative_descendant_and_unresolved_output() {
        let mut session = Session::new("tree", "model");
        let first = session.begin_turn("first prompt").unwrap();
        complete(&mut session, &first, "first answer");
        let prompt = session.message(&first).unwrap().parent_id.clone().unwrap();
        let descendant = session.begin_turn("later prompt").unwrap();
        complete(&mut session, &descendant, "later answer");
        let retry = session.retry(&first).unwrap();
        complete(&mut session, &retry, "retry answer");
        let edited = session.edit_prompt(&prompt, "edited prompt").unwrap();
        let message = session.message_mut(&edited).unwrap();
        message.content = "partial".into();
        message.reasoning = "reasoning".into();
        message.status = MessageStatus::Interrupted;
        message.metadata = json!({"error":{"message":"runtime lost"}});
        let original = session.clone();
        let view = session_view(&session);
        assert_eq!(
            view.messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            ["edited prompt", "partial"]
        );
        assert!(view.unresolved);
        assert_eq!(view.messages[1].reasoning, "reasoning");
        assert_eq!(view.messages[1].error.as_deref(), Some("runtime lost"));
        let alternatives: HashSet<_> = view
            .messages
            .iter()
            .flat_map(|m| &m.alternatives)
            .map(|(id, _)| id.as_str())
            .collect();
        assert!(
            alternatives.contains(descendant.as_str())
                && alternatives.contains(retry.as_str())
                && alternatives.contains(first.as_str())
        );
        assert_eq!(alternatives.len(), session.messages.len() - 1);
        assert_eq!(session, original);
        session.active_head = None;
        let empty = session_view(&session);
        assert!(!empty.messages.is_empty());
        assert_eq!(empty.messages[0].alternatives.len(), session.messages.len());
    }
    #[test]
    fn tool_cards_keep_identity_uncertainty_and_resolvable_branch_targets() {
        let mut session = Session::new("tools", "turn-model");
        let assistant = session.begin_turn("write").unwrap();
        let message = session.message_mut(&assistant).unwrap();
        let request = &mut message.model_requests[0];
        request.model = "actual-step-model".into();
        let request_id = request.id.clone();
        let mut tool = ToolRecord::new("call:1", "provider", "write", json!({"path":"/file"}));
        tool.model_request_id = Some(request_id);
        tool.status = ToolStatus::OutcomeUnknown;
        message.tools.push(tool);
        message.status = MessageStatus::Interrupted;
        let view = session_view(&session);
        let card = view.messages.iter().find(|m| m.role == "tool").unwrap();
        assert_eq!(card.status, "outcome-unknown");
        assert_eq!(card.model, "actual-step-model");
        assert!(
            card.content.contains("call:1")
                && card.content.contains("/file")
                && card.content.contains("no result recorded")
        );
        assert!(
            card.error
                .as_ref()
                .unwrap()
                .contains("Side effects may have occurred")
        );
        assert_eq!(branch_head(&session, &card.id), Some(assistant.as_str()));
        assert_eq!(branch_head(&session, &assistant), Some(assistant.as_str()));
        assert_eq!(branch_head(&session, "absent"), None);
        assert!(view.unresolved);
    }
    #[test]
    fn usage_preserves_unknowns_estimates_and_per_request_timing_without_double_counting() {
        let mut session = Session::new("usage", "m");
        let id = session.begin_turn("q").unwrap();
        let message = session.message_mut(&id).unwrap();
        message.model_requests[0].usage = Usage {
            prompt_tokens: Some(10),
            completion_tokens: Some(6),
            total_tokens: Some(16),
            reasoning_tokens: Some(2),
            ..Usage::default()
        };
        message.model_requests[0].client_elapsed_ms = Some(2000);
        message.model_requests[0].client_first_output_ms = Some(125);
        message.model_requests[0].server_timing = json!({"decode_ms":100});
        let mut estimate = ModelRequest::new("estimated-model", GenerationSettings::default());
        estimate.usage.source = UsageSource::Estimated;
        estimate.usage.completion_tokens = Some(100);
        message.model_requests.push(estimate);
        complete(&mut session, &id, "answer");
        let retry = session.retry(&id).unwrap();
        session.message_mut(&retry).unwrap().status = MessageStatus::Cancelled;
        let view = session_view(&session);
        assert!(view.usage.contains("3 model requests"));
        assert!(view.usage.contains("partial known subtotal"));
        assert!(view.usage.contains("completion 6, total 16"));
        assert!(view.usage.contains("separate estimated subtotals"));
        assert!(view.usage.contains("completion 100"));
        session.select_branch(Some(id)).unwrap();
        let view = session_view(&session);
        let usage = &view.messages.last().unwrap().usage;
        assert!(
            usage.contains("2000 ms")
                && usage.contains("125 ms")
                && usage.contains("3.00 end-to-end generated tokens/s")
        );
        assert!(usage.contains("reasoning 2 (subset of completion)"));
        assert!(usage.contains("server-reported timing") && usage.contains("decode_ms"));
        assert!(usage.contains("Request 2") && usage.contains("estimated tokens: prompt unknown"));
    }
    #[test]
    fn invalid_branch_stays_visible_and_does_not_hide_alternatives() {
        let mut session = Session::new("broken", "m");
        let id = session.begin_turn("q").unwrap();
        session.active_head = Some("missing".into());
        let view = session_view(&session);
        assert!(view.unresolved);
        assert!(view.messages[0].error.is_some());
        assert!(
            view.messages[0]
                .alternatives
                .iter()
                .any(|(head, _)| head == &id)
        );
    }

    #[test]
    fn interrupted_or_unknown_timing_never_claims_terminal_throughput() {
        let mut session = Session::new("timing", "m");
        let id = session.begin_turn("q").unwrap();
        let request = &mut session.message_mut(&id).unwrap().model_requests[0];
        request.status = MessageStatus::Interrupted;
        request.usage.completion_tokens = Some(10);
        request.client_elapsed_ms = Some(1000);
        let view = session_view(&session);
        let usage = &view.messages.last().unwrap().usage;
        assert!(usage.contains("terminal timing unavailable after interruption"));
        assert!(!usage.contains("tokens/s"));
        let request = &mut session.message_mut(&id).unwrap().model_requests[0];
        request.status = MessageStatus::Completed;
        request.client_elapsed_ms = Some(0);
        let view = session_view(&session);
        assert!(!view.messages.last().unwrap().usage.contains("tokens/s"));
        let request = &mut session.message_mut(&id).unwrap().model_requests[0];
        request.client_elapsed_ms = None;
        request.usage.completion_tokens = None;
        let view = session_view(&session);
        let usage = &view.messages.last().unwrap().usage;
        assert!(usage.contains("completion unknown"));
        assert!(usage.contains("client dispatch-to-terminal elapsed: unavailable"));
    }
}
