#!/usr/bin/env python3
"""Foreground Linux supervisor for an already initialized Second deployment."""

import argparse
import json
import signal
import subprocess
import sys
import time
import uuid
from pathlib import Path


def validator_id(value):
    number = int(value)
    if not 0 <= number <= 2**64 - 1:
        raise ValueError("ValidatorId must fit u64")
    return number


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--second-exe", required=True, type=Path)
    parser.add_argument("--config-file", required=True, type=Path)
    parser.add_argument("--deployment-directory", required=True, type=Path)
    parser.add_argument("--validator-ids", nargs="+", type=validator_id)
    parser.add_argument("--run-seconds", type=int, default=0)
    args = parser.parse_args()
    if not sys.platform.startswith("linux"):
        parser.error("Use run-network.ps1 on Windows")
    if not 0 <= args.run_seconds <= 86400:
        parser.error("run-seconds must be between 0 and 86400")
    executable = args.second_exe.resolve(strict=True)
    deployment = args.deployment_directory.resolve(strict=True)
    config = json.loads(args.config_file.read_text(encoding="utf-8"))
    selected_ids = set(args.validator_ids or [])
    selected = [v for v in config["validators"]
                if not selected_ids or v["validator_id"] in selected_ids]
    if not selected or (selected_ids and selected_ids != {v["validator_id"] for v in selected}):
        raise ValueError("Selected ValidatorId is missing from config")
    children = []
    endpoints = []
    run = uuid.uuid4().hex
    stopping = False

    def stop(_signal, _frame):
        nonlocal stopping
        stopping = True

    signal.signal(signal.SIGINT, stop)
    signal.signal(signal.SIGTERM, stop)
    try:
        for validator in selected:
            if stopping:
                return
            identity = validator_id(validator["validator_id"])
            base = deployment / f"validator-{identity}" / "second"
            stdout = Path(f"{base}.operator-{run}.stdout.log")
            stderr = Path(f"{base}.operator-{run}.stderr.log")
            # Keep exact argv boundaries, including paths containing spaces.
            with stdout.open("x") as out, stderr.open("x") as err:
                child = subprocess.Popen(
                    [str(executable), "node", validator["listen_address"], str(base)],
                    stdout=out, stderr=err, start_new_session=True,
                )
            children.append(child)
            deadline = time.monotonic() + 30
            line = ""
            while not line:
                if stopping:
                    return
                if child.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError(f"Validator {identity} failed to start; inspect {stderr}")
                with stdout.open(encoding="utf-8") as out:
                    candidate = out.readline(8192)
                if candidate.endswith("\n"):
                    line = candidate.strip()
                else:
                    time.sleep(0.1)
            fields = line.split()
            if (len(fields) != 8 or fields[0] != "LISTENING"
                    or fields[1] != validator["listen_address"] or fields[2] != "NODE"
                    or fields[4] != "CERT" or fields[6] != "VALIDATOR"
                    or fields[7] != str(identity)):
                raise RuntimeError(f"Validator {identity} startup identity/endpoint mismatch: {line}")
            endpoints.append({"address": fields[1], "certificate_base64": fields[5]})
            print(line, flush=True)
        inventory = deployment / f"operator-{run}.endpoints.json"
        with inventory.open("x", encoding="utf-8") as out:
            json.dump(endpoints, out, indent=2)
        print(f"ENDPOINTS {inventory}", flush=True)
        started = time.monotonic()
        while not stopping and (args.run_seconds == 0 or time.monotonic() - started < args.run_seconds):
            if any(child.poll() is not None for child in children):
                raise RuntimeError("A managed node exited; inspect this run's stderr logs")
            time.sleep(1)
    finally:
        # Only this invocation's child objects; never kill by name or PID file.
        for child in children:
            if child.poll() is None:
                child.terminate()
        for child in children:
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, RuntimeError) as error:
        print(f"ERROR {error}", file=sys.stderr)
        sys.exit(1)
