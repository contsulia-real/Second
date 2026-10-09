#!/usr/bin/env python3
"""Explicit local OS-service acceptance; creates only its own temporary node/service."""
import argparse
import base64
import ctypes
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import time
import uuid


def command(argv, check=True):
    result = subprocess.run(list(map(str, argv)), text=True, capture_output=True, timeout=45)
    if check and result.returncode != 0:
        raise RuntimeError(f"{argv[0]} exited {result.returncode}: {result.stderr[-8192:]}")
    return result


def wait(label, predicate):
    deadline = time.monotonic() + 45
    while not predicate():
        if time.monotonic() >= deadline:
            raise TimeoutError(label)
        time.sleep(0.2)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--second-exe", required=True, type=Path)
    parser.add_argument("--user-account", help="Linux existing non-root service user")
    parser.add_argument("--plan-only", action="store_true", help="Windows non-admin fixture/preflight/SCM-plan validation only")
    args = parser.parse_args()
    windows = os.name == "nt"
    if windows:
        if not args.plan_only and not ctypes.windll.shell32.IsUserAnAdmin():
            raise PermissionError("SCM smoke requires Administrator; no test service/data created")
    elif os.geteuid() != 0 or not args.user_account:
        raise PermissionError("Linux smoke requires root and --user-account; no service/data created")
    exe = args.second_exe.resolve(strict=True)
    deploy = Path(__file__).resolve().parent
    temporary_parent = None
    if not windows:
        import pwd
        temporary_parent = pwd.getpwnam(args.user_account).pw_dir
    root = Path(tempfile.mkdtemp(prefix="second-service smoke-", dir=temporary_parent))
    owned_root = root.resolve(strict=True)
    name = ("Second-" if windows else "second-") + "smoke-" + uuid.uuid4().hex
    installed = False
    passed = False
    print(f"SERVICE-ARTIFACTS {root}", flush=True)
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as reserve:
            reserve.bind(("127.0.0.1", 0))
            address = f"127.0.0.1:{reserve.getsockname()[1]}"
        public = command([exe, "authorizer-keygen", root / "authorizer.json"]).stdout.strip().split("public_key=")[1]
        command([exe, "validator-keygen", "1", root / "validator.keys.json"])
        config = {"validator_set_version": 1, "first_currency_address": 1,
            "reserve_count": 0, "accounts": [], "authorizer_public_keys_base64": [public],
            "bft_timeouts_ms": {"proposal": 1000, "prevote": 1000, "precommit": 1000},
            "validators": [{"validator_id": 1, "listen_address": address, "keyring_file": "validator.keys.json"}]}
        (root / "network.json").write_text(json.dumps(config))
        command([exe, "init-network", root / "network.json", root / "data"])
        base = root / "data/validator-1/second"
        checked = command([exe, "node-check", address, base]).stdout.split()
        certificate = checked[6]
        assert checked[0] == "CHECKED"
        if not windows:
            import pwd
            account = pwd.getpwnam(args.user_account)
            assert account.pw_uid != 0
            for path in [root, *root.rglob("*")]:
                os.chown(path, account.pw_uid, account.pw_gid)
                if path.is_dir():
                    path.chmod(0o700)

        def manage(action, check=True):
            if windows:
                argv = ["pwsh", "-NoProfile", "-File", deploy / "windows-service.ps1", "-Action", action, "-Name", name]
                if action.lower() in ("install", "plan"):
                    argv += ["-SecondExe", exe, "-SnapshotBase", base, "-ListenAddress", address]
            else:
                argv = ["python3", deploy / "linux-service.py", action.lower(), "--name", name]
                if action.lower() == "install":
                    argv += ["--second-exe", exe, "--snapshot-base", base, "--listen-address", address, "--user-account", args.user_account]
            return command(argv, check)

        if args.plan_only:
            if not windows:
                raise ValueError("--plan-only is the Windows non-admin check; use render on Linux")
            plan = manage("Plan").stdout
            assert f'"{base}"' in plan and f'"{exe}" service {name}' in plan
            assert command([exe, "service", name, address, base, root / "logs"], False).returncode != 0
            passed = True
            print("SERVICE-PLAN-PASS real node preflight; paths with spaces; SCM-only entry rejects console; no service installed", flush=True)
            return

        def ping():
            return command([exe, "ping", address, "71", certificate], False).returncode == 0

        def process_id():
            if windows:
                return command(["pwsh", "-NoProfile", "-Command", f"(Get-CimInstance Win32_Service -Filter \"Name='{name}'\").ProcessId"]).stdout.strip()
            return command(["systemctl", "show", "--property=MainPID", "--value", name + ".service"]).stdout.strip()

        manage("Install")
        installed = True
        wait("installed service pinned QUIC", ping)
        assert manage("Install", False).returncode != 0, "must refuse replacing an existing service"
        request = root / "unsigned.json"
        account_address = "acct_" + base64.urlsafe_b64encode(bytes([1]) * 32).decode().rstrip("=")
        request.write_text(json.dumps({"request_id": "service-smoke-" + uuid.uuid4().hex, "version": 1,
            "expires_at": None, "operations": [{"type": "register_account", "account": account_address},
            {"type": "issue", "recipient": account_address, "amount": 1}]}))
        signed = root / "signed.json"
        command([exe, "transaction-sign", root / "authorizer.json", request, signed])
        command([exe, "submit", address, signed, public, certificate])
        wait("service business finalized", lambda: "state=succeeded" in command([exe, "task-status", address, signed, public, certificate]).stdout)
        manage("Stop")
        before = command([exe, "snapshot-status", base]).stdout
        assert "supply=1 " in before
        command([exe, "node-check", address, base])
        manage("Start")
        wait("service restart identity", ping)
        assert "state=succeeded" in command([exe, "task-status", address, signed, public, certificate]).stdout
        old_process = process_id()
        if windows:
            # Stop only the SCM-owned PID of the just-created, verified service.
            escaped_exe = str(exe).replace("'", "''")
            command(["pwsh", "-NoProfile", "-Command", f"$s=Get-CimInstance Win32_Service -Filter \"Name='{name}'\"; if($s.Description -ne 'Managed by Second windows-service.ps1' -or $s.ProcessId -eq 0){{throw 'ownership'}}; $p=Get-Process -Id $s.ProcessId; $null=$p.Handle; if($p.Path -ne '{escaped_exe}'){{throw 'image'}}; $p.Kill(); $p.WaitForExit(); $p.Dispose()"])
        else:
            command(["systemctl", "kill", "--kill-whom=main", "--signal=SIGKILL", name + ".service"])
        # Enforce a new process rather than accepting a ping still in flight.
        time.sleep(1)
        wait("abnormal exit automatically restarts", lambda: process_id() not in ("0", old_process) and ping())
        assert "state=succeeded" in command([exe, "task-status", address, signed, public, certificate]).stdout
        manage("Remove")
        installed = False
        after = command([exe, "snapshot-status", base]).stdout
        def summary(value):
            return [part for part in value.split() if not part.startswith("generation=")]
        assert summary(after) == summary(before), "service control must preserve business and safety state"
        command([exe, "node-check", address, base])
        passed = True
        print("SERVICE-PASS install; signed business; stop/start; abnormal restart; unchanged identity/state; remove preserves data", flush=True)
    finally:
        if installed:
            manage("Remove", False)
        if passed:
            if root.is_symlink() or root.resolve(strict=True) != owned_root:
                raise RuntimeError("Temporary directory identity changed; refusing recursive cleanup")
            shutil.rmtree(owned_root)
        else:
            print(f"Private failed diagnostics retained at {root}", flush=True)


if __name__ == "__main__":
    main()
