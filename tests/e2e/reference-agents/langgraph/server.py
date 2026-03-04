"""AG-UI server wrapping the LangGraph reference agent.

Starts an HTTP server exposing the LangGraph agent via AG-UI protocol.
Prints ``READY port=<N>`` to stdout when the server is listening.
"""

from __future__ import annotations

import argparse
import sys

import uvicorn
from ag_ui_langgraph import LangGraphAgent, add_langgraph_fastapi_endpoint
from fastapi import FastAPI
from fastapi.responses import JSONResponse

from agent import create_graph

# Parsed at module level so startup event can access the port.
_parser = argparse.ArgumentParser(description="LangGraph AG-UI reference agent")
_parser.add_argument("--llm-base-url", required=True, help="Mock LLM base URL")
_parser.add_argument("--port", type=int, default=8000, help="Listen port")
_parser.add_argument(
    "--mcp-server", action="append", default=[], help="MCP server URL (repeatable)",
)
_args = _parser.parse_args()

app = FastAPI()


@app.get("/health")
async def health() -> JSONResponse:
    return JSONResponse({"status": "ok"})


@app.on_event("startup")
async def on_startup() -> None:
    graph = await create_graph(_args.llm_base_url, _args.mcp_server)
    agent = LangGraphAgent(name="e2e-test", graph=graph)
    add_langgraph_fastapi_endpoint(app, agent, "/")
    print(f"READY port={_args.port}", flush=True)


if __name__ == "__main__":
    uvicorn.run(app, host="127.0.0.1", port=_args.port, log_level="warning")
