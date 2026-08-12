#!/usr/bin/env python3
"""Orchestrate ADNet's nightly real-network acceptance matrix over SSH.

The controller never installs firewall rules itself. Restricted-NAT/offline behavior is
provided by an operator-owned, self-restoring command in the inventory. This keeps SSH
control reachable and makes fault injection explicit and auditable.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shlex
import subprocess
import sys
import time
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET
from dataclasses import dataclass
from pathlib import Path
from typing import Any


@dataclass
class Case:
    name: str
    status: str
    seconds: float
    evidence: dict[str, Any]
    error: str | None = None


class AcceptanceError(RuntimeError):
    pass


def run(
    command: list[str],
    *,
    input_text: str | None = None,
    check: bool = True,
    timeout: int | None = None,
) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(
        command,
        input=input_text,
        # Audit V5 P1-3: pin UTF-8 decoding and *replace* (not
        # raise) on bytes that cannot be decoded. Without this,
        # a remote host that emits latin-1 / binary output from a
        # command such as `cat /var/log/syslog` triggers
        # UnicodeDecodeError and the whole acceptance matrix
        # aborts. `errors="replace"` substitutes the U+FFFD
        # replacement character so we keep a best-effort string
        # in the result for the operator to inspect.
        text=True,
        encoding="utf-8",
        errors="replace",
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
        check=False,
    )
    if check and result.returncode != 0:
        rendered = " ".join(shlex.quote(part) for part in command)
        raise AcceptanceError(
            f"command failed ({result.returncode}): {rendered}\n"
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return result


def ssh_command(node: dict[str, Any], script: str, *, timeout: int = 120, check: bool = True):
    command = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15"]
    command += ["-p", str(node.get("port", 22)), node["target"], "bash", "-s"]
    return run(command, input_text="set -euo pipefail\n" + script, check=check, timeout=timeout)


def scp(node: dict[str, Any], source: Path, destination: str) -> None:
    command = ["scp", "-q", "-P", str(node.get("port", 22)), str(source)]
    command.append(f"{node['target']}:{destination}")
    run(command, timeout=180)


def q(value: Any) -> str:
    return shlex.quote(str(value))


def validate_inventory(inventory: dict[str, Any]) -> None:
    if inventory.get("schema") != 1:
        raise AcceptanceError("inventory schema must be 1")
    relay = inventory.get("relay", {})
    nodes = inventory.get("nodes", {})
    for field in ("url", "health_url"):
        if not relay.get(field):
            raise AcceptanceError(f"inventory relay.{field} is required")
    relay_host = (urllib.parse.urlparse(relay["url"]).hostname or "").rstrip(".")
    if not relay_host:
        raise AcceptanceError("relay.url must contain a hostname")
    if relay_host.endswith("n0.iroh.link") or relay_host.endswith("iroh.link"):
        raise AcceptanceError(
            "relay.url points at public n0 infrastructure; nightly acceptance requires a self-hosted relay"
        )
    for role in ("server", "client"):
        node = nodes.get(role, {})
        for field in ("target", "data_dir"):
            if not node.get(field):
                raise AcceptanceError(f"inventory nodes.{role}.{field} is required")
    if not nodes["client"].get("command_prefix"):
        raise AcceptanceError(
            "inventory nodes.client.command_prefix is required so workload traffic runs in the restricted-NAT context"
        )
    if not nodes["client"].get("topology_check_command"):
        raise AcceptanceError(
            "inventory nodes.client.topology_check_command is required to capture restricted-NAT evidence"
        )
    server = nodes["server"]
    for field in ("bind", "advertise_direct"):
        if not server.get(field):
            raise AcceptanceError(f"inventory nodes.server.{field} is required")
    fault = inventory.get("fault", {})
    if not fault.get("offline_command"):
        raise AcceptanceError("inventory fault.offline_command is required")
    duration = int(fault.get("duration_seconds", 20))
    if duration < 10 or duration > 120:
        raise AcceptanceError("fault.duration_seconds must be between 10 and 120")


def prepare_relay(inventory: dict[str, Any]) -> None:
    relay = inventory["relay"]
    if relay.get("ssh") and relay.get("prepare_command"):
        ssh_command(relay["ssh"], relay["prepare_command"], timeout=180)
    request = urllib.request.Request(relay["health_url"], method="GET")
    with urllib.request.urlopen(request, timeout=15) as response:
        if response.status >= 400:
            raise AcceptanceError(f"relay health returned HTTP {response.status}")


def deploy(binary: Path, inventory: dict[str, Any]) -> tuple[str, str]:
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    destinations: list[str] = []
    for role in ("server", "client"):
        node = inventory["nodes"][role]
        remote_dir = node.get("binary_dir", "/tmp/adnet-network-acceptance")
        remote_binary = f"{remote_dir}/network_acceptance"
        ssh_command(node, f"install -d -m 700 {q(remote_dir)}")
        scp(node, binary, remote_binary + ".new")
        ssh_command(
            node,
            f"got=$(sha256sum {q(remote_binary + '.new')} | cut -d' ' -f1)\n"
            f"test \"$got\" = {q(digest)}\n"
            f"chmod 700 {q(remote_binary + '.new')}\n"
            f"mv -f {q(remote_binary + '.new')} {q(remote_binary)}",
        )
        destinations.append(remote_binary)
    return destinations[0], destinations[1]


def stop_server(node: dict[str, Any]) -> None:
    data_dir = node["data_dir"]
    pid_file = f"{data_dir}/acceptance-server.pid"
    ssh_command(
        node,
        f"if test -f {q(pid_file)}; then\n"
        f"  pid=$(cat {q(pid_file)})\n"
        "  if kill -0 \"$pid\" 2>/dev/null; then\n"
        "    kill -INT \"$pid\" || true\n"
        "    for _ in $(seq 1 50); do kill -0 \"$pid\" 2>/dev/null || break; sleep 0.2; done\n"
        "    kill -TERM \"$pid\" 2>/dev/null || true\n"
        "  fi\n"
        f"  rm -f {q(pid_file)}\n"
        "fi",
        check=False,
    )


def start_server(node: dict[str, Any], binary: str, relay_url: str) -> dict[str, Any]:
    data_dir = node["data_dir"]
    log_file = f"{data_dir}/acceptance-server.log"
    pid_file = f"{data_dir}/acceptance-server.pid"
    stop_server(node)
    command = (
        f"{q(binary)} serve --state-dir {q(data_dir)} --bind {q(node['bind'])} "
        f"--advertise-direct {q(node['advertise_direct'])} --relay-url {q(relay_url)}"
    )
    ssh_command(
        node,
        f"install -d -m 700 {q(data_dir)}\n"
        f": > {q(log_file)}\n"
        f"nohup env RUST_LOG=info {command} > {q(log_file)} 2>&1 &\n"
        f"echo $! > {q(pid_file)}",
    )

    deadline = time.monotonic() + 45
    last_log = ""
    while time.monotonic() < deadline:
        result = ssh_command(node, f"cat {q(log_file)}", check=False)
        last_log = result.stdout
        ready_lines = [line for line in last_log.splitlines() if line.startswith("ADNET_READY ")]
        if ready_lines:
            return json.loads(ready_lines[-1].removeprefix("ADNET_READY "))
        time.sleep(1)
    raise AcceptanceError(f"server did not become ready; log follows:\n{last_log}")


def write_server_info(node: dict[str, Any], info: dict[str, Any]) -> str:
    path = f"{node['data_dir']}/server-info.json"
    payload = json.dumps(info, separators=(",", ":"))
    ssh_command(node, f"umask 077\nprintf '%s' {q(payload)} > {q(path)}")
    return path


def parse_probe(stdout: str) -> dict[str, Any]:
    for line in reversed(stdout.splitlines()):
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict) and value.get("schema") == 1 and "checks" in value:
            return value
    raise AcceptanceError(f"probe emitted no report JSON:\n{stdout}")


def probe(
    node: dict[str, Any],
    binary: str,
    relay_url: str,
    server_info_path: str,
    *,
    path: str,
    checks: str,
    timeout_seconds: int = 30,
    expect_success: bool = True,
) -> dict[str, Any]:
    prefix = node.get("command_prefix", "").strip()
    command = (
        f"{prefix + ' ' if prefix else ''}"
        f"{q(binary)} probe --state-dir {q(node['data_dir'])} --relay-url {q(relay_url)} "
        f"--server-info-file {q(server_info_path)} --path {q(path)} --checks {q(checks)} "
        f"--timeout-seconds {q(timeout_seconds)}"
    )
    result = ssh_command(node, command, timeout=max(120, timeout_seconds * 8), check=False)
    try:
        report = parse_probe(result.stdout)
    except AcceptanceError:
        if not expect_success and result.returncode != 0:
            return {
                "passed": False,
                "negative_control": True,
                "exit_code": result.returncode,
                "stderr": result.stderr[-4000:],
            }
        raise
    actual_success = result.returncode == 0 and bool(report.get("passed"))
    if expect_success and not actual_success:
        raise AcceptanceError(
            f"probe expected success but failed: {json.dumps(report, ensure_ascii=False)}\n{result.stderr}"
        )
    if not expect_success and actual_success:
        raise AcceptanceError("outage negative control unexpectedly succeeded")
    return report


def execute_case(cases: list[Case], name: str, action) -> Any:
    started = time.monotonic()
    try:
        evidence = action()
    except Exception as error:
        cases.append(Case(name, "failed", time.monotonic() - started, {}, str(error)))
        raise
    cases.append(Case(name, "passed", time.monotonic() - started, evidence))
    return evidence


def write_reports(output_dir: Path, inventory: dict[str, Any], cases: list[Case]) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    summary = {
        "schema": 1,
        "generated_unix_ms": int(time.time() * 1000),
        "relay_url": inventory["relay"]["url"],
        "passed": all(case.status == "passed" for case in cases),
        "cases": [case.__dict__ for case in cases],
    }
    (output_dir / "summary.json").write_text(
        json.dumps(summary, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )
    with (output_dir / "results.jsonl").open("w", encoding="utf-8") as handle:
        for case in cases:
            handle.write(json.dumps(case.__dict__, ensure_ascii=False) + "\n")

    suite = ET.Element(
        "testsuite",
        name="adnet-real-network",
        tests=str(len(cases)),
        failures=str(sum(case.status != "passed" for case in cases)),
        time=f"{sum(case.seconds for case in cases):.3f}",
    )
    for case in cases:
        node = ET.SubElement(suite, "testcase", name=case.name, time=f"{case.seconds:.3f}")
        if case.error:
            failure = ET.SubElement(node, "failure", message=case.error)
            failure.text = case.error
        output = ET.SubElement(node, "system-out")
        output.text = json.dumps(case.evidence, ensure_ascii=False)
    ET.ElementTree(suite).write(output_dir / "junit.xml", encoding="utf-8", xml_declaration=True)


def orchestrate(binary: Path, inventory: dict[str, Any], output_dir: Path) -> None:
    validate_inventory(inventory)
    cases: list[Case] = []
    server_node = inventory["nodes"]["server"]
    client_node = inventory["nodes"]["client"]
    relay_url = inventory["relay"]["url"]
    server_binary = ""
    client_binary = ""

    try:
        execute_case(cases, "relay/self-hosted-health", lambda: (prepare_relay(inventory) or {"url": relay_url}))
        server_binary, client_binary = execute_case(
            cases,
            "deployment/two-public-nodes",
            lambda: deploy(binary, inventory),
        )
        execute_case(
            cases,
            "topology/restricted-nat",
            lambda: {
                "command": client_node["topology_check_command"],
                "output": ssh_command(
                    client_node,
                    client_node["topology_check_command"],
                    timeout=30,
                ).stdout,
            },
        )
        server_info = execute_case(
            cases,
            "server/initial-start",
            lambda: start_server(server_node, server_binary, relay_url),
        )
        server_info_path = write_server_info(client_node, server_info)

        execute_case(
            cases,
            "transport/direct",
            lambda: probe(
                client_node,
                client_binary,
                relay_url,
                server_info_path,
                path="direct",
                checks="frame",
            ),
        )
        execute_case(
            cases,
            "transport/relay-fallback-restricted-nat",
            lambda: probe(
                client_node,
                client_binary,
                relay_url,
                server_info_path,
                path="relay",
                checks="frame",
            ),
        )
        execute_case(
            cases,
            "transport/reconnect",
            lambda: probe(
                client_node,
                client_binary,
                relay_url,
                server_info_path,
                path="relay",
                checks="reconnect",
            ),
        )
        execute_case(
            cases,
            "protocols/blobs-gossip-docs",
            lambda: probe(
                client_node,
                client_binary,
                relay_url,
                server_info_path,
                path="relay",
                checks="blobs,gossip,docs",
                timeout_seconds=45,
            ),
        )

        def restart_and_probe():
            stop_server(server_node)
            time.sleep(2)
            restarted = start_server(server_node, server_binary, relay_url)
            if restarted.get("endpoint_id") != server_info.get("endpoint_id"):
                raise AcceptanceError(
                    "server endpoint identity changed across restart: "
                    f"{server_info.get('endpoint_id')} -> {restarted.get('endpoint_id')}"
                )
            restarted_path = write_server_info(client_node, restarted)
            result = probe(
                client_node,
                client_binary,
                relay_url,
                restarted_path,
                path="relay",
                checks="frame,reconnect,blobs,gossip,docs",
                timeout_seconds=45,
            )
            return {"server": restarted, "probe": result}

        restarted_evidence = execute_case(cases, "recovery/server-restart", restart_and_probe)
        server_info_path = write_server_info(client_node, restarted_evidence["server"])

        fault = inventory["fault"]
        duration = int(fault.get("duration_seconds", 20))
        settle = int(fault.get("settle_seconds", 3))

        def inject_and_prove_outage():
            command = (
                f"export ADNET_FAULT_SECONDS={q(duration)}\n"
                f"{fault['offline_command']}"
            )
            ssh_command(client_node, command, timeout=30)
            time.sleep(settle)
            return probe(
                client_node,
                client_binary,
                relay_url,
                server_info_path,
                path="relay",
                checks="frame",
                timeout_seconds=5,
                expect_success=False,
            )

        execute_case(cases, "recovery/outage-negative-control", inject_and_prove_outage)
        time.sleep(duration + int(fault.get("recovery_grace_seconds", 5)))
        execute_case(
            cases,
            "recovery/network-restored",
            lambda: probe(
                client_node,
                client_binary,
                relay_url,
                server_info_path,
                path="relay",
                checks="frame,reconnect,blobs,gossip,docs",
                timeout_seconds=45,
            ),
        )
    finally:
        if server_binary:
            stop_server(server_node)
        write_reports(output_dir, inventory, cases)

    if any(case.status != "passed" for case in cases):
        raise AcceptanceError("real-network acceptance matrix failed")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--inventory", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()

    try:
        inventory = json.loads(args.inventory.read_text(encoding="utf-8"))
        orchestrate(args.binary.resolve(), inventory, args.output_dir.resolve())
    except Exception as error:
        print(f"real-network acceptance failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
