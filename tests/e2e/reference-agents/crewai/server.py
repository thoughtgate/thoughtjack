"""AG-UI server wrapping the CrewAI reference agent.

Thin SSE endpoint that triggers a real CrewAI Crew with MCP tools.
Prints ``READY port=<N>`` to stdout when the server is listening.
"""

from __future__ import annotations

import argparse
import json
import uuid

import uvicorn
from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse, StreamingResponse

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

# Build the crew at import time (MCP tools are discovered here).
_crew = create_crew(_args.llm_base_url, _args.mcp_server, _args.a2a_server)


@app.get("/health")
async def health() -> JSONResponse:
    return JSONResponse({"status": "ok"})


def _sse(data: dict) -> str:
    return f"data: {json.dumps(data)}\n\n"


@app.post("/")
async def agent_endpoint(request: Request) -> StreamingResponse:
    """AG-UI compatible endpoint: triggers CrewAI crew and streams SSE events."""
    body = await request.json()

    # Extract user message from AG-UI RunAgentInput
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


if __name__ == "__main__":
    uvicorn.run(app, host="127.0.0.1", port=_args.port, log_level="warning")
