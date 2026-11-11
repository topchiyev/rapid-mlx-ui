# MCP Client

## Purpose

Connect the desktop app to user-registered MCP servers and make selected server tools and context available to local-model conversations.

## ADDED Requirements

### Requirement: Persistent server registration
The app SHALL let users register, name, edit, enable, disable, and remove MCP servers. Non-secret registrations SHALL persist under `~/.rapid-mlx-ui` and describe either a stdio executable/arguments/environment references or a Streamable HTTP endpoint.

#### Scenario: Reopen saved registrations
- **WHEN** the user restarts after registering an MCP server
- **THEN** its connection definition and enabled state are restored without exposing stored secret values
- **AND** unfinished prior tool calls are not replayed

#### Scenario: Remove a registration
- **WHEN** the user removes a server registration
- **THEN** it disconnects, is removed from future session tool selection, and historical calls retain its identity

### Requirement: Transport and connection lifecycle
The app SHALL support local stdio and loopback/remote Streamable HTTP servers using negotiated compatible MCP versions and capabilities. It SHALL show disconnected, connecting, connected, and failed states with actionable protocol, authentication, and transport errors.

#### Scenario: Connect a local server
- **WHEN** the user connects a valid stdio registration
- **THEN** the app launches the configured server, completes compatible discovery, and shows its identity and advertised capabilities

#### Scenario: Connect a remote server
- **WHEN** the user connects an explicitly registered HTTPS Streamable HTTP endpoint
- **THEN** the app connects with the configured authentication and reports success only after compatible discovery completes

#### Scenario: Server requires authentication
- **WHEN** credentials are missing, rejected, or require an unsupported authentication flow
- **THEN** the UI explains the requirement and does not present the server as connected

### Requirement: Discoverable tools and context
The app SHALL list discovered tools with schemas and server identity, follow pagination, and refresh changed discovery when supported. It SHALL list advertised resources/prompts and include them in chat only on explicit selection.

#### Scenario: Server advertises multiple pages of tools
- **WHEN** discovery returns paginated tool results
- **THEN** the UI makes all discovered tools available for selection without silently dropping later pages

#### Scenario: Include a server resource
- **WHEN** the user selects an advertised resource or prompt for the current conversation
- **THEN** its fetched content is visibly attributed to that server and included as context without changing execution permissions

### Requirement: Session-selected MCP execution
Only selected tools from connected enabled MCP servers SHALL be advertised to verified tool-capable models. The app SHALL validate arguments, namespace collisions, apply session execution policy, route results by call identity, and show tool failures without silently substituting a different server.

#### Scenario: Two servers advertise the same tool name
- **WHEN** both tools are enabled for a session
- **THEN** their model-visible identifiers remain distinct and each call reaches its selected server

#### Scenario: Server disconnects during a tool call
- **WHEN** the connection is lost before the call outcome is known
- **THEN** the turn shows the failure or uncertain outcome and does not automatically repeat a possibly side-effecting call

### Requirement: Owned connection cleanup and data handling
The app SHALL stop only stdio processes it launched, disconnect HTTP clients without terminating remote servers, and keep credentials out of persisted registrations and routine logs. It SHALL send only selected conversation context and tool arguments needed by the enabled operation.

#### Scenario: Quit with MCP connections active
- **WHEN** the app shuts down
- **THEN** owned stdio children are stopped and HTTP connections close
- **AND** external servers remain running and tokens are absent from saved state and routine logs
