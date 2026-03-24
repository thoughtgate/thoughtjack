#!/usr/bin/env python3
"""ThoughtJack E2E conformance test orchestrator.

Manages the lifecycle of mock-llm, reference agents, and ThoughtJack to run
end-to-end conformance scenarios. See TJ-SPEC-021 §8.4.

Exit codes:
    0  - All assertions passed
    1  - Verdict mismatch
    10 - Infrastructure error (process crash, timeout, etc.)
"""

from __future__ import annotations

import argparse
import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

import yaml

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def wait_healthy(url: str, timeout: float = 10.0, interval: float = 0.5) -> None:
    """Poll GET /health until 200 or timeout."""
    import urllib.request
    import urllib.error

    deadline = time.monotonic() + timeout
    last_err = None
    while time.monotonic() < deadline:
        try:
            req = urllib.request.Request(f"{url}/health", method="GET")
            with urllib.request.urlopen(req, timeout=2) as resp:
                if resp.status == 200:
                    return
        except (urllib.error.URLError, OSError) as exc:
            last_err = exc
        time.sleep(interval)
    raise TimeoutError(f"Health check failed for {url} after {timeout}s: {last_err}")


def wait_ready_marker(proc: subprocess.Popen, timeout: float = 15.0) -> int:
    """Read stdout for ``READY port=<N>`` and return the port number."""
    deadline = time.monotonic() + timeout
    assert proc.stdout is not None
    while time.monotonic() < deadline:
        line = proc.stdout.readline()
        if not line:
            rc = proc.poll()
            if rc is not None:
                raise RuntimeError(f"Agent exited early with code {rc}")
            continue
        text = line.strip()
        if text.startswith("READY port="):
            return int(text.split("=", 1)[1])
    raise TimeoutError(f"Agent did not emit READY marker within {timeout}s")


def kill_process_group(proc: subprocess.Popen, grace: float = 3.0) -> None:
    """Send SIGTERM to process group, wait, then SIGKILL if needed."""
    if proc.poll() is not None:
        return
    try:
        pgid = os.getpgid(proc.pid)
        os.killpg(pgid, signal.SIGTERM)
    except (ProcessLookupError, PermissionError):
        return
    try:
        proc.wait(timeout=grace)
    except subprocess.TimeoutExpired:
        try:
            pgid = os.getpgid(proc.pid)
            os.killpg(pgid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass
        proc.wait(timeout=5)


def read_stderr(path: Path) -> str:
    """Return last 50 lines of a stderr capture file."""
    if not path.exists():
        return "(no stderr file)"
    lines = path.read_text().splitlines()
    return "\n".join(lines[-50:])


def compare_verdict(actual_path: Path, expected_path: Path) -> bool:
    """Compare verdict JSON against expected YAML.

    Returns True if the verdict matches expectations.
    """
    with open(actual_path) as f:
        actual = json.load(f)
    with open(expected_path) as f:
        expected = yaml.safe_load(f)

    passed = True

    # Check top-level verdict result — fail explicitly if missing
    actual_verdict = actual.get("verdict", {})
    actual_result = actual_verdict.get("result")
    expected_result = expected.get("verdict", {}).get("result")
    if actual_result is None:
        print("FAIL: verdict.result is missing from output")
        print(f"  verdict JSON: {json.dumps(actual_verdict, indent=2)[:500]}")
        return False
    if actual_result != expected_result:
        print(f"FAIL: verdict.result: expected={expected_result}, actual={actual_result}")
        print(f"  verdict JSON: {json.dumps(actual_verdict, indent=2)[:500]}")
        passed = False

    # Check individual indicator results if specified
    expected_indicators = expected.get("indicators", {})
    actual_ind_list = actual_verdict.get("indicator_verdicts", [])
    if expected_indicators:
        if not actual_ind_list:
            print(
                f"FAIL: expected {len(expected_indicators)} indicator(s) "
                f"but verdict contains no indicator_verdicts"
            )
            passed = False
        else:
            actual_indicators = {
                ind["id"]: ind
                for ind in actual_ind_list
                if "id" in ind
            }
            for ind_id, spec in expected_indicators.items():
                expected_ind_result = spec.get("result")
                actual_ind = actual_indicators.get(ind_id)
                if actual_ind is None:
                    print(
                        f"FAIL: indicator '{ind_id}': "
                        f"expected={expected_ind_result}, not found in output"
                    )
                    print(
                        f"  available indicators: "
                        f"{[i.get('id') for i in actual_ind_list]}"
                    )
                    passed = False
                    continue
                actual_ind_result = actual_ind.get("result")
                if actual_ind_result != expected_ind_result:
                    print(
                        f"FAIL: indicator '{ind_id}': "
                        f"expected={expected_ind_result}, actual={actual_ind_result}"
                    )
                    # Print evidence for debugging
                    evidence = actual_ind.get("evidence")
                    if evidence:
                        print(f"  evidence: {json.dumps(evidence)[:300]}")
                    passed = False

    if not passed:
        return False

    # Execution health checks: verify actors actually ran
    exec_summary = actual.get("execution_summary", {})
    actors = exec_summary.get("actors", [])

    # Fail if ALL actors errored (infrastructure failure)
    error_actors = [a for a in actors if a.get("status") == "error"]
    if error_actors and len(error_actors) == len(actors):
        for a in error_actors:
            print(f"FAIL: actor '{a['name']}' errored: {a.get('error', '?')}")
        return False

    # Fail if zero trace messages were recorded (nothing actually happened)
    trace_count = exec_summary.get("trace_messages", 0)
    min_traces = expected.get("execution", {}).get("min_trace_messages", 1)
    if trace_count < min_traces:
        print(
            f"FAIL: expected at least {min_traces} trace messages, "
            f"got {trace_count} (actors may not have exchanged any data)"
        )
        # Print actor statuses for debugging
        for a in actors:
            status = a.get("status", "?")
            err = a.get("error", "")
            detail = f" — {err}" if err else ""
            print(f"  actor '{a['name']}': {status}{detail}")
        return False

    return True


def post_mock_llm_config(mock_llm_url: str, config_path: Path) -> None:
    """POST mock-llm configuration to replace current rules."""
    import urllib.error
    import urllib.request

    with open(config_path) as f:
        data = f.read().encode("utf-8")
    req = urllib.request.Request(
        f"{mock_llm_url}/config",
        data=data,
        headers={"Content-Type": "application/x-yaml"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=5) as resp:
            if resp.status not in (200, 204):
                raise RuntimeError(f"mock-llm config POST returned {resp.status}")
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8", errors="replace")[:500]
        raise RuntimeError(
            f"mock-llm config POST failed: {e.code} {e.reason}\n{body}"
        ) from e


# ---------------------------------------------------------------------------
# Scenario runners
# ---------------------------------------------------------------------------


def run_framework_scenario(
    scenario_dir: Path,
    framework: str,
    tj_binary: Path,
    mock_llm_url: str,
    base_port: int,
    timeout: int,
    output_dir: Path,
) -> bool:
    """Run a framework-based e2e scenario. Returns True on pass."""
    attack_yaml = scenario_dir / "attack.yaml"
    mock_llm_yaml = scenario_dir / "mock-llm.yaml"
    expected_yaml = scenario_dir / "expected.yaml"
    frameworks_yaml = scenario_dir / "frameworks.yaml"

    # Check framework applicability
    if frameworks_yaml.exists():
        with open(frameworks_yaml) as f:
            fw_spec = yaml.safe_load(f)
        allowed = fw_spec.get("frameworks", [])
        if framework not in allowed:
            print(f"SKIP: {scenario_dir.name} not applicable to {framework}")
            return True

    scenario_name = scenario_dir.name
    verdict_path = output_dir / f"{scenario_name}-{framework}-verdict.json"
    stderr_path = output_dir / f"{scenario_name}-{framework}.stderr"

    mcp_port = base_port
    a2a_port = base_port + 1
    agent_port = base_port + 2

    # Load attack YAML to detect modes
    with open(attack_yaml) as f:
        attack_doc = yaml.safe_load(f)
    execution = attack_doc.get("attack", {}).get("execution", {})
    mode = execution.get("mode", "")
    actors = execution.get("actors", [])
    modes = [mode] if mode else [a.get("mode", "") for a in actors]

    agent_url = f"http://127.0.0.1:{agent_port}"
    agent_proc = None
    try:
        # 1. Configure mock-llm
        wait_healthy(mock_llm_url, timeout=5)
        post_mock_llm_config(mock_llm_url, mock_llm_yaml)

        # 2. Start reference agent first (it defers MCP connection to request time)
        agent_dir = Path(__file__).parent / "reference-agents" / framework
        server_py = agent_dir / "server.py"
        agent_cmd = [
            sys.executable, str(server_py),
            "--llm-base-url", mock_llm_url,
            "--port", str(agent_port),
        ]
        if any(m == "mcp_server" for m in modes):
            agent_cmd.extend(["--mcp-server", f"http://127.0.0.1:{mcp_port}/message"])
        if any(m == "a2a_server" for m in modes):
            agent_cmd.extend(["--a2a-server", f"http://127.0.0.1:{a2a_port}"])

        with open(stderr_path, "w") as stderr_file:
            agent_proc = subprocess.Popen(
                agent_cmd,
                stdout=subprocess.PIPE,
                stderr=stderr_file,
                text=True,
                preexec_fn=os.setpgrp,
            )

        # 3. Wait for agent readiness (READY printed before MCP connection)
        actual_port = wait_ready_marker(agent_proc, timeout=15)
        wait_healthy(f"http://127.0.0.1:{actual_port}", timeout=10)

        # 4. Run ThoughtJack (agent will lazily connect to TJ's MCP on first request)
        tj_cmd = [
            str(tj_binary), "run",
            "--config", str(attack_yaml),
            "--output", str(verdict_path),
            "--max-session", f"{timeout}s",
            "--no-semantic",
        ]
        if any(m == "mcp_server" for m in modes):
            tj_cmd.extend(["--mcp-server", f"127.0.0.1:{mcp_port}"])
        if any(m == "a2a_server" for m in modes):
            tj_cmd.extend(["--a2a-server", f"127.0.0.1:{a2a_port}"])
        tj_cmd.extend(["--agui-client-endpoint", f"{agent_url}/"])

        print(f"RUN: {' '.join(tj_cmd)}")
        result = subprocess.run(tj_cmd, capture_output=True, text=True, timeout=timeout + 10)

        if result.returncode not in (0, 1, 2, 3, 4, 5):
            print(f"FAIL: ThoughtJack exited with code {result.returncode}")
            if result.stderr:
                print(f"  stderr: {result.stderr[:500]}")
            return False

        # 5. Compare verdict
        if not verdict_path.exists():
            print(f"FAIL: No verdict file at {verdict_path}")
            return False

        return compare_verdict(verdict_path, expected_yaml)

    except (TimeoutError, RuntimeError, subprocess.TimeoutExpired) as exc:
        print(f"INFRA ERROR: {exc}")
        if stderr_path.exists():
            print(f"Agent stderr:\n{read_stderr(stderr_path)}")
        return False
    finally:
        if agent_proc is not None:
            kill_process_group(agent_proc)


def run_self_test(
    scenario_dir: Path,
    tj_binary: Path,
    base_port: int,
    timeout: int,
    output_dir: Path,
) -> bool:
    """Run a self-test scenario (no agent, no mock-llm). Returns True on pass."""
    attack_yaml = scenario_dir / "attack.yaml"
    expected_yaml = scenario_dir / "expected.yaml"
    scenario_name = scenario_dir.name
    verdict_path = output_dir / f"{scenario_name}-selftest-verdict.json"

    mcp_port = base_port
    a2a_port = base_port + 1

    # Load attack YAML to detect modes
    with open(attack_yaml) as f:
        attack_doc = yaml.safe_load(f)
    execution = attack_doc.get("attack", {}).get("execution", {})
    actors = execution.get("actors", [])
    modes = [a.get("mode", "") for a in actors]

    tj_cmd = [
        str(tj_binary), "run",
        "--config", str(attack_yaml),
        "--output", str(verdict_path),
        "--max-session", f"{timeout}s",
        "--no-semantic",
    ]

    if any(m == "mcp_server" for m in modes):
        tj_cmd.extend(["--mcp-server", f"127.0.0.1:{mcp_port}"])
    if any(m == "a2a_server" for m in modes):
        tj_cmd.extend(["--a2a-server", f"127.0.0.1:{a2a_port}"])
    # Wire client actors to their server counterparts
    if any(m == "mcp_client" for m in modes):
        tj_cmd.extend(["--mcp-client-endpoint", f"http://127.0.0.1:{mcp_port}/message"])
    if any(m == "a2a_client" for m in modes):
        tj_cmd.extend(["--a2a-client-endpoint", f"http://127.0.0.1:{a2a_port}"])

    print(f"RUN: {' '.join(tj_cmd)}")
    try:
        result = subprocess.run(tj_cmd, capture_output=True, text=True, timeout=timeout + 10)
    except subprocess.TimeoutExpired:
        print(f"FAIL: ThoughtJack timed out after {timeout + 10}s")
        return False

    if result.returncode not in (0, 1, 2, 3):
        print(f"FAIL: ThoughtJack exited with code {result.returncode}")
        if result.stderr:
            print(f"  stderr: {result.stderr[:500]}")
        return False

    if not verdict_path.exists():
        print(f"FAIL: No verdict file at {verdict_path}")
        return False

    return compare_verdict(verdict_path, expected_yaml)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def main() -> int:
    parser = argparse.ArgumentParser(
        description="ThoughtJack E2E conformance test orchestrator"
    )
    parser.add_argument(
        "--scenario",
        type=Path,
        required=True,
        help="Path to fixture directory",
    )
    parser.add_argument(
        "--framework",
        choices=["langgraph", "crewai"],
        help="Framework to test (omit for self-test)",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="Run as self-test (no agent, no mock-llm)",
    )
    parser.add_argument(
        "--tj-binary",
        type=Path,
        default=Path("./target/release/thoughtjack"),
        help="Path to ThoughtJack binary",
    )
    parser.add_argument(
        "--mock-llm-url",
        default="http://localhost:6556",
        help="Mock LLM base URL",
    )
    parser.add_argument(
        "--base-port",
        type=int,
        default=19000,
        help="Base port for services (MCP=+0, A2A=+1, agent=+2)",
    )
    parser.add_argument(
        "--timeout",
        type=int,
        default=30,
        help="Scenario timeout in seconds",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path("tests/e2e/results"),
        help="Directory for verdict output files",
    )
    args = parser.parse_args()

    args.output_dir.mkdir(parents=True, exist_ok=True)

    if not args.tj_binary.exists():
        print(f"ERROR: ThoughtJack binary not found: {args.tj_binary}")
        return 10

    if not args.scenario.is_dir():
        print(f"ERROR: Scenario directory not found: {args.scenario}")
        return 10

    if args.self_test:
        ok = run_self_test(
            args.scenario,
            args.tj_binary,
            args.base_port,
            args.timeout,
            args.output_dir,
        )
    elif args.framework:
        ok = run_framework_scenario(
            args.scenario,
            args.framework,
            args.tj_binary,
            args.mock_llm_url,
            args.base_port,
            args.timeout,
            args.output_dir,
        )
    else:
        print("ERROR: Specify --framework or --self-test")
        return 10

    if ok:
        print("PASS")
        return 0
    else:
        print("FAIL")
        return 1


if __name__ == "__main__":
    sys.exit(main())
