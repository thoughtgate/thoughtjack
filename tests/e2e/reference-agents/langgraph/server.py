"""AG-UI server wrapping the LangGraph reference agent.

Starts an HTTP server exposing the LangGraph agent via AG-UI protocol.
MCP tool discovery is deferred to the first request (lazy init) to avoid
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

from agent import create_graph

_parser = argparse.ArgumentParser(description="LangGraph AG-UI reference agent")
_parser.add_argument("--llm-base-url", required=True, help="LLM base URL (OpenAI-compatible)")
_parser.add_argument("--port", type=int, default=8000, help="Listen port")
_parser.add_argument(
    "--mcp-server", action="append", default=[], help="MCP server URL (repeatable)",
)
_parser.add_argument("--api-key", default="mock-key", help="LLM API key")
_parser.add_argument("--model", default="mock", help="LLM model name")
_parser.add_argument(
    "--default-headers", default=None,
    help="JSON string of extra HTTP headers for the LLM client",
)
_args = _parser.parse_args()

app = FastAPI()

# Lazily initialized on first AG-UI request (after TJ has started).
_graph = None


def _sse(data: dict) -> str:
    return f"data: {json.dumps(data)}\n\n"


@app.get("/health")
async def health() -> JSONResponse:
    return JSONResponse({"status": "ok"})


@app.post("/")
async def agent_endpoint(request: Request) -> StreamingResponse:
    """AG-UI endpoint: lazily creates the LangGraph agent, runs it, streams SSE."""
    global _graph
    if _graph is None:
        headers = json.loads(_args.default_headers) if _args.default_headers else None
        _graph = await create_graph(
            _args.llm_base_url,
            _args.mcp_server,
            api_key=_args.api_key,
            model=_args.model,
            default_headers=headers,
        )

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
            from langchain_core.messages import HumanMessage

            result = await _graph.ainvoke({"messages": [HumanMessage(content=user_msg)]})
            final_msgs = result.get("messages", [])
            content = final_msgs[-1].content if final_msgs else "done"
        except Exception as exc:
            yield _sse({"type": "RUN_ERROR", "runId": run_id, "message": str(exc)})
            return

        msg_id = str(uuid.uuid4())
        yield _sse({"type": "TEXT_MESSAGE_START", "messageId": msg_id, "role": "assistant"})
        yield _sse({"type": "TEXT_MESSAGE_CONTENT", "messageId": msg_id, "delta": content})
        yield _sse({"type": "TEXT_MESSAGE_END", "messageId": msg_id})
        yield _sse({"type": "RUN_FINISHED", "runId": run_id, "threadId": thread_id})

    return StreamingResponse(generate(), media_type="text/event-stream")


@app.on_event("startup")
async def on_startup() -> None:
    print(f"READY port={_args.port}", flush=True)


if __name__ == "__main__":
    uvicorn.run(app, host="0.0.0.0", port=_args.port, log_level="warning")
