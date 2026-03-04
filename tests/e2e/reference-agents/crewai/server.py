"""AG-UI server wrapping the CrewAI reference agent.

Thin SSE endpoint that triggers a real CrewAI Crew with MCP tools.
Crew creation is deferred to the first request (lazy init) to avoid
a startup deadlock: the agent needs TJ's MCP server, but TJ needs the
agent's AG-UI endpoint.

Prints ``READY port=<N>`` to stdout when the server is listening.
"""

from __future__ import annotations

import argparse
import json
import uuid

import uvicorn
from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse, StreamingResponse

from crewai_tools import MCPServerAdapter

from agent import create_crew

_parser = argparse.ArgumentParser(description="CrewAI AG-UI reference agent")
_parser.add_argument("--llm-base-url", required=True, help="Mock LLM base URL")
_parser.add_argument("--port", type=int, default=8000, help="Listen port")
_parser.add_argument(
    "--mcp-server", action="append", default=[], help="MCP server URL (repeatable)",
)
_parser.add_argument(
    "--a2a-server", action="append", default=[], help="A2A server URL (repeatable)",
)
_args = _parser.parse_args()

app = FastAPI()

# Lazily initialized on first AG-UI request (after TJ has started).
_crew = None
_adapters: list[MCPServerAdapter] = []


def _sse(data: dict) -> str:
    return f"data: {json.dumps(data)}\n\n"


@app.get("/health")
async def health() -> JSONResponse:
    return JSONResponse({"status": "ok"})


@app.post("/")
async def agent_endpoint(request: Request) -> StreamingResponse:
    """AG-UI endpoint: lazily creates the CrewAI crew, kicks it off, streams SSE."""
    global _crew, _adapters
    if _crew is None:
        tools = []
        for url in _args.mcp_server:
            # MCPServerAdapter expects a dict with url + transport, not a bare URL.
            # TJ's MCP HTTP transport exposes GET /sse (SSE) + POST /message.
            # The orchestrator passes http://host:port/message; derive the SSE URL.
            sse_url = url.rsplit("/", 1)[0] + "/sse" if "/message" in url else url
            adapter = MCPServerAdapter(
                {"url": sse_url, "transport": "sse"},
                connect_timeout=10,
            )
            _adapters.append(adapter)
            tools.extend(adapter.tools)
        _crew = create_crew(_args.llm_base_url, tools=tools, a2a_server_urls=_args.a2a_server)

    body = await request.json()
    messages = body.get("messages", [])
    user_msg = "Use available tools as instructed."
    for m in reversed(messages):
        if m.get("role") == "user":
            user_msg = m.get("content", user_msg)
            break

    run_id = str(uuid.uuid4())
    thread_id = body.get("threadId", str(uuid.uuid4()))

    async def generate():
        yield _sse({"type": "RUN_STARTED", "runId": run_id, "threadId": thread_id})

        try:
            result = _crew.kickoff(inputs={"user_message": user_msg})
            content = str(result)
        except Exception as exc:
            yield _sse({"type": "RUN_ERROR", "runId": run_id, "message": str(exc)})
            return

        msg_id = str(uuid.uuid4())
        yield _sse({"type": "TEXT_MESSAGE_START", "messageId": msg_id, "role": "assistant"})
        yield _sse({"type": "TEXT_MESSAGE_CONTENT", "messageId": msg_id, "delta": content or "done"})
        yield _sse({"type": "TEXT_MESSAGE_END", "messageId": msg_id})
        yield _sse({"type": "RUN_FINISHED", "runId": run_id, "threadId": thread_id})

    return StreamingResponse(generate(), media_type="text/event-stream")


@app.on_event("startup")
async def on_startup() -> None:
    print(f"READY port={_args.port}", flush=True)


@app.on_event("shutdown")
async def on_shutdown() -> None:
    for adapter in _adapters:
        try:
            adapter.__exit__(None, None, None)
        except Exception:
            pass


if __name__ == "__main__":
    uvicorn.run(app, host="127.0.0.1", port=_args.port, log_level="warning")
