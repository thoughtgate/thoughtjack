"""CrewAI reference agent for ThoughtJack e2e conformance tests.

Creates a Flow[CopilotKitState] that uses MCP tools via crewai-tools.
"""

from __future__ import annotations

from ag_ui_crewai import CopilotKitState, copilotkit_stream
from crewai.flow.flow import Flow, start
from litellm import acompletion


class E2ETestFlow(Flow[CopilotKitState]):
    """Simple agentic chat flow that streams LLM responses with tool support."""

    model: str = "openai/mock"
    base_url: str = "http://localhost:6556"
    api_key: str = "mock-key"

    @start()
    async def chat(self):
        """Run a single LLM completion with available tools."""
        # Gather tools from CopilotKit actions (wired by AG-UI endpoint)
        tools = self.state.copilotkit.actions if self.state.copilotkit else []

        response = await copilotkit_stream(
            acompletion(
                model=self.model,
                messages=[{"role": "system", "content": "You are a test agent."}]
                + [m.model_dump() for m in self.state.messages],
                tools=tools if tools else None,
                api_base=self.base_url,
                api_key=self.api_key,
                stream=True,
            )
        )

        # Append assistant response to state
        self.state.messages.append(response)
        return response
