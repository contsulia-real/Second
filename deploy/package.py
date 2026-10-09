#!/usr/bin/env python3
"""Build a host-native, key-free Second deployment archive using the standard library."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
import zipfile


def main():
    repo = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-directory", type=Path, default=repo / "target/dist")
    parser.add_argument("--binary", type=Path, help="Use an already built native binary; skip cargo build")
    args = parser.parse_args()
    host = "windows" if os.name == "nt" else "linux" if platform.system() == "Linux" else None
    if host is None:
        raise ValueError("Only native Windows and Linux packages are supported")
    version = tomllib.loads((repo / "Cargo.toml").read_text())["package"]["version"]
    if not args.binary:
        subprocess.run(["cargo", "build", "--release", "--locked", "--bin", "second"], cwd=repo, check=True)
    binary = (args.binary or repo / "target/release" / ("second.exe" if host == "windows" else "second")).resolve(strict=True)
    identity = subprocess.check_output([str(binary), "--version"], text=True).strip().split()
    if len(identity) != 4 or identity[:3] != ["Second", version, host]:
        raise ValueError("Binary version/platform does not match this source and host")
    name = f"second-{version}-{host}-{identity[3]}"
    output = args.output_directory.resolve()
    output.mkdir(parents=True, exist_ok=True)
    archive = output / (name + (".zip" if host == "windows" else ".tar.gz"))
    if archive.exists():
        raise FileExistsError(f"Refusing to replace {archive}")
    with tempfile.TemporaryDirectory(prefix="second-package-") as temporary:
        if Path(temporary).resolve().parent != Path(tempfile.gettempdir()).resolve():
            raise ValueError("Unexpected temporary package directory")
        root = Path(temporary) / name
        (root / "bin").mkdir(parents=True)
        shutil.copy2(binary, root / "bin" / ("second.exe" if host == "windows" else "second"))
        files = ["docs/node-operations.md", "docs/business-integration.md", "docs/deployment.md", "deploy/network.example.json", "deploy/service-smoke.py"]
        files += ["deploy/windows-service.ps1", "examples/run-network.ps1", "examples/submit-transaction.ps1"] if host == "windows" else ["deploy/linux-service.py", "examples/run-network.py"]
        for relative in files:
            target = root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(repo / relative, target)
        # Do not inherit /mnt/c's permissive mode bits into a Linux archive.
        root.chmod(0o755)
        for path in root.rglob("*"):
            path.chmod(0o755 if path.is_dir() or path.parent == root / "bin" else 0o644)
        checksums = {p.relative_to(root).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest()
                     for p in sorted(root.rglob("*")) if p.is_file()}
        (root / "manifest.json").write_text(json.dumps({"version": version, "platform": host,
            "architecture": identity[3], "sha256": checksums}, indent=2) + "\n", encoding="utf-8")
        (root / "manifest.json").chmod(0o644)
        # Exclusive output creation also closes the race with another packager.
        with archive.open("xb") as stream:
            if host == "windows":
                with zipfile.ZipFile(stream, "w", zipfile.ZIP_DEFLATED) as bundle:
                    for p in root.rglob("*"):
                        if p.is_file():
                            bundle.write(p, p.relative_to(root.parent))
            else:
                with tarfile.open(fileobj=stream, mode="w:gz") as bundle:
                    bundle.add(root, arcname=name)
    print(archive)


if __name__ == "__main__":
    main()
