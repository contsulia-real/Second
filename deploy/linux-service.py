#!/usr/bin/env python3
"""Install or manage one existing Second node as a system systemd service."""
import argparse
import os
from pathlib import Path
import pwd
import re
import subprocess
import sys

MARKER = "# Managed by Second linux-service.py\n"


def quote(value):
    if any(c in str(value) for c in '\r\n\0$"\\'):
        raise ValueError("Control characters, dollar signs, quotes and backslashes are not allowed in unit arguments")
    return '"' + str(value).replace('\\', '\\\\').replace('"', '\\"').replace('%', '%%').replace('$', '$$') + '"'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["install", "start", "stop", "restart", "status", "remove", "render"])
    parser.add_argument("--name", required=True)
    parser.add_argument("--second-exe", type=Path)
    parser.add_argument("--snapshot-base", type=Path)
    parser.add_argument("--listen-address")
    parser.add_argument("--user-account")
    args = parser.parse_args()
    if sys.platform != "linux" or not re.fullmatch(r"second-[A-Za-z0-9_-]{1,48}", args.name):
        raise ValueError("Use Linux and a service name second- followed by 1..48 letters/digits/_/-")
    unit = Path("/etc/systemd/system") / (args.name + ".service")
    if args.action in ("install", "render"):
        if not all([args.second_exe, args.snapshot_base, args.listen_address, args.user_account]):
            parser.error("install/render requires binary, base, address and existing non-root user-account")
        account = pwd.getpwnam(args.user_account)
        if account.pw_uid == 0 or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_-]*", args.user_account):
            raise ValueError("An existing non-root service user is required")
        executable = args.second_exe.resolve(strict=True)
        base = args.snapshot_base.absolute()
        directory = base.parent.resolve(strict=True)
        base = directory / base.name
        if directory == Path("/") or executable.is_relative_to(directory):
            raise ValueError("Keep the program outside the writable node directory; / is not a data boundary")
        metadata = directory.stat()
        if metadata.st_uid != account.pw_uid or metadata.st_mode & 0o077:
            raise ValueError("Node directory must belong to the service user and be private (0700)")
        if args.action == "install" and unit.exists():
            raise FileExistsError("Refusing to replace an existing systemd unit")
        if not args.listen_address or any(c in args.listen_address for c in '\r\n\0'):
            raise ValueError("Invalid listen address")
        check = [str(executable), "node-check", args.listen_address, str(base)]
        if os.geteuid() == 0:
            check = ["runuser", "-u", args.user_account, "--", *check]
        elif os.getuid() != account.pw_uid:
            raise ValueError("render must run as root or the selected service user")
        subprocess.run(check, check=True, stdout=sys.stderr, timeout=30)
        text = MARKER + f"""[Unit]
Description=Second node {args.name}
Wants=network-online.target
After=network-online.target
StartLimitIntervalSec=60
StartLimitBurst=5

[Service]
Type=exec
User={args.user_account}
UMask=0077
WorkingDirectory={str(directory).replace('%', '%%')}
ExecStart={quote(executable)} node {quote(args.listen_address)} {quote(base)}
Restart=on-failure
RestartSec=5
TimeoutStopSec=30
KillSignal=SIGTERM
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths={quote(directory)}
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
"""
        if args.action == "render":
            print(text, end="")
            return
        if os.geteuid() != 0:
            raise PermissionError("Installation requires root")
        created = False
        try:
            with unit.open("x", encoding="utf-8") as out:
                created = True
                out.write(text)
            unit.chmod(0o644)
            subprocess.run(["systemctl", "daemon-reload"], check=True)
            subprocess.run(["systemctl", "enable", "--now", unit.name], check=True)
        except BaseException:
            if created:
                subprocess.run(["systemctl", "disable", "--now", unit.name], check=False)
                unit.unlink(missing_ok=True)
                subprocess.run(["systemctl", "daemon-reload"], check=False)
                subprocess.run(["systemctl", "reset-failed", unit.name], check=False, capture_output=True)
            raise
    else:
        if not unit.is_file() or unit.is_symlink() or not unit.read_text().startswith(MARKER):
            raise ValueError("Refusing to manage a unit not installed by this tool")
        if args.action == "remove":
            if os.geteuid() != 0:
                raise PermissionError("Removal requires root")
            subprocess.run(["systemctl", "disable", "--now", unit.name], check=True)
            unit.unlink()
            subprocess.run(["systemctl", "daemon-reload"], check=True)
            subprocess.run(["systemctl", "reset-failed", unit.name], check=False, capture_output=True)
        else:
            subprocess.run(["systemctl", args.action, unit.name], check=True)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        print(f"ERROR {error}", file=sys.stderr)
        sys.exit(1)
