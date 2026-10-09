"""Owned Linux children for the explicitly invoked Windows/WSL integration test."""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

os.umask(0o077)
root = Path(tempfile.mkdtemp(prefix="second-mixed-", dir=Path.home()))
binary = sys.argv[1]
children = {}


def stop(identity):
    child = children.pop(identity, None)
    if child is not None:
        if child.poll() is None:
            child.terminate()
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()


def request(item):
    action = item["action"]
    if action == "info":
        return str(root)
    if action == "import":
        for identity in [3, 4]:
            directory = f"validator-{identity}"
            shutil.copytree(Path(item["source"]) / directory, root / "network" / directory)
        for path in (root / "network").rglob("*"):
            path.chmod(0o700 if path.is_dir() else 0o600)
        return str(root / "network")
    if action == "copy":
        destination = Path(item["destination"])
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(item["source"], destination)
        destination.chmod(0o600)
        return True
    if action == "cli":
        result = subprocess.run([binary, *item["args"]], capture_output=True,
                                text=True, timeout=35)
        return {"code": result.returncode, "stdout": result.stdout, "stderr": result.stderr}
    if action == "snapshot":
        probe = Path(binary).parent / "examples" / "mixed_snapshot_probe"
        result = subprocess.run([str(probe), item["base"]], capture_output=True, text=True, timeout=15)
        if result.returncode:
            raise RuntimeError(result.stderr)
        return json.loads(result.stdout)
    if action == "metrics":
        result = {}
        for identity, child in children.items():
            if child.poll() is not None:
                raise RuntimeError(f"Owned node {identity} exited unexpectedly")
            fields = Path(f"/proc/{child.pid}/stat").read_text().rsplit(")", 1)[1].split()
            result[str(identity)] = {"pid": child.pid,
                "cpu_seconds": (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK"),
                "rss_bytes": int(fields[21]) * os.sysconf("SC_PAGE_SIZE"),
                "threads": int(fields[17])}
        return result
    if action == "finish":
        for identity in list(children):
            stop(identity)
        shutil.rmtree(root)
        return True
    if action == "start":
        identity = item["id"]
        if identity in children:
            raise ValueError("Child is already running")
        run = time.monotonic_ns()
        stdout = root / f"node-{identity}-{run}.stdout.log"
        stderr = root / f"node-{identity}-{run}.stderr.log"
        with stdout.open("x") as out, stderr.open("x") as err:
            child = subprocess.Popen([binary, "node", item["address"], item["base"]],
                                     stdout=out, stderr=err)
        children[identity] = child
        deadline = time.monotonic() + 30
        while child.poll() is None and time.monotonic() < deadline:
            with stdout.open() as out:
                line = out.readline(8192)
            if line.endswith("\n"):
                return line.strip()
            time.sleep(0.1)
        raise RuntimeError(f"Linux node did not start: {stderr.read_text()}")
    if action == "stop":
        stop(item["id"])
        return True
    if action == "close":
        return True
    raise ValueError(f"Unknown action: {action}")


try:
    for line in sys.stdin:
        item = {}
        try:
            item = json.loads(line)
            result = {"result": request(item)}
        except Exception as error:
            result = {"error": str(error)}
        print(json.dumps(result), flush=True)
        if item.get("action") == "close":
            break
finally:
    for identity in list(children):
        stop(identity)
