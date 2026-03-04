"""CrewAI reference agent for ThoughtJack e2e conformance tests.

Creates a single-agent Crew wired to MCP tools and optional A2A servers.
"""

from __future__ import annotations

from crewai import Agent, Crew, LLM, Task


def create_crew(
    llm_base_url: str,
    tools: list | None = None,
    a2a_server_urls: list[str] | None = None,
) -> Crew:
    """Build a CrewAI Crew wired to pre-initialized tools and A2A agents.

    Args:
        llm_base_url: Base URL for the mock LLM (OpenAI-compatible).
        tools: Pre-initialized tool list (from MCPServerAdapter etc.).
        a2a_server_urls: List of A2A server URLs.

    Returns:
        A Crew instance ready to kickoff.
    """
    llm = LLM(
        model="openai/mock",
        base_url=llm_base_url,
        api_key="mock-key",
    )

    agent = Agent(
        role="E2E Test Agent",
        goal="Execute tool calls and tasks as instructed",
        backstory="A test agent for e2e conformance testing",
        llm=llm,
        tools=tools or [],
        verbose=False,
    )

    task = Task(
        description="Follow instructions and use available tools",
        agent=agent,
        expected_output="Task result",
    )

    crew_kwargs: dict = {
        "agents": [agent],
        "tasks": [task],
        "verbose": False,
    }

    # Wire A2A agents if provided
    if a2a_server_urls:
        crew_kwargs["a2a_agents"] = [
            {"url": url} for url in a2a_server_urls
        ]

    return Crew(**crew_kwargs)
