# TJ-SPEC-021: Test Orchestration

| Metadata | Value |
|----------|-------|
| **ID** | TJ-SPEC-021 |
| **Title** | Test Orchestration |
| **Type** | Infrastructure Specification |
| **Status** | Draft |
| **Priority** | High |
| **Version** | v3.0.0 |
| **Depends On** | TJ-SPEC-007 (CLI Interface), TJ-SPEC-013 (OATF Integration), TJ-SPEC-014 (Verdict & Evaluation Output), TJ-SPEC-015 (Multi-Actor Orchestration), TJ-SPEC-016 (AG-UI Protocol Support), TJ-SPEC-017 (A2A Protocol Support), TJ-SPEC-018 (MCP Client Mode) |
| **Tags** | #e2e #testing #conformance #orchestration #mock-llm #langgraph #crewai |

---

## 1. Context

### 1.1 Motivation

ThoughtJack has unit tests that verify individual components and integration tests that verify protocol handling with mock transports. Neither answers the question that matters: *does ThoughtJack actually work against a real agent?*

Phase engines, verdict pipelines, and protocol drivers are meaningless if the compiled thoughtjack binary cannot connect to a LangGraph agent over AG-UI, drive it through MCP tool discovery, detect correct data flow via indicator evaluation, and produce the correct exit code. That requires a real agent running a real graph with real protocol endpoints.

There is a second problem: agent frameworks are moving targets. New releases of LangGraph and CrewAI can change AG-UI event formats, tool calling conventions, SSE streaming behavior, or MCP client integration. ThoughtJack needs to detect these regressions before users do.

This spec defines end-to-end conformance testing for ThoughtJack using @dwmkerr/mock-llm for deterministic agent behavior across two agent frameworks (LangGraph and CrewAI) and five protocol modes. ThoughtJack remains a protocol tool — it binds servers, connects clients, runs OATF documents, and produces verdicts. A lightweight Python orchestrator (`run_conformance.py`) handles everything else: starting agents, managing mock-llm config, port allocation, readiness detection, and process cleanup.

### 1.2 Scope

This spec covers:

- Conformance testing philosophy: no attack scenarios, protocol plumbing verification only
- @dwmkerr/mock-llm as external dependency for deterministic agent behavior
- Reference agents for LangGraph and CrewAI (OpenAI Agents SDK deferred; see §16.4)
- Framework protocol support matrix with per-capability coverage
- **Phase 1:** MCP server, AG-UI client, and A2A server conformance against framework agents; MCP client and A2A client self-testing via multi-actor OATF documents
- **Phase 2:** MCP client and A2A client conformance against framework server endpoints (LangGraph Agent Server, CrewAI A2A server)
- Use of existing SPEC-007 flags (`--mcp-server`, `--a2a-server`) for deterministic port binding
- Python orchestrator (`run_conformance.py`) for agent lifecycle, port allocation, and readiness
- Conformance fixture format: OATF document + mock-llm config + expected verdict
- CI workflows: PR smoke tests and nightly compatibility
- README coverage chart format

This spec does **not** cover:

- Real LLM integration (deferred to benchmark spec)
- Statistical model evaluation or benchmarking (deferred to benchmark spec)
- Mock-llm lifecycle management by ThoughtJack (deferred to TJ-SPEC-022: OATF Actor Model)
- Phase-aware mock-llm reconfiguration (deferred to TJ-SPEC-022)
- Mock-llm as a first-class OATF actor type (deferred to TJ-SPEC-022)
- LLM proxy actor for traffic inspection and response mutation (deferred to TJ-SPEC-022)

### 1.3 Design Principles

**Conformance, not attacks.** A mock LLM has no judgment, no ability to refuse, no resilience. A rug pull with a mock LLM "succeeds" every time -- you are not testing whether the attack works, you are testing whether the protocol plumbing delivers data correctly. Attack scenarios belong exclusively in the benchmark where a real LLM makes real decisions and resilience is the thing being measured. Conformance testing verifies: does every byte ThoughtJack sends arrive correctly at the agent, and does every byte the agent sends back arrive correctly at ThoughtJack, across every protocol surface?

**ThoughtJack is a protocol tool, not a process manager.** ThoughtJack binds servers on specified ports, connects clients to specified endpoints, runs OATF documents, and produces verdicts. It does not start agents, manage child processes, parse readiness markers, or allocate ports. Process orchestration is inherently messy (platform-specific signals, process groups, health polling, stdout parsing) and belongs in a scripting language where it is trivial, not in a compiled binary where it adds hundreds of lines of code unrelated to protocol simulation.

**One mock LLM, all frameworks.** Every major agent framework supports OpenAI-compatible chat completion APIs via a base_url parameter. A single @dwmkerr/mock-llm instance serves both frameworks identically. Zero per-framework LLM code.

**Orchestrator owns lifecycle, ThoughtJack owns protocol.** The Python orchestrator starts mock-llm, starts reference agents, allocates ports, waits for readiness, configures mock-llm rules, invokes ThoughtJack with explicit endpoint flags, collects results, and tears everything down. ThoughtJack sees only pre-resolved endpoints — it has no idea whether the agent was started by the orchestrator, by a CI script, or by a human in another terminal.

**Single-run atomicity.** Each orchestrator invocation starts a fresh agent, runs one ThoughtJack invocation against one OATF document, and tears everything down. No state leaks between scenarios. Running multiple scenarios means multiple orchestrator invocations.

**Test the binary, not the library.** ThoughtJack runs as the compiled binary with real CLI parsing, transport setup, phase engine, and verdict pipeline. This exercises the same code path users run.

**Test what exists natively.** Each framework is tested only through protocol surfaces it natively supports. No custom adapters or community wrappers -- those would become part of the test surface rather than testing the framework itself. The coverage chart documents what is supported, what is missing, and what is planned, doubling as a signal to framework maintainers about protocol gaps.

---

## 2. Architecture

### 2.1 Component Overview

```
                                    +----------------------+
                                    |  @dwmkerr/mock-llm   |
                                    |  (external process)   |
                                    |                       |
                                    |  /v1/chat/completions |
                                    |  /v1/models           |
                                    |  /health              |
                                    +----------+------------+
                                               |
                                    OpenAI-compatible API
                                               |
+----------------------------------------------+------------------------+
|                                              |                        |
|  +-------------+  +-------------+                                    |
|  |  LangGraph  |  |   CrewAI    |                                    |
|  |  Reference  |  |  Reference  |                                    |
|  |  Agent      |  |  Agent      |                                    |
|  |             |  |             |                                    |
|  |  AG-UI *    |  |  AG-UI *    |                                    |
|  |  MCP   *    |  |  MCP   *    |                                    |
|  |  A2A   o    |  |  A2A   *    |                                    |
|  +------+------+  +------+------+                                    |
|         |                |                                            |
|         +----------------+                                            |
|                          | protocol connections                       |
|         +----------------+                                            |
|         |                |                                            |
|  +------v-----------------------------------------v------+            |
|  |                 ThoughtJack Binary                     |            |
|  |                                                       |            |
|  |  MCP server  |  AG-UI client  |  A2A server          |            |
|  |  MCP client  |                |  A2A client          |            |
|  |              |  Phase Engine  |  Verdict Pipeline     |            |
|  +-------------------------------------------------------+            |
|                                                                       |
|  Orchestrated by: run_conformance.py                                  |
|  (port allocation, agent lifecycle, mock-llm config, readiness)       |
|                                                                       |
|  MCP client and A2A client conformance tested via                     |
|  multi-actor OATF documents (ThoughtJack against itself)              |
|                                                                       |
|     * = native SDK support                                            |
|     o = not yet supported                                             |
+-----------------------------------------------------------------------+
```

### 2.2 Directory Layout

```
tests/e2e/
|-- run_conformance.py              # Python orchestrator (~120 lines)
|-- reference-agents/
|   |-- langgraph/
|   |   |-- pyproject.toml          # langgraph, langchain-mcp-adapters, ag-ui-langgraph
|   |   |-- agent.py                # ~50 lines: StateGraph + ReAct pattern
|   |   +-- server.py               # uvicorn AG-UI endpoint (Phase 1)
|   |-- langgraph-server/
|   |   |-- langgraph.json          # Agent Server config (Phase 2)
|   |   |-- pyproject.toml          # langgraph-api >= 0.4.21
|   |   +-- agent.py                # Same graph, deployed via Agent Server
|   |-- crewai/
|   |   |-- pyproject.toml          # crewai, crewai-tools[mcp], ag-ui-crewai
|   |   |-- agent.py                # ~50 lines: Crew + Agent + Task
|   |   +-- server.py               # uvicorn AG-UI endpoint + A2A server (Phase 2)
|-- fixtures/
|   |-- mcp-tool-discovery/         # Phase 1: TJ MCP server ← agent MCP client
|   |   |-- attack.yaml
|   |   |-- mock-llm.yaml
|   |   +-- expected.yaml
|   |-- agui-event-streaming/       # Phase 1: TJ AG-UI client → agent AG-UI server
|   |   |-- attack.yaml
|   |   |-- mock-llm.yaml
|   |   +-- expected.yaml
|   |-- a2a-task-delegation/        # Phase 1: TJ A2A server ← agent A2A client
|   |   |-- attack.yaml
|   |   |-- mock-llm.yaml
|   |   |-- frameworks.yaml         # frameworks: [crewai]
|   |   +-- expected.yaml
|   |-- mcp-client-basic/           # Phase 1: self-test (TJ client → TJ server)
|   |   |-- attack.yaml
|   |   +-- expected.yaml
|   |-- a2a-client-basic/           # Phase 1: self-test (TJ client → TJ server)
|   |   |-- attack.yaml
|   |   +-- expected.yaml
|   |-- mcp-client-langgraph/       # Phase 2: TJ MCP client → Agent Server /mcp
|   |   |-- attack.yaml
|   |   |-- mock-llm.yaml
|   |   |-- frameworks.yaml         # frameworks: [langgraph-server]
|   |   +-- expected.yaml
|   |-- a2a-client-langgraph/       # Phase 2: TJ A2A client → Agent Server /a2a/{id}
|   |   |-- attack.yaml
|   |   |-- mock-llm.yaml
|   |   |-- frameworks.yaml         # frameworks: [langgraph-server]
|   |   +-- expected.yaml
|   +-- a2a-client-crewai/          # Phase 2: TJ A2A client → CrewAI A2A server
|       |-- attack.yaml
|       |-- mock-llm.yaml
|       |-- frameworks.yaml         # frameworks: [crewai]
|       +-- expected.yaml
+-- results/                        # CI output directory
    +-- .gitkeep
```

---

## 3. External Dependencies

### 3.1 @dwmkerr/mock-llm

ThoughtJack uses @dwmkerr/mock-llm as its mock LLM server. Mock-llm is a mature, actively maintained OpenAI-compatible mock server (MIT license, 209 commits, 23 releases) used in production by McKinsey's Ark project.

**Why mock-llm, not a custom echo LLM:**

Mock-llm provides default echo mode (echoes last user message), YAML rule configuration with JMESPath matching for conditional responses, sequential responses via sequence: 0, 1, 2... for multi-turn tool calling flows, streaming SSE support, built-in MCP and A2A protocol mocking, runtime config updates via /config endpoint, and health/readiness probes at /health and /ready.

The sequential response feature is critical for conformance testing. A tool calling flow requires at minimum two LLM calls: the first returns tool_calls to trigger MCP tool invocation, the second returns text content to complete the conversation. Mock-llm's sequence field handles this deterministically.

**Installation and operation:**

```bash
# npm
npm install -g @dwmkerr/mock-llm
mock-llm

# Docker
docker run -p 6556:6556 ghcr.io/dwmkerr/mock-llm

# With custom config
mock-llm --config path/to/mock-llm.yaml
```

Mock-llm runs on port 6556 by default. Both reference agents connect to it via base_url configuration pointing at http://localhost:6556/v1.

**Runtime configuration updates:**

The /config endpoint accepts GET, POST, PATCH, and DELETE. Before each conformance scenario, the orchestrator POSTs the scenario's mock-llm configuration to reconfigure responses without restarting the server:

```bash
curl -X POST http://localhost:6556/config \
  -H "Content-Type: application/x-yaml" \
  -d @fixtures/mcp-tool-discovery/mock-llm.yaml
```

**POST semantics assumption:** This spec assumes that POST to /config performs a full replacement of the configuration (rules array and sequence counters), not an append. If mock-llm's POST appends to existing rules, use DELETE followed by POST to achieve a clean slate between scenarios. Verify this behavior against the mock-llm version pinned in CI.

### 3.2 Mock-llm Configuration Format

Each conformance fixture includes a mock-llm.yaml file that scripts the LLM's behavior for that scenario. The format follows mock-llm's native YAML schema:

```yaml
rules:
  # First LLM call: trigger tool discovery by calling a tool
  - path: "/v1/chat/completions"
    sequence: 0
    response:
      status: 200
      content: |
        {
          "id": "chatcmpl-{{timestamp}}",
          "object": "chat.completion",
          "model": "{{jmes request body.model}}",
          "choices": [{
            "message": {
              "role": "assistant",
              "content": null,
              "tool_calls": [{
                "id": "call_001",
                "type": "function",
                "function": {
                  "name": "file_read",
                  "arguments": "{\"path\": \"/test/document.txt\"}"
                }
              }]
            },
            "finish_reason": "tool_calls"
          }]
        }

  # Second LLM call: receive tool results, return final response
  - path: "/v1/chat/completions"
    sequence: 1
    response:
      status: 200
      content: |
        {
          "id": "chatcmpl-{{timestamp}}",
          "object": "chat.completion",
          "model": "{{jmes request body.model}}",
          "choices": [{
            "message": {
              "role": "assistant",
              "content": "I found the file contents."
            },
            "finish_reason": "stop"
          }]
        }
```

Scenarios that do not require a mock LLM (MCP client and A2A client self-testing via multi-actor documents) omit the mock-llm.yaml file.

### 3.3 Framework LLM Connectivity

Both frameworks connect to mock-llm identically via OpenAI-compatible base_url:

| Framework | Configuration |
|-----------|--------------|
| **LangGraph** | ChatOpenAI(base_url="http://localhost:6556/v1", api_key="mock-key") |
| **CrewAI** | LLM(model="openai/mock", base_url="http://localhost:6556/v1", api_key="mock-key") |

One mock-llm instance. Zero per-framework LLM code.

---

## 4. Framework Protocol Support

### 4.1 Integration Matrix

| Capability | LangGraph | CrewAI |
|---|---|---|
| **MCP Client** | langchain-mcp-adapters -- MultiServerMCPClient, supports stdio, SSE, streamable HTTP | crewai-tools[mcp] -- MCPServerAdapter, native mcps=[] DSL on agents |
| **MCP Server** | Agent Server (`langgraph-api >= 0.2.3`) -- auto-exposes agents as MCP tools at `/mcp`, streamable HTTP transport. First-party | No native MCP server |
| **AG-UI Server** | ag-ui-langgraph PyPI -- add_langgraph_fastapi_endpoint(). First-party | ag-ui-crewai PyPI -- add_crewai_flow_fastapi_endpoint(). First-party |
| **A2A Client** | Community samples only. No native SDK integration | First-class crewai[a2a] -- A2AClientConfig with auth, streaming, polling |
| **A2A Server** | Agent Server (`langgraph-api >= 0.4.21`) -- auto-exposes agents at `/a2a/{assistant_id}`, supports message/send, message/stream, tasks/get. Agent Card at `/.well-known/agent-card.json`. First-party | First-class A2AServerConfig -- agents serve as both A2A client and server |

**LangGraph Agent Server** is the deployment runtime for LangGraph agents. It provides MCP and A2A server endpoints automatically — any LangGraph agent gets these endpoints for free when deployed via Agent Server. This is distinct from the standalone AG-UI integration (ag-ui-langgraph), which requires explicit FastAPI wiring. Phase 1 uses standalone FastAPI agents; Phase 2 uses Agent Server for MCP/A2A server testing.

### 4.2 Testing Matrix

#### Phase 1: ThoughtJack as Server + AG-UI Client

| ThoughtJack Mode | LangGraph | CrewAI | Self-Test |
|---|---|---|---|
| **MCP server** (TJ serves tools, agent's MCP client connects) | Y | Y | -- |
| **AG-UI client** (TJ connects to agent's AG-UI endpoint) | Y | Y | -- |
| **A2A server** (TJ serves Agent Card, agent's A2A client connects) | -- | Y | -- |
| **MCP client** (TJ connects to TJ's own MCP server) | -- | -- | Y |
| **A2A client** (TJ connects to TJ's own A2A server) | -- | -- | Y |

Phase 1 uses standalone FastAPI reference agents. Self-tests validate ThoughtJack's client implementations against its own server implementations — they catch internal bugs but cannot catch interoperability issues.

#### Phase 2: ThoughtJack as Client Against Framework Servers

| ThoughtJack Mode | LangGraph | CrewAI |
|---|---|---|
| **MCP client** (TJ connects to agent's MCP server) | Y (Agent Server `/mcp`) | -- |
| **A2A client** (TJ connects to agent's A2A server) | Y (Agent Server `/a2a/{id}`) | Y (A2AServerConfig) |

Phase 2 tests ThoughtJack's client modes against real framework server endpoints. These are true interoperability tests — ThoughtJack's implementation of the protocol client talks to a completely independent implementation of the protocol server. This catches mismatches in Agent Card format, task lifecycle semantics, streamable HTTP framing, MCP tool schema interpretation, and other cross-implementation divergences that self-tests cannot detect.

Phase 2 requires the LangGraph reference agent to run inside Agent Server (`langgraph dev`) rather than standalone FastAPI, and requires the CrewAI reference agent to expose an A2A server endpoint.

MCP server conformance (Phase 1) remains the highest-value test surface — both frameworks have mature MCP client support and this is where real-world attacks happen. AG-UI client conformance covers both frameworks via first-party ag-ui-* packages. Phase 2 client-mode tests cover the reverse direction and provide full protocol coverage.

### 4.3 README Coverage Chart

```
## Protocol Conformance Matrix

| ThoughtJack Mode      | LangGraph | CrewAI | Self-Test   |
|-----------------------|-----------|--------|-------------|
| MCP Server            | pass      | pass   | --          |
| AG-UI Client          | pass      | pass   | --          |
| A2A Server            | gap *     | pass   | --          |
| MCP Client            | pass †    | --     | pass        |
| A2A Client            | pass †    | pass † | pass        |

* A2A Server: LangGraph lacks native A2A client integration
† Phase 2: tested against framework server endpoints
```

---

## 5. Reference Agents

### 5.1 Design Contract

Each reference agent is a minimal Python package (~50 lines of agent code) that wires its framework to mock-llm and exposes protocol endpoints. Reference agents are intentionally simple -- their job is to be well-behaved agents that route messages, call tools, and stream responses the way a production agent would. They have no business logic, no custom tools, no persistence.

All reference agents SHALL:

- Accept a --llm-base-url argument (default: http://localhost:6556/v1) for mock-llm connection
- Accept a --port argument (default: 0 for dynamic allocation) for their HTTP endpoint
- Accept MCP server URLs via --mcp-server argument (repeatable, ignored if empty)
- Accept A2A server URLs via --a2a-server argument (repeatable, for frameworks with A2A client support, ignored if empty)
- Serve the AG-UI endpoint at the root path (/)
- Print a readiness marker to stdout: READY port=<bound_port> when accepting connections
- Expose a GET /health endpoint returning 200 when ready
- Exit cleanly on SIGTERM or SIGINT

**AG-UI path convention:** All reference agents mount the AG-UI endpoint at `/`. Standardizing on `/` avoids per-framework path discovery. Users testing their own agents at different paths use ThoughtJack's `--agui-client-endpoint` flag directly.

### 5.2 LangGraph Reference Agent

Implements the standard ReAct pattern with a StateGraph:

```
START -> agent_node (calls LLM) -> has_tool_calls? -> yes -> tool_node (MCP call) -> agent_node
                                                   -> no  -> END
```

**Protocol surfaces:**
- AG-UI endpoint via ag-ui-langgraph (add_langgraph_fastapi_endpoint) at /
- MCP client via langchain-mcp-adapters (MultiServerMCPClient, streamable HTTP transport)

**Dependencies:** langgraph, langchain-openai, langchain-mcp-adapters, ag-ui-langgraph, uvicorn, fastapi

### 5.3 CrewAI Reference Agent

Implements a single-agent Crew with tool-calling capability:

```
Agent (with MCP tools) -> Task execution -> Crew result
```

**Protocol surfaces:**
- AG-UI endpoint via ag-ui-crewai (add_crewai_flow_fastapi_endpoint) at /
- MCP client via crewai-tools[mcp] (MCPServerAdapter / native mcps=[])
- A2A client via crewai[a2a] (A2AClientConfig, connected via --a2a-server argument)

**Dependencies:** crewai, crewai-tools[mcp], crewai[a2a], ag-ui-crewai, uvicorn, fastapi

### 5.4 Phase 2: Framework Server Endpoints

Phase 2 extends the reference agents to expose server endpoints that ThoughtJack's client modes connect to. This enables true interoperability testing — ThoughtJack's protocol client against an independent protocol server implementation.

#### 5.4.1 LangGraph via Agent Server

The Phase 1 LangGraph agent is a standalone FastAPI app. Phase 2 replaces it with Agent Server (`langgraph dev`), which auto-exposes MCP and A2A server endpoints:

```
langgraph dev --port <agent_port>
```

This single process exposes:
- AG-UI (if ag-ui-langgraph is wired into the graph) or custom routes
- MCP server at `/mcp` (streamable HTTP, agents as tools) — requires `langgraph-api >= 0.2.3`
- A2A server at `/a2a/{assistant_id}` (message/send, message/stream, tasks/get) — requires `langgraph-api >= 0.4.21`
- Agent Card at `/.well-known/agent-card.json?assistant_id={id}`
- Health at `/ok`

**Agent structure changes:** Instead of `agent.py` + `server.py`, the agent becomes a `langgraph.json` config pointing at the graph module:

```json
{
  "graphs": {
    "conformance_agent": {
      "path": "./agent.py:graph",
      "description": "ThoughtJack conformance testing agent"
    }
  }
}
```

**Orchestrator changes for Phase 2 LangGraph scenarios:**
- Start command: `langgraph dev --port <agent_port>` (not `python -m`)
- Readiness detection: poll `GET /ok` (Agent Server health endpoint) instead of READY marker
- MCP client endpoint: `http://localhost:<agent_port>/mcp`
- A2A client endpoint: `http://localhost:<agent_port>/a2a/conformance_agent`
- AG-UI endpoint: unchanged (still at `/` if ag-ui-langgraph is wired in)

**Dependencies:** langgraph, langgraph-api >= 0.4.21, langchain-openai, langchain-mcp-adapters, ag-ui-langgraph

#### 5.4.2 CrewAI A2A Server

The Phase 1 CrewAI agent already serves AG-UI and acts as an MCP/A2A client. Phase 2 adds an A2A server endpoint using CrewAI's native `A2AServerConfig`:

```python
from crewai import A2AServerConfig

a2a_config = A2AServerConfig(
    port=a2a_serve_port,
    agent_card={
        "name": "CrewAI Conformance Agent",
        "description": "ThoughtJack conformance testing agent",
        "skills": [{"id": "conformance", "name": "Conformance Test"}],
    }
)
```

The reference agent gains an `--a2a-serve-port <port>` argument for the A2A server endpoint (distinct from `--a2a-server` which specifies upstream A2A servers to connect to as a client).

**Orchestrator changes for Phase 2 CrewAI A2A scenarios:**
- Port allocation: 4 consecutive ports (mcp=base, a2a=base+1, agent=base+2, a2a-serve=base+3)
- Agent startup adds `--a2a-serve-port <a2a_serve_port>`
- A2A client endpoint: `http://localhost:<a2a_serve_port>/.well-known/agent.json` for discovery

---

## 6. Client Conformance Testing

### 6.1 Two Levels of Client Testing

ThoughtJack's MCP client and A2A client modes are tested at two levels:

**Self-tests (Phase 1):** ThoughtJack's client connects to ThoughtJack's own server. A single OATF document defines two actors — one server, one client — and ThoughtJack's multi-actor orchestrator (TJ-SPEC-015) runs both in-process. No external agent, no mock-llm, no Python orchestrator. Self-tests validate ThoughtJack's internal correctness but share the same author's interpretation of the protocol spec on both sides.

**Framework interop tests (Phase 2):** ThoughtJack's client connects to a real framework server endpoint. ThoughtJack's MCP client connects to LangGraph Agent Server's `/mcp` endpoint. ThoughtJack's A2A client connects to LangGraph Agent Server's `/a2a/{id}` endpoint and CrewAI's A2A server. These catch cross-implementation divergences — Agent Card format differences, streamable HTTP framing mismatches, tool schema interpretation gaps — that self-tests structurally cannot detect.

### 6.2 MCP Client Self-Test (Phase 1)

Two actors in one OATF document:

- **Actor 1 (mcp_server):** Serves a known set of tools with deterministic responses.
- **Actor 2 (mcp_client):** Connects to actor 1's endpoint, discovers tools, calls them, and verifies responses via indicators.

The readiness gate (TJ-SPEC-015) ensures the server is bound before the client connects.

### 6.3 A2A Client Self-Test (Phase 1)

Same pattern:

- **Actor 1 (a2a_server):** Serves a static Agent Card and accepts task submissions with fixed results.
- **Actor 2 (a2a_client):** Reads the Agent Card, submits a task, and verifies the result via indicators.

No mock-llm required for either self-test — both sides use static OATF state, no LLM calls.

### 6.4 MCP Client vs LangGraph Agent Server (Phase 2)

ThoughtJack's MCP client connects to `http://localhost:<agent_port>/mcp` (LangGraph Agent Server's streamable HTTP MCP endpoint). The orchestrator starts the agent via `langgraph dev`, waits for health, then invokes ThoughtJack with `--mcp-client-endpoint http://localhost:<agent_port>/mcp`. ThoughtJack discovers tools exposed by the LangGraph agent and calls them. Indicators verify tool discovery succeeds, tool schemas match expectations, and call/response round-trips complete.

This tests a real-world attack surface: ThoughtJack's MCP client auditing a LangGraph agent's MCP server — exactly the flow a security team would use to assess whether an agent's exposed tools leak sensitive data or accept dangerous inputs.

### 6.5 A2A Client vs Framework A2A Servers (Phase 2)

ThoughtJack's A2A client connects to framework A2A server endpoints:

- **LangGraph:** `http://localhost:<agent_port>/a2a/{assistant_id}` — Agent Server auto-exposes A2A. Agent Card at `/.well-known/agent-card.json?assistant_id={id}`.
- **CrewAI:** `http://localhost:<a2a_serve_port>/` — native A2AServerConfig. Agent Card at `/.well-known/agent.json`.

ThoughtJack reads the Agent Card, submits a task via `message/send`, and verifies the response. Indicators check Agent Card discovery, task submission, artifact parsing, and task status lifecycle.

This tests ThoughtJack's ability to interact with real A2A servers as they exist in production — different card locations, different response formats, different streaming implementations.

---

## 7. CLI Interface

### 7.1 Existing Flags Are Sufficient

ThoughtJack needs **no new flags** for conformance testing. SPEC-007 already defines:

```
thoughtjack run --config <path>
  --mcp-server <addr:port>          # Bind MCP server on addr:port (HTTP transport)
  --a2a-server <addr:port>          # Bind A2A server on addr:port
  --agui-client-endpoint <url>      # Connect AG-UI client to agent endpoint
  --output <path>                   # Write verdict JSON to file
```

The orchestrator passes concrete addresses to these existing flags:

```bash
# What the orchestrator invokes (all flags already exist in SPEC-007)
thoughtjack run \
  --config attack.yaml \
  --mcp-server 127.0.0.1:19000 \
  --a2a-server 127.0.0.1:19001 \
  --agui-client-endpoint http://localhost:19002/ \
  --output verdict.json
```

The chicken-and-egg problem (ThoughtJack needs to bind before the agent starts, but the agent needs to know the port) is solved by the orchestrator choosing ports in advance and passing them to both processes. ThoughtJack binds on the specified address; the agent connects to it. No dynamic port discovery needed.

### 7.2 User Workflow (Unchanged)

Users testing their own agents use these same flags directly. No orchestrator needed:

```bash
# User's agent is already running
thoughtjack run --config rug-pull.yaml --agui-client-endpoint http://my-agent:8000/

# User configures their agent to connect to ThoughtJack's MCP server on a known port
thoughtjack run --config rug-pull.yaml --mcp-server 127.0.0.1:9000
```

The orchestrator is strictly for ThoughtJack's own CI. Users never see it.

---

## 8. Python Orchestrator

### 8.1 Design

`run_conformance.py` is a ~120-line Python script that coordinates ThoughtJack, mock-llm, and reference agents for conformance testing. It owns all process lifecycle management — the kind of work that is trivial in Python and painful in Rust.

**Responsibilities:** port allocation, mock-llm health check and config posting, reference agent startup with readiness detection, ThoughtJack invocation with explicit endpoint flags, process group cleanup on exit, verdict comparison against expected results.

**Not responsible for:** protocol simulation (ThoughtJack), agent behavior (reference agents), LLM mocking (mock-llm).

### 8.2 Interface

```bash
# Run a single scenario against a single framework
python run_conformance.py \
  --scenario mcp-tool-discovery \
  --framework langgraph \
  --tj-binary ./target/release/thoughtjack \
  --mock-llm-url http://localhost:6556

# Run a self-test scenario (no framework, no mock-llm)
python run_conformance.py \
  --scenario mcp-client-basic \
  --self-test \
  --tj-binary ./target/release/thoughtjack

# Override ports (for parallel execution on same machine)
python run_conformance.py \
  --scenario mcp-tool-discovery \
  --framework langgraph \
  --base-port 20000
```

| Flag | Default | Description |
|------|---------|-------------|
| `--scenario` | (required) | Fixture directory name under fixtures/ |
| `--framework` | None | Reference agent framework (langgraph, crewai) |
| `--self-test` | false | Self-test mode (no agent, no mock-llm) |
| `--tj-binary` | ./target/release/thoughtjack | Path to ThoughtJack binary |
| `--mock-llm-url` | http://localhost:6556 | Mock-llm base URL |
| `--base-port` | 19000 | Starting port for allocation (3 consecutive) |
| `--timeout` | 30 | Scenario timeout in seconds |
| `--output-dir` | ./results | Directory for verdict JSON files |

### 8.3 Execution Flow

**Framework scenarios (--framework specified):**

```
1. Validate fixture directory contains attack.yaml + mock-llm.yaml
2. Allocate ports: mcp=base, a2a=base+1, agent=base+2
3. Verify mock-llm is healthy (GET /health, 5s timeout)
4. POST mock-llm.yaml to mock-llm /config endpoint
5. Start reference agent in new process group (os.setpgrp):
     python -m reference_agents.<framework>
       --mcp-server http://localhost:<mcp_port>
       --a2a-server http://localhost:<a2a_port>
       --llm-base-url <mock_llm_url>/v1
       --port <agent_port>
6. Wait for agent readiness:
     a. Watch stdout for READY port=<port> (15s timeout)
     b. Poll GET /health (10s timeout, 200ms interval)
7. Invoke ThoughtJack:
     <tj_binary> run
       --config fixtures/<scenario>/attack.yaml
       --mcp-server 127.0.0.1:<mcp_port>
       --a2a-server 127.0.0.1:<a2a_port>
       --agui-client-endpoint http://localhost:<agent_port>/
       --output <output_dir>/<scenario>.json
8. Capture ThoughtJack exit code
9. Kill agent process group (SIGTERM, 5s grace, SIGKILL)
10. Compare verdict against expected.yaml
11. Exit 0 on pass, 1 on comparison fail, preserve TJ exit code otherwise
```

**Self-test scenarios (--self-test):**

```
1. Validate fixture directory contains attack.yaml (no mock-llm.yaml)
2. Invoke ThoughtJack directly (no ports, no agent)
3. Compare verdict against expected.yaml
4. Exit with ThoughtJack's exit code (or 1 if comparison fails)
```

### 8.4 Reference Implementation

```python
#!/usr/bin/env python3
"""run_conformance.py — orchestrator for ThoughtJack conformance tests.

Platform: Linux/macOS only (uses os.setpgrp, os.killpg, select).
"""

import argparse, json, os, select, signal, subprocess, sys, tempfile, time
from pathlib import Path

import requests
import yaml

FIXTURES_DIR = Path(__file__).parent / "fixtures"


def wait_healthy(url, timeout=10, interval=0.2):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            if requests.get(f"{url}/health", timeout=1).ok:
                return True
        except requests.ConnectionError:
            pass
        time.sleep(interval)
    return False


def wait_ready_marker(proc, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break
        ready, _, _ = select.select([proc.stdout], [], [], min(remaining, 0.5))
        if ready:
            line = proc.stdout.readline()
            if not line:
                break
            if line.startswith("READY port="):
                return int(line.strip().split("=", 1)[1])
    raise TimeoutError(f"Agent did not print READY marker within {timeout}s")


def kill_process_group(proc, grace=5):
    if proc.poll() is not None:
        return
    try:
        pgid = os.getpgid(proc.pid)
        os.killpg(pgid, signal.SIGTERM)
        try:
            proc.wait(timeout=grace)
        except subprocess.TimeoutExpired:
            os.killpg(pgid, signal.SIGKILL)
            proc.wait(timeout=2)
    except ProcessLookupError:
        pass


def read_stderr(stderr_path):
    """Read last 50 lines of agent stderr file for diagnostics."""
    try:
        with open(stderr_path) as f:
            lines = f.readlines()
        return "".join(lines[-50:])
    except (FileNotFoundError, IOError):
        return "(no stderr captured)"


def compare_verdict(actual_path, expected_path):
    """Compare actual verdict JSON against expected YAML.

    SPEC-014 format: indicator_verdicts is an array of {id, result, evidence}.
    expected.yaml format: indicators is a dict of {id: {result: ...}}.
    """
    with open(actual_path) as f:
        actual = json.load(f)
    with open(expected_path) as f:
        expected = yaml.safe_load(f)

    if actual["verdict"]["result"] != expected["verdict"]["result"]:
        print(f"  FAIL verdict: {actual['verdict']['result']} != "
              f"{expected['verdict']['result']}")
        return False

    # Convert SPEC-014 array format to dict for comparison
    actual_indicators = {
        v["id"]: v
        for v in actual.get("verdict", {}).get("indicator_verdicts", [])
    }

    failures = 0
    for ind_id, exp in expected.get("verdict", {}).get("indicators", {}).items():
        act = actual_indicators.get(ind_id, {})
        if act.get("result") != exp.get("result"):
            print(f"  FAIL indicator '{ind_id}': "
                  f"{act.get('result', 'missing')} != {exp.get('result')}")
            failures += 1
    return failures == 0


def run_framework_scenario(args):
    fixture_dir = FIXTURES_DIR / args.scenario
    mcp_port, a2a_port, agent_port = (
        args.base_port, args.base_port + 1, args.base_port + 2)
    output = Path(args.output_dir) / f"{args.scenario}.json"
    output.parent.mkdir(parents=True, exist_ok=True)
    stderr_path = Path(args.output_dir) / f"{args.scenario}.agent.stderr"

    # Check framework applicability
    fw_file = fixture_dir / "frameworks.yaml"
    if fw_file.exists():
        with open(fw_file) as f:
            fw_meta = yaml.safe_load(f)
        if args.framework not in fw_meta.get("frameworks", []):
            print(f"SKIP: {args.scenario} not applicable to {args.framework}")
            sys.exit(0)

    # Verify mock-llm
    if not wait_healthy(args.mock_llm_url, timeout=5):
        print(f"ERROR: mock-llm not reachable at {args.mock_llm_url}\n"
              f"Start it with: mock-llm\n"
              f"Or with Docker: docker run -p 6556:6556 ghcr.io/dwmkerr/mock-llm",
              file=sys.stderr)
        sys.exit(10)

    # POST mock-llm config
    mock_cfg = fixture_dir / "mock-llm.yaml"
    if mock_cfg.exists():
        with open(mock_cfg) as f:
            resp = requests.post(
                f"{args.mock_llm_url}/config", data=f.read(),
                headers={"Content-Type": "application/x-yaml"})
            if not resp.ok:
                print(f"ERROR: mock-llm config POST failed: {resp.status_code} "
                      f"{resp.text}", file=sys.stderr)
                sys.exit(10)

    # Start agent — stderr to file to avoid pipe buffer deadlock
    agent_cmd = [
        "python", "-m", f"reference_agents.{args.framework}",
        "--mcp-server", f"http://localhost:{mcp_port}",
        "--a2a-server", f"http://localhost:{a2a_port}",
        "--llm-base-url", f"{args.mock_llm_url}/v1",
        "--port", str(agent_port),
    ]
    stderr_file = open(stderr_path, "w")
    agent = subprocess.Popen(
        agent_cmd, stdout=subprocess.PIPE, stderr=stderr_file,
        text=True, preexec_fn=os.setpgrp)

    try:
        # Wait for readiness with proper error handling
        try:
            wait_ready_marker(agent, timeout=15)
        except TimeoutError:
            print(f"ERROR: Agent failed to start for scenario '{args.scenario}'\n"
                  f"Agent stderr:\n{read_stderr(stderr_path)}", file=sys.stderr)
            sys.exit(10)

        if not wait_healthy(f"http://localhost:{agent_port}", timeout=10):
            print(f"ERROR: Agent health check failed for scenario '{args.scenario}'\n"
                  f"Agent stderr:\n{read_stderr(stderr_path)}", file=sys.stderr)
            sys.exit(10)

        # Run ThoughtJack
        tj_cmd = [
            args.tj_binary, "run",
            "--config", str(fixture_dir / "attack.yaml"),
            "--mcp-server", f"127.0.0.1:{mcp_port}",
            "--a2a-server", f"127.0.0.1:{a2a_port}",
            "--agui-client-endpoint", f"http://localhost:{agent_port}/",
            "--output", str(output),
        ]
        result = subprocess.run(tj_cmd, timeout=args.timeout)

        # Compare verdict
        expected = fixture_dir / "expected.yaml"
        if expected.exists() and output.exists():
            if not compare_verdict(output, expected):
                print(f"Agent stderr:\n{read_stderr(stderr_path)}", file=sys.stderr)
                sys.exit(1)
        sys.exit(result.returncode)

    except subprocess.TimeoutExpired:
        print(f"ERROR: ThoughtJack timed out after {args.timeout}s for "
              f"scenario '{args.scenario}'", file=sys.stderr)
        sys.exit(10)
    finally:
        kill_process_group(agent)
        stderr_file.close()


def run_self_test(args):
    fixture_dir = FIXTURES_DIR / args.scenario
    output = Path(args.output_dir) / f"{args.scenario}.json"
    output.parent.mkdir(parents=True, exist_ok=True)

    try:
        result = subprocess.run([
            args.tj_binary, "run",
            "--config", str(fixture_dir / "attack.yaml"),
            "--output", str(output),
        ], timeout=args.timeout)
    except subprocess.TimeoutExpired:
        print(f"ERROR: ThoughtJack timed out after {args.timeout}s for "
              f"scenario '{args.scenario}'", file=sys.stderr)
        sys.exit(10)

    expected = fixture_dir / "expected.yaml"
    if expected.exists() and output.exists():
        if not compare_verdict(output, expected):
            sys.exit(1)
    sys.exit(result.returncode)


def main():
    p = argparse.ArgumentParser(
        description="ThoughtJack conformance test orchestrator")
    p.add_argument("--scenario", required=True)
    p.add_argument("--framework")
    p.add_argument("--self-test", action="store_true")
    p.add_argument("--tj-binary", default="./target/release/thoughtjack")
    p.add_argument("--mock-llm-url", default="http://localhost:6556")
    p.add_argument("--base-port", type=int, default=19000)
    p.add_argument("--timeout", type=int, default=30)
    p.add_argument("--output-dir", default="./results")
    args = p.parse_args()

    if args.self_test:
        run_self_test(args)
    elif args.framework:
        run_framework_scenario(args)
    else:
        p.error("Either --framework or --self-test is required")

if __name__ == "__main__":
    main()
```

### 8.5 Process Group Cleanup

The orchestrator spawns the agent in a new process group using `preexec_fn=os.setpgrp`. On teardown, `os.killpg(pgid, signal.SIGTERM)` terminates the entire process tree — including uvicorn workers and any grandchild processes — without requiring agents to implement any cleanup logic. After a 5-second grace period, any survivors get SIGKILL. The orchestrator owns the cleanup because it owns the lifecycle.

### 8.6 Port Allocation

The orchestrator allocates consecutive ports starting from `--base-port` (default 19000):

| Offset | Used By | Phase |
|--------|---------|-------|
| +0 | ThoughtJack MCP server (--mcp-server) | 1 |
| +1 | ThoughtJack A2A server (--a2a-server) | 1 |
| +2 | Reference agent AG-UI endpoint (--port) | 1 |
| +3 | CrewAI A2A serve endpoint (--a2a-serve-port) | 2 |

CI matrix entries run in separate GitHub Actions jobs with their own network namespaces — default ports work. For local parallel runs, pass different `--base-port` values.

### 8.7 Phase 2: Agent Server Framework Handling

The orchestrator treats `langgraph-server` as a distinct framework with different startup and readiness semantics:

**Startup:** Instead of `python -m reference_agents.langgraph`, the orchestrator runs:
```bash
langgraph dev --port <agent_port> --dir tests/e2e/reference-agents/langgraph-server/
```

**Readiness:** Agent Server exposes `/ok` for health checks. The orchestrator polls `GET /ok` (no READY marker parsing needed — Agent Server doesn't print one). Agent stdout/stderr is still captured to file for diagnostics.

**Endpoint mapping:** For Phase 2 LangGraph scenarios, ThoughtJack receives:
- `--mcp-client-endpoint http://localhost:<agent_port>/mcp` (for mcp-client-langgraph)
- `--a2a-client-endpoint http://localhost:<agent_port>/a2a/conformance_agent` (for a2a-client-langgraph)

For Phase 2 CrewAI A2A scenarios, the CrewAI agent starts with `--a2a-serve-port <base+3>` and ThoughtJack receives:
- `--a2a-client-endpoint http://localhost:<base+3>/` (for a2a-client-crewai)

**Framework dispatch in orchestrator:**
```python
if args.framework == "langgraph-server":
    agent_cmd = ["langgraph", "dev", "--port", str(agent_port),
                 "--dir", "tests/e2e/reference-agents/langgraph-server/"]
    # No READY marker — poll /ok directly
    if not wait_healthy(f"http://localhost:{agent_port}", timeout=30, path="/ok"):
        ...
elif args.framework in ("langgraph", "crewai"):
    # Phase 1: standalone FastAPI
    agent_cmd = ["python", "-m", f"reference_agents.{args.framework}", ...]
    wait_ready_marker(agent, timeout=15)
    ...
```

The OATF document in each fixture determines which ThoughtJack flags are needed — the orchestrator doesn't need to know whether ThoughtJack is acting as server or client. It just starts the right agent, waits for readiness, and passes the right endpoints.

-----|---------|
| +0 | ThoughtJack MCP server (--mcp-server) |
| +1 | ThoughtJack A2A server (--a2a-server) |
| +2 | Reference agent AG-UI endpoint (--port) |

CI matrix entries run in separate GitHub Actions jobs with their own network namespaces — default ports work. For local parallel runs, pass different `--base-port` values.

---

## 9. Conformance Fixtures

### 9.1 Fixture Structure

Each conformance scenario is a directory containing up to three files:

```
fixtures/<scenario-name>/
|-- attack.yaml             # OATF document defining the conformance test
|-- mock-llm.yaml           # mock-llm rule configuration (optional)
|-- frameworks.yaml          # Applicable frameworks list (optional)
+-- expected.yaml           # Expected verdict and exit code
```

**Naming note:** The file is named `attack.yaml` and the root OATF key is `attack:` because OATF defines a single document type for all agent security tests — adversarial attacks, conformance checks, and fuzz scenarios alike. The naming is an OATF format constraint, not a ThoughtJack choice. Conformance fixtures use `severity.level: info` and indicators that check for correct behavior. The expected verdict is `not_exploited` — meaning "the protocol plumbing worked correctly."

The mock-llm.yaml file is present when the scenario requires scripted LLM behavior (framework tests). It is absent for self-test scenarios. This presence/absence doubles as the self-test discriminator for CI scripts.

The optional `frameworks.yaml` file lists which frameworks the scenario applies to:

```yaml
# fixtures/a2a-task-delegation/frameworks.yaml
frameworks: [crewai]
```

When absent, the scenario applies to all frameworks. The orchestrator checks this file and exits with code 0 (skip, not failure) if the specified framework is not listed. This prevents the nightly workflow from recording permanent false failures for non-applicable pairs (e.g., a2a-task-delegation × langgraph).

### 9.2 Expected Verdict Format

```yaml
exit_code: 0
verdict:
  result: "not_exploited"
  indicators:
    tool_discovery_successful:
      result: matched
    correct_tool_arguments:
      result: matched
```

### 9.3 Fixture Discovery

Fixtures are discovered by scanning the fixtures/ directory. Each subdirectory containing an attack.yaml is a valid fixture. The directory name becomes the scenario identifier.

### 9.4 Initial Conformance Scenarios

#### 9.4.1 Scenario 1: mcp-tool-discovery (Complete Example)

Verifies ThoughtJack's MCP server correctly serves tools and the agent's MCP client discovers and calls them.

- ThoughtJack mode: mcp_server
- Agent action: Discover tools via tools/list, call a tool via tools/call
- Mock-llm: Sequence 0 returns tool_calls, sequence 1 returns text
- Indicators: Tool list received, tool call arguments correct, tool result returned
- Applicable frameworks: LangGraph, CrewAI

**attack.yaml** (complete OATF document):

```yaml
oatf: "0.1"

attack:
  id: "TJ-CONF-001"
  name: "MCP Tool Discovery Conformance"
  version: 1
  description: >
    Conformance test: verifies that ThoughtJack's MCP server correctly serves
    tools and that an agent's MCP client discovers and calls them. This is NOT
    an attack scenario — expected outcome is not_exploited with all indicators
    passing.
  severity:
    level: info
    confidence: high

execution:
  actors:
    - name: mcp_poison
      mode: mcp_server
      phases:
        - name: serve-tools
          state:
            tools:
              - name: file_read
                description: "Read a file from disk"
                inputSchema:
                  type: object
                  properties:
                    path:
                      type: string
                      description: "Path to the file to read"
                  required: ["path"]
            responses:
              - on:
                  method: "tools/call"
                result:
                  content:
                    - type: text
                      text: "Contents of /test/document.txt: Hello from ThoughtJack conformance test."
          triggers:
            - event: "tools/call"
              advance_to: done
          extractors:
            - name: tool_call_args
              source: request
              method: "tools/call"
              type: json_path
              path: "$.params.arguments.path"
        - name: done
          terminal: true

  indicators:
    - id: tool_discovery_successful
      description: "Agent discovered ThoughtJack's tools via tools/list"
      method: expression
      expression: "size(trace.filter(e, e.method == 'tools/list' && e.direction == 'request')) > 0"
      expect: matched

    - id: correct_tool_arguments
      description: "Agent called file_read with the expected path argument"
      method: pattern
      target: "request.params.arguments.path"
      event: "tools/call"
      pattern: "/test/document.txt"
      expect: matched

    - id: tool_result_returned
      description: "ThoughtJack returned a text result for the tool call"
      method: pattern
      target: "response.result.content[0].text"
      event: "tools/call"
      pattern: "Hello from ThoughtJack conformance test"
      expect: matched
```

**mock-llm.yaml** and **expected.yaml**: See §3.2 for mock-llm format. Expected verdict is `not_exploited` with all three indicators `matched`.

#### 9.4.2 Scenario 2: agui-event-streaming

Verifies ThoughtJack's AG-UI client correctly connects to the agent's AG-UI endpoint and receives SSE events.

- ThoughtJack mode: ag_ui_client + mcp_server
- Agent action: Accept RunAgentInput, stream AG-UI events, invoke MCP tools
- Applicable frameworks: LangGraph, CrewAI

#### 9.4.3 Scenario 3: a2a-task-delegation

Verifies ThoughtJack's A2A server correctly serves an Agent Card and handles task delegation.

- ThoughtJack mode: a2a_server
- Agent action: Discover Agent Card, submit task, receive result
- Applicable frameworks: CrewAI

#### 9.4.4 Scenario 4: mcp-client-basic (Phase 1)

Verifies ThoughtJack's MCP client against ThoughtJack's own MCP server (self-test, no agent).

#### 9.4.5 Scenario 5: a2a-client-basic (Phase 1)

Verifies ThoughtJack's A2A client against ThoughtJack's own A2A server (self-test, no agent).

#### 9.4.6 Scenario 6: mcp-client-langgraph (Phase 2)

Verifies ThoughtJack's MCP client against LangGraph Agent Server's `/mcp` endpoint.

- ThoughtJack mode: mcp_client
- Agent: LangGraph agent running in Agent Server, auto-exposing MCP tools at `/mcp`
- Flow: ThoughtJack discovers tools via MCP tools/list, calls a tool, verifies the response
- Mock-llm: Configures the agent to perform a deterministic action when its tool is called
- Indicators: Tool discovery succeeds, tool schema matches, tool call round-trip completes
- Applicable frameworks: langgraph-server

This tests the real-world scenario where ThoughtJack audits an agent's MCP server to check what tools it exposes and how it responds to tool calls.

#### 9.4.7 Scenario 7: a2a-client-langgraph (Phase 2)

Verifies ThoughtJack's A2A client against LangGraph Agent Server's `/a2a/{id}` endpoint.

- ThoughtJack mode: a2a_client
- Agent: LangGraph agent running in Agent Server, auto-exposing A2A at `/a2a/{assistant_id}`
- Flow: ThoughtJack reads Agent Card from `/.well-known/agent-card.json`, submits task via message/send, verifies result
- Mock-llm: Configures a deterministic response
- Indicators: Agent Card discovered, task submitted, response artifact received, task status correct
- Applicable frameworks: langgraph-server

#### 9.4.8 Scenario 8: a2a-client-crewai (Phase 2)

Verifies ThoughtJack's A2A client against CrewAI's native A2A server.

- ThoughtJack mode: a2a_client
- Agent: CrewAI agent with A2AServerConfig, serving Agent Card at `/.well-known/agent.json`
- Flow: ThoughtJack reads Agent Card, submits task, verifies result
- Mock-llm: Configures a deterministic response
- Indicators: Agent Card discovered, task submitted, response artifact received, task status correct
- Applicable frameworks: crewai

Testing against both LangGraph and CrewAI A2A servers catches implementation differences — different Agent Card locations (`agent-card.json` vs `agent.json`), different response formats, different streaming behaviors.

---

## 10. CI Workflows

### 10.1 PR Smoke Tests

```yaml
name: E2E Smoke Tests
on:
  pull_request:
    paths: ["src/**", "tests/e2e/**", "Cargo.toml"]

jobs:
  e2e:
    runs-on: ubuntu-24.04
    strategy:
      matrix:
        include:
          # Phase 1: ThoughtJack as server + AG-UI client
          - { scenario: mcp-tool-discovery,   framework: langgraph  }
          - { scenario: mcp-tool-discovery,   framework: crewai     }
          - { scenario: agui-event-streaming,  framework: langgraph  }
          - { scenario: agui-event-streaming,  framework: crewai     }
          - { scenario: a2a-task-delegation,   framework: crewai     }
          - { scenario: mcp-client-basic,      framework: self-test  }
          - { scenario: a2a-client-basic,      framework: self-test  }
          # Phase 2: ThoughtJack as client against framework servers
          - { scenario: mcp-client-langgraph,  framework: langgraph-server }
          - { scenario: a2a-client-langgraph,  framework: langgraph-server }
          - { scenario: a2a-client-crewai,     framework: crewai     }
      fail-fast: false

    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-python@v5
        with: { python-version: "3.11" }

      - run: cargo build --release
      - run: pip install requests pyyaml

      - name: Install mock-llm
        if: matrix.framework != 'self-test'
        run: npm install -g @dwmkerr/mock-llm

      - name: Start mock-llm and wait
        if: matrix.framework != 'self-test'
        run: |
          mock-llm &
          for i in $(seq 1 30); do
            curl -sf http://localhost:6556/health && break
            sleep 0.5
          done

      - name: Install reference agent
        if: matrix.framework != 'self-test'
        run: |
          if [ "${{ matrix.framework }}" = "langgraph-server" ]; then
            pip install -e tests/e2e/reference-agents/langgraph-server/
          else
            pip install -e tests/e2e/reference-agents/${{ matrix.framework }}/
          fi

      - name: Run conformance test
        run: |
          if [ "${{ matrix.framework }}" = "self-test" ]; then
            python tests/e2e/run_conformance.py \
              --scenario ${{ matrix.scenario }} --self-test
          else
            python tests/e2e/run_conformance.py \
              --scenario ${{ matrix.scenario }} \
              --framework ${{ matrix.framework }}
          fi

      - uses: actions/upload-artifact@v4
        if: always()
        with:
          name: e2e-${{ matrix.scenario }}-${{ matrix.framework }}
          path: results/
```

### 10.2 Nightly Compatibility

```yaml
name: Nightly Compatibility
on:
  schedule: [{ cron: "0 3 * * *" }]

jobs:
  e2e-frameworks:
    runs-on: ubuntu-24.04
    strategy:
      matrix:
        framework: [langgraph, crewai, langgraph-server]
        version: [pinned, latest]
      fail-fast: false
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-python@v5
        with: { python-version: "3.11" }
      - run: cargo build --release
      - run: pip install requests pyyaml
      - run: npm install -g @dwmkerr/mock-llm
      - name: Start mock-llm and wait
        run: |
          mock-llm &
          for i in $(seq 1 30); do
            curl -sf http://localhost:6556/health && break; sleep 0.5
          done
      - name: Install reference agent
        run: |
          AGENT_DIR="${{ matrix.framework }}"
          if [ "${{ matrix.framework }}" = "langgraph-server" ]; then
            AGENT_DIR="langgraph-server"
          fi
          if [ "${{ matrix.version }}" = "latest" ]; then
            pip install -e tests/e2e/reference-agents/$AGENT_DIR/ --upgrade
          else
            pip install -e tests/e2e/reference-agents/$AGENT_DIR/
          fi
      - name: Run all framework scenarios
        run: |
          FAILURES=0
          for d in tests/e2e/fixtures/*/; do
            [ ! -f "$d/mock-llm.yaml" ] && continue
            python tests/e2e/run_conformance.py \
              --scenario "$(basename "$d")" \
              --framework ${{ matrix.framework }} || FAILURES=$((FAILURES+1))
          done
          [ "$FAILURES" -gt 0 ] && exit 1 || exit 0
      - uses: actions/upload-artifact@v4
        if: always()
        with:
          name: e2e-${{ matrix.framework }}-${{ matrix.version }}
          path: results/

  e2e-self-tests:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-python@v5
        with: { python-version: "3.11" }
      - run: cargo build --release
      - run: pip install requests pyyaml
      - name: Run self-test scenarios
        run: |
          for d in tests/e2e/fixtures/*/; do
            [ -f "$d/mock-llm.yaml" ] && continue
            python tests/e2e/run_conformance.py \
              --scenario "$(basename "$d")" --self-test
          done
      - uses: actions/upload-artifact@v4
        if: always()
        with:
          name: e2e-self-tests-nightly
          path: results/
```

---

## 11. Results Schema

### 11.1 Per-Run Output

ThoughtJack produces a standard AttackVerdict (TJ-SPEC-014). No conformance-specific metadata — the verdict is identical whether run by the orchestrator, CI, or a human:

```json
{
  "verdict": {
    "result": "not_exploited",
    "indicator_verdicts": [
      { "id": "tool_discovery_successful", "result": "matched", "evidence": "tools/list request observed in trace" },
      { "id": "correct_tool_arguments", "result": "matched", "evidence": "path argument matched /test/document.txt" }
    ],
    "evaluation_summary": {
      "matched": 2,
      "not_matched": 0,
      "error": 0,
      "skipped": 0
    }
  },
  "execution_summary": {
    "actors": [{ "name": "mcp_poison", "status": "completed" }],
    "duration_ms": 3200,
    "trace_messages": 4
  }
}
```

### 11.2 Verdict Comparison

The orchestrator's `compare_verdict()` checks overall verdict result and per-indicator results against expected.yaml. It exits 1 on mismatch. The orchestrator prints diagnostic context (scenario, framework, ports, both processes' stderr) on failure.

---

## 12. Non-Functional Requirements

### NFR-001: Individual Scenario Performance
Each scenario SHALL complete in under 30 seconds including agent startup, execution, and teardown.

### NFR-002: Full Suite Performance
The complete CI matrix SHALL complete in under 5 minutes with parallel execution.

### NFR-003: No External Network Dependencies
All conformance tests SHALL execute without internet access in mock mode.

### NFR-004: Failure Diagnostics
Failures SHALL include ThoughtJack stderr, agent stderr, verdict JSON, exit codes, scenario name, and framework version.

### NFR-005: Fixture Independence
Each fixture SHALL be self-contained. No shared state between scenarios.

### NFR-006: Zero ThoughtJack Code Changes for New Fixtures
New scenarios require only a fixture directory. No Rust changes, no orchestrator changes.

---

## 13. Edge Cases

### EC-001: Mock-llm Not Running
Orchestrator checks health before starting agent. Exits code 10 with installation instructions.

### EC-002: Agent Fails to Start
Orchestrator kills process group after 15s timeout, prints agent stderr, exits code 10.

### EC-003: Agent Crashes Mid-Scenario
ThoughtJack detects broken connections and reports error verdict. Orchestrator captures both exit codes and agent stderr.

### EC-004: Port Conflict
Agent or ThoughtJack fails to bind. Error messages captured. User resolves with `--base-port`.

### EC-005: Mock-llm Config POST Fails
Orchestrator prints HTTP error, exits code 10.

### EC-006: Scenario Not Applicable to Framework
The orchestrator checks `frameworks.yaml` before starting the agent. If the specified framework is not listed, it prints a SKIP message and exits code 0 (not a failure). The PR matrix avoids non-applicable pairs explicitly; the nightly loop relies on this check for dynamic filtering.

### EC-007: Concurrent Runs
Separate CI jobs have own network namespaces. Local parallel runs use different `--base-port`. Mock-llm config is POSTed fresh per scenario.

### EC-008: ThoughtJack Times Out
Orchestrator's `subprocess.run(timeout=...)` fires, kills agent process group, exits code 10.

---

## 14. Anti-Patterns

| Anti-Pattern | Why | Correct Approach |
|---|---|---|
| Testing attacks with mock LLM | Mock LLM has no judgment — attacks succeed trivially | Conformance with mock, attacks with real LLM |
| Process management in ThoughtJack | 300+ lines of platform-specific Rust unrelated to protocol work | Python orchestrator handles lifecycle |
| Template variable engines in ThoughtJack | Spawn command resolution is orchestration, not protocol | Orchestrator passes concrete values |
| Per-framework echo LLMs | All frameworks support OpenAI-compatible APIs | Single mock-llm |
| Testing via community adapters | Adapter bugs contaminate results | Only natively supported surfaces |
| Hardcoded ports | Conflicts in parallel runs | Orchestrator allocates, passes via flags |
| Sleeping instead of readiness checks | Flaky | READY marker + /health probe |
| Sequential scenarios without resetting mock-llm | Stale sequence counters | Orchestrator POSTs config per scenario |

---

## 15. Definition of Done

### Phase 1

- [ ] @dwmkerr/mock-llm integration verified with both frameworks
- [ ] LangGraph reference agent (~50 lines, AG-UI at /, MCP client)
- [ ] CrewAI reference agent (~50 lines, AG-UI at /, MCP client, A2A client)
- [ ] MCP client self-test fixture (multi-actor OATF, no mock-llm.yaml)
- [ ] A2A client self-test fixture (multi-actor OATF, no mock-llm.yaml)
- [ ] `run_conformance.py` orchestrator: lifecycle, readiness, cleanup, verdict comparison
- [ ] Scenario 1: mcp-tool-discovery (LangGraph, CrewAI)
- [ ] Scenario 2: agui-event-streaming (LangGraph, CrewAI)
- [ ] Scenario 3: a2a-task-delegation (CrewAI)
- [ ] Scenario 4: mcp-client-basic (self-test)
- [ ] Scenario 5: a2a-client-basic (self-test)
- [ ] README coverage chart
- [ ] PR smoke test workflow
- [ ] Nightly compatibility workflow (frameworks + self-tests)
- [ ] All edge cases (EC-001 through EC-008) handled
- [ ] NFR-001 through NFR-006 met

### Phase 2

- [ ] LangGraph Agent Server reference agent (langgraph.json + agent.py)
- [ ] Agent Server startup and readiness handling in orchestrator (`langgraph dev`, `/ok` health)
- [ ] CrewAI A2A server endpoint (A2AServerConfig, --a2a-serve-port)
- [ ] Scenario 6: mcp-client-langgraph (TJ MCP client → Agent Server /mcp)
- [ ] Scenario 7: a2a-client-langgraph (TJ A2A client → Agent Server /a2a/{id})
- [ ] Scenario 8: a2a-client-crewai (TJ A2A client → CrewAI A2A server)
- [ ] PR smoke test matrix includes Phase 2 entries
- [ ] Nightly compatibility includes langgraph-server
- [ ] README coverage chart updated with Phase 2 results

---

## 16. Future Work

### 16.1 TJ-SPEC-022: OATF Actor Model
Makes mock-llm a first-class OATF actor with lifecycle management, inline config, phase-aware rules, and actor substitution (mock → real LLM).

### 16.2 Benchmark Spec
Live LLM integration, statistical execution (K runs), scoring, ranking, public display.

### 16.3 Additional Frameworks
The reference agent contract is framework-agnostic. AutoGen, Semantic Kernel, Haystack agents can be added without changes to ThoughtJack or the orchestrator. Each framework that supports Agent Server–style MCP/A2A endpoints gets client-mode testing for free.

### 16.4 OpenAI Agents SDK
Deferred — lacks AG-UI server support. When available, add a reference agent; fixtures already exist from LangGraph/CrewAI.

### 16.5 AG-UI via Agent Server
It is unclear whether LangGraph Agent Server natively exposes an AG-UI endpoint or whether this requires explicit ag-ui-langgraph FastAPI wiring. If Agent Server gains native AG-UI support, the Phase 2 LangGraph agent could replace the Phase 1 standalone agent entirely — one process, all protocols. Investigate during Phase 2 implementation.

---

## 17. References

- [TJ-SPEC-007: CLI Interface](./TJ-SPEC-007_CLI_Interface.md)
- [TJ-SPEC-013: OATF Integration](./TJ-SPEC-013_OATF_Integration.md)
- [TJ-SPEC-014: Verdict & Evaluation Output](./TJ-SPEC-014_Verdict_Evaluation_Output.md)
- [TJ-SPEC-015: Multi-Actor Orchestration](./TJ-SPEC-015_Multi_Actor_Orchestration.md)
- [TJ-SPEC-016: AG-UI Protocol Support](./TJ-SPEC-016_AGUI_Protocol_Support.md)
- [TJ-SPEC-017: A2A Protocol Support](./TJ-SPEC-017_A2A_Protocol_Support.md)
- [TJ-SPEC-018: MCP Client Mode](./TJ-SPEC-018_MCP_Client_Mode.md)
- [@dwmkerr/mock-llm](https://github.com/dwmkerr/mock-llm)
- [LangGraph](https://langchain-ai.github.io/langgraph/)
- [LangGraph Agent Server — MCP endpoint](https://docs.langchain.com/langsmith/server-mcp)
- [LangGraph Agent Server — A2A endpoint](https://docs.langchain.com/langsmith/server-a2a)
- [langchain-mcp-adapters](https://github.com/langchain-ai/langchain-mcp-adapters)
- [ag-ui-langgraph](https://pypi.org/project/ag-ui-langgraph/)
- [CrewAI](https://docs.crewai.com/)
- [ag-ui-crewai](https://pypi.org/project/ag-ui-crewai/)
- [AG-UI Protocol](https://github.com/ag-ui-protocol/ag-ui)
