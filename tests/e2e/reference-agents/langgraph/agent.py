"""LangGraph reference agent for ThoughtJack e2e conformance tests.

Creates a ReAct-pattern agent that uses MCP tools via langchain-mcp-adapters.
"""

from __future__ import annotations

from langchain_mcp_adapters.client import MultiServerMCPClient
from langchain_openai import ChatOpenAI
from langgraph.prebuilt import create_react_agent


async def create_graph(
    llm_base_url: str,
    mcp_server_urls: list[str],
) -> object:
    """Build a compiled LangGraph StateGraph wired to MCP tool servers.

    Args:
        llm_base_url: Base URL for the mock LLM (OpenAI-compatible).
        mcp_server_urls: List of MCP server HTTP URLs.

    Returns:
        A compiled LangGraph graph.
    """
    llm = ChatOpenAI(
        base_url=llm_base_url,
        api_key="mock-key",
        model="mock",
    )

    # Build MCP client config: one entry per server URL
    mcp_servers = {}
    for i, url in enumerate(mcp_server_urls):
        mcp_servers[f"server_{i}"] = {
            "transport": "streamable_http",
            "url": url,
        }

    client = MultiServerMCPClient(mcp_servers)
    tools = await client.get_tools()

    graph = create_react_agent(llm, tools)
    return graph
