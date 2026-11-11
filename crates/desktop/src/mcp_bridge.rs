//! Pure projections between desktop forms and the independent MCP backend.
use std::collections::BTreeMap;

use anyhow::{Result, bail};
use rapid_mlx_core::mcp::{McpConnectionInfo, McpConnectionState, McpRegistration, McpTransport};
use serde_json::Value;

use crate::view::{McpForm, McpView};

/// Parse references only; credentials are resolved by the manager at connect time.
/// Empty JSON editors mean their safe empty defaults. Errors never quote input.
pub fn registration(form: &McpForm) -> Result<McpRegistration> {
    let transport = match form.transport.trim() {
        "stdio" => {
            let args: Vec<String> = serde_json::from_str(if form.arguments.trim().is_empty() {
                "[]"
            } else {
                &form.arguments
            })
            .map_err(|_| anyhow::anyhow!("Arguments must be a JSON array of strings."))?;
            let env: BTreeMap<String, String> =
                serde_json::from_str(if form.environment.trim().is_empty() {
                    "{}"
                } else {
                    &form.environment
                })
                .map_err(|_| {
                    anyhow::anyhow!(
                        "Environment must be a JSON object mapping child variables to source environment variable names, never literal secret values."
                    )
                })?;
            McpTransport::Stdio {
                command: form.target.trim().into(),
                args,
                env,
            }
        }
        "http" | "streamable_http" => McpTransport::StreamableHttp {
            url: form.target.trim().into(),
            bearer_token_env: match form.credential_env.trim() {
                "" => None,
                reference => Some(reference.into()),
            },
        },
        _ => bail!("Choose local stdio or Streamable HTTP transport."),
    };
    let mut registration = McpRegistration::new(form.name.trim(), transport);
    if !form.id.is_empty() {
        registration.id.clone_from(&form.id);
    }
    registration.enabled = form.enabled;
    registration.validate()?;
    Ok(registration)
}

pub fn form(registration: &McpRegistration) -> McpForm {
    let mut form = McpForm {
        id: registration.id.clone(),
        name: registration.name.clone(),
        enabled: registration.enabled,
        arguments: "[]".into(),
        environment: "{}".into(),
        ..McpForm::default()
    };
    match &registration.transport {
        McpTransport::Stdio { command, args, env } => {
            form.transport = "stdio".into();
            form.target.clone_from(command);
            // String-only containers cannot fail serde serialization.
            form.arguments = serde_json::to_string(args).expect("serialize string arguments");
            form.environment =
                serde_json::to_string(env).expect("serialize environment references");
        }
        McpTransport::StreamableHttp {
            url,
            bearer_token_env,
        } => {
            form.transport = "http".into();
            form.target.clone_from(url);
            form.credential_env = bearer_token_env.clone().unwrap_or_default();
        }
    }
    form
}

/// Selection flags are false here; session selection belongs to the caller.
/// Ignore snapshots for a different stable registration ID.
pub fn view(registration: &McpRegistration, connection: Option<&McpConnectionInfo>) -> McpView {
    let connection = connection.filter(|connection| connection.id == registration.id);
    let mut view = McpView {
        registration: form(registration),
        state: if registration.enabled {
            "Disconnected — connect explicitly".into()
        } else {
            "Disabled".into()
        },
        ..McpView::default()
    };
    let Some(connection) = connection else {
        return view;
    };
    if !registration.enabled {
        return view;
    }
    let state = match connection.state {
        McpConnectionState::Disconnected => "Disconnected",
        McpConnectionState::Connecting => "Connecting",
        McpConnectionState::Connected => "Connected",
        McpConnectionState::Failed => "Failed",
    };
    view.state = state.into();
    if let Some(version) = &connection.protocol_version {
        view.state.push_str(&format!(" · MCP {version}"));
    }
    if let Some(error) = &connection.error {
        view.state.push_str(&format!(" — {}", error.message));
    }
    if connection.discovery_changed {
        view.state
            .push_str(" — discovery changed; refresh required");
    }
    if connection.state != McpConnectionState::Connected
        || connection.error.is_some()
        || connection.discovery_changed
    {
        return view;
    }
    view.tools = connection
        .tools
        .iter()
        .map(|tool| {
            let description = match tool.description.as_deref().filter(|text| !text.is_empty()) {
                Some(description) => format!("{} — {description}", tool.name),
                None => tool.name.clone(),
            };
            (tool.id.clone(), description, false)
        })
        .collect();
    view.resources = connection
        .resources
        .iter()
        .filter_map(|resource| {
            let uri = resource.get("uri")?.as_str()?;
            Some((uri.into(), label(resource, uri)))
        })
        .collect();
    view.prompts = connection
        .prompts
        .iter()
        .filter_map(|prompt| {
            let name = prompt.get("name")?.as_str()?;
            Some((name.into(), label(prompt, name)))
        })
        .collect();
    view
}

fn label(value: &Value, fallback: &str) -> String {
    value
        .get("title")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .or_else(|| value.get("name").and_then(Value::as_str))
        .unwrap_or(fallback)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rapid_mlx_core::mcp::McpTool;
    use serde_json::json;

    #[test]
    fn stdio_defaults_and_reference_roundtrip() {
        let draft = McpForm {
            name: "Fixture".into(),
            transport: "stdio".into(),
            target: "fixture-server".into(),
            enabled: true,
            ..McpForm::default()
        };
        let mut saved = registration(&draft).unwrap();
        let projected = form(&saved);
        assert_eq!(projected.arguments, "[]");
        assert_eq!(projected.environment, "{}");
        let mut edit = projected;
        edit.arguments = r#"["--read-only","path with spaces"]"#.into();
        edit.environment = r#"{"TOKEN":"MCP_SOURCE_TOKEN"}"#.into();
        saved = registration(&edit).unwrap();
        assert_eq!(saved.id, edit.id);
        assert_eq!(registration(&form(&saved)).unwrap(), saved);
        assert!(
            serde_json::to_string(&saved)
                .unwrap()
                .contains("MCP_SOURCE_TOKEN")
        );
    }

    #[test]
    fn invalid_input_errors_never_echo_private_values() {
        let mut draft = McpForm {
            name: "Fixture".into(),
            transport: "stdio".into(),
            target: "fixture-server".into(),
            arguments: r#"["private-value",5]"#.into(),
            ..McpForm::default()
        };
        assert!(
            !registration(&draft)
                .unwrap_err()
                .to_string()
                .contains("private-value")
        );
        draft.arguments = "[]".into();
        draft.environment = r#"{"TOKEN":"private-value with spaces"}"#.into();
        assert!(registration(&draft).is_err());
        draft.environment = r#"{"TOKEN":5}"#.into();
        assert!(registration(&draft).is_err());
    }

    #[test]
    fn http_references_and_endpoint_policy_roundtrip() {
        let mut draft = McpForm {
            name: "Remote fixture".into(),
            transport: "http".into(),
            target: "https://example.test/mcp".into(),
            credential_env: "MCP_TOKEN_REF".into(),
            enabled: true,
            ..McpForm::default()
        };
        let saved = registration(&draft).unwrap();
        assert_eq!(registration(&form(&saved)).unwrap(), saved);
        draft.target = "http://example.test/mcp".into();
        assert!(registration(&draft).is_err());
        draft.target = "https://example.test/mcp?token=private-value".into();
        assert!(
            !registration(&draft)
                .unwrap_err()
                .to_string()
                .contains("private-value")
        );
    }

    #[test]
    fn connected_view_uses_actual_tool_ids_and_attributed_context_labels() {
        let saved = McpRegistration::new(
            "Fixture",
            McpTransport::StreamableHttp {
                url: "https://example.test/mcp".into(),
                bearer_token_env: None,
            },
        );
        let mut connection = McpConnectionInfo {
            id: saved.id.clone(),
            name: saved.name.clone(),
            state: McpConnectionState::Connected,
            protocol_version: Some("2025-11-25".into()),
            server_info: None,
            tools: vec![McpTool {
                id: "mcp_namespaced".into(),
                server_id: saved.id.clone(),
                server_name: saved.name.clone(),
                name: "original_echo".into(),
                description: Some("Echo text".into()),
                input_schema: json!({"type":"object"}),
            }],
            resources: vec![
                json!({"uri":"fixture://readme","name":"Readme","title":"Fixture context"}),
                json!({"name":"invalid missing URI"}),
            ],
            prompts: vec![json!({"name":"summarize","title":"Summarize context"})],
            discovery_changed: false,
            error: None,
        };
        let projected = view(&saved, Some(&connection));
        assert_eq!(projected.tools[0].0, "mcp_namespaced");
        assert!(projected.tools[0].1.contains("original_echo"));
        assert!(!projected.tools[0].2);
        assert_eq!(
            projected.resources,
            vec![("fixture://readme".into(), "Fixture context".into())]
        );
        assert_eq!(
            projected.prompts,
            vec![("summarize".into(), "Summarize context".into())]
        );
        connection.discovery_changed = true;
        assert!(view(&saved, Some(&connection)).tools.is_empty());
        connection.discovery_changed = false;
        connection.state = McpConnectionState::Disconnected;
        assert!(view(&saved, Some(&connection)).tools.is_empty());
        connection.id = uuid::Uuid::new_v4().to_string();
        assert!(
            view(&saved, Some(&connection))
                .state
                .starts_with("Disconnected")
        );
        let mut disabled = saved;
        disabled.enabled = false;
        assert_eq!(view(&disabled, Some(&connection)).state, "Disabled");
    }
}
