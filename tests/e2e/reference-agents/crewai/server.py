"""AG-UI server wrapping the CrewAI reference agent.

Starts an HTTP server exposing the CrewAI agent via AG-UI protocol.
Prints ``READY port=<N>`` to stdout when the server is listening.
"""

from __future__ import annotations

import argparse
import asyncio
import signal
import sys

import uvicorn
from ag_ui_crewai import add_crewai_flow_fastapi_endpoint
from fastapi import FastAPI
from fastapi.responses import JSONResponse

from agent import E2ETestFlow


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="CrewAI AG-UI reference agent")
    parser.add_argument("--llm-base-url", required=True, help="Mock LLM base URL")
    parser.add_argument("--port", type=int, default=0, help="Listen port (0 = random)")
    parser.add_argument(
        "--mcp-server",
        action="append",
        default=[],
        help="MCP server URL (repeatable)",
    )
    parser.add_argument(
        "--a2a-server",
        action="append",
        default=[],
        help="A2A server URL (repeatable)",
    )
    return parser.parse_args()


app = FastAPI()


@app.get("/health")
async def health() -> JSONResponse:
    return JSONResponse({"status": "ok"})


async def main() -> None:
    args = parse_args()

    flow = E2ETestFlow()
    flow.model = "openai/mock"
    flow.base_url = args.llm_base_url
    flow.api_key = "mock-key"

    add_crewai_flow_fastapi_endpoint(app, flow, "/")

    config = uvicorn.Config(
        app,
        host="127.0.0.1",
        port=args.port,
        log_level="warning",
    )
    server = uvicorn.Server(config)

    # Handle shutdown signals
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGTERM, signal.SIGINT):
        loop.add_signal_handler(sig, lambda: server.should_exit.__setattr__("_value", True))

    # Start server and emit readiness marker
    await server.startup()
    for sock in server.servers:
        port = sock.sockets[0].getsockname()[1]
        print(f"READY port={port}", flush=True)
        break

    await server.main_loop()
    await server.shutdown()


if __name__ == "__main__":
    asyncio.run(main())
