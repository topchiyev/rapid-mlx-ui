//! Synthetic MCP server for local integration checks. Stdout is exclusively MCP.
//! Run through a stdio MCP client; no external service or model is used.
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use anyhow::{Context, Result, bail};
use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    model::*,
    service::{NotificationContext, RequestContext},
};

#[derive(Clone, Default)]
struct Fixture {
    lifecycle_dir: Option<PathBuf>,
    wait_calls: Arc<AtomicUsize>,
}

impl Fixture {
    async fn mark(&self, name: &str, contents: &str) -> Result<()> {
        if let Some(directory) = &self.lifecycle_dir {
            tokio::fs::write(directory.join(name), contents)
                .await
                .context("write synthetic lifecycle marker")?;
        }
        Ok(())
    }
}

impl ServerHandler for Fixture {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
        )
        .with_server_info(Implementation::new("rapid-mlx-mcp-fixture", "1.0.0"))
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools = serde_json::from_str(include_str!("../../../fixtures/mcp/tools.json"))
            .map_err(|_| ErrorData::internal_error("invalid compiled synthetic tools", None))?;
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        match request.name.as_ref() {
            "echo" => {
                let message = request
                    .arguments
                    .as_ref()
                    .and_then(|args| args.get("message"))
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| ErrorData::invalid_params("message must be a string", None))?;
                Ok(CallToolResult::success(vec![ContentBlock::text(message)]).into())
            }
            "wait" => {
                let count = self.wait_calls.fetch_add(1, Ordering::SeqCst) + 1;
                self.mark("wait_started", &count.to_string())
                    .await
                    .map_err(|_| {
                        ErrorData::internal_error("cannot write synthetic start marker", None)
                    })?;
                context.ct.cancelled().await;
                self.mark("wait_cancelled", &count.to_string())
                    .await
                    .map_err(|_| {
                        ErrorData::internal_error(
                            "cannot write synthetic cancellation marker",
                            None,
                        )
                    })?;
                Err(ErrorData::internal_error("synthetic wait cancelled", None))
            }
            _ => Err(ErrorData::invalid_params("unknown synthetic tool", None)),
        }
    }

    async fn on_cancelled(
        &self,
        _: CancelledNotificationParam,
        _: NotificationContext<RoleServer>,
    ) {
        let _ = self.mark("cancellation_received", "received").await;
    }

    async fn list_resources(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(vec![
            serde_json::from_value(serde_json::json!({
                "uri":"fixture://readme", "name":"Synthetic stdio context", "mimeType":"text/plain"
            }))
            .expect("constant synthetic resource"),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        if request.uri != "fixture://readme" {
            return Err(ErrorData::invalid_params(
                "unknown synthetic resource",
                None,
            ));
        }
        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            "Synthetic Rust stdio MCP fixture; no external services.",
            request.uri,
        )])
        .into())
    }

    async fn list_prompts(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(ListPromptsResult::with_all_items(vec![
            serde_json::from_value(
                serde_json::json!({"name":"fixture_greeting", "description":"Synthetic greeting"}),
            )
            .expect("constant synthetic prompt"),
        ]))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        if request.name != "fixture_greeting" {
            return Err(ErrorData::invalid_params("unknown synthetic prompt", None));
        }
        Ok(serde_json::from_value::<GetPromptResult>(serde_json::json!({
            "messages":[{"role":"user", "content":{"type":"text", "text":"Hello from the synthetic stdio fixture."}}]
        })).expect("constant synthetic greeting").into())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let lifecycle_dir = match args.next() {
        None => None,
        Some(flag) if flag == "--lifecycle-dir" => {
            let directory = PathBuf::from(
                args.next()
                    .context("--lifecycle-dir requires an existing directory")?,
            );
            if !directory.is_dir() {
                bail!("lifecycle directory must already exist");
            }
            Some(directory)
        }
        _ => bail!("usage: mcp_fixture [--lifecycle-dir EXISTING_DIRECTORY]"),
    };
    if args.next().is_some() {
        bail!("unexpected fixture argument");
    }
    let fixture = Fixture {
        lifecycle_dir,
        ..Default::default()
    };
    fixture.mark("pid", &std::process::id().to_string()).await?;
    // The tuple uses the SDK's async-rw transport already enabled by server/child-process.
    // No transport-io feature or hand-written protocol framing is needed.
    let service = fixture
        .clone()
        .serve((tokio::io::stdin(), tokio::io::stdout()))
        .await?;
    service.waiting().await?;
    fixture
        .mark("graceful_exit", &std::process::id().to_string())
        .await?;
    Ok(())
}
