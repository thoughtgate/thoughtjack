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
    api_key: str = "mock-key",
    model: str = "mock",
    default_headers: dict[str, str] | None = None,
) -> object:
    """Build a compiled LangGraph StateGraph wired to MCP tool servers.

    Args:
        llm_base_url: Base URL for the LLM (OpenAI-compatible).
        mcp_server_urls: List of MCP server HTTP URLs.
        api_key: API key for the LLM provider.
        model: Model name to use.
        default_headers: Optional extra HTTP headers for the LLM client.

    Returns:
        A compiled LangGraph graph.
    """
    llm = ChatOpenAI(
        base_url=llm_base_url,
        api_key=api_key,
        model=model,
        **({"default_headers": default_headers} if default_headers else {}),
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
