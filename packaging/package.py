#!/usr/bin/env python3
"""Build and sign a source-independent multi-binary release using an isolated source copy."""
from __future__ import annotations

import argparse
import base64
import hashlib
import gzip
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib

from third_party import collect

PRODUCT = "tokemula-image-connector"
BINARIES = ("tokemula-image", "tokemula-image-mcp")
VERSION = re.compile(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?\Z")
TARGET = re.compile(r"[a-z0-9_]+(?:-[a-z0-9_]+){2,}\Z")
ED25519_SPKI_PREFIX = bytes.fromhex("302a300506032b6570032100")


def run(command: list[str], *, cwd: Path | None = None, data: bytes | None = None,
        env: dict[str, str] | None = None) -> bytes:
    return subprocess.run(command, cwd=cwd, input=data, env=env, check=True,
                          stdout=subprocess.PIPE).stdout


def build_environment() -> dict[str, str]:
    home = Path.home()
    env = {
        "HOME": str(home),
        "PATH": os.pathsep.join([str(home / ".cargo/bin"), os.environ.get("PATH", os.defpath)]),
        "CARGO_NET_OFFLINE": "true",
    }
    for name in ("CARGO_HOME", "RUSTUP_HOME", "TMPDIR", "SDKROOT", "MACOSX_DEPLOYMENT_TARGET", "SystemRoot", "WINDIR", "TEMP", "TMP", "LIB", "LIBPATH", "INCLUDE"):
        if name in os.environ:
            env[name] = os.environ[name]
    return env


def openssl_path(value: str | None = None) -> str:
    result = value or shutil.which("openssl")
    if not result:
        raise ValueError("OpenSSL is needed only on the packaging machine")
    return result


def generate_test_key(path: Path, openssl: str) -> None:
    if path.exists():
        raise ValueError("refusing to replace an existing signing key")
    path.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
    if os.name != "nt" and path.parent.stat().st_mode & 0o077:
        raise ValueError("choose a private 0700 signing-key directory")
    run([openssl, "genpkey", "-algorithm", "ED25519", "-out", str(path)])
    os.chmod(path, 0o600)


def public_key(path: Path, openssl: str) -> str:
    der = run([openssl, "pkey", "-in", str(path), "-pubout", "-outform", "DER"])
    if not der.startswith(ED25519_SPKI_PREFIX) or len(der) != len(ED25519_SPKI_PREFIX) + 32:
        raise ValueError("signing key must be Ed25519")
    return base64.b64encode(der[len(ED25519_SPKI_PREFIX):]).decode("ascii")


def sign(data: bytes, key: Path, openssl: str) -> str:
    # OpenSSL's Ed25519 one-shot operation needs a sized regular input, not a pipe.
    with tempfile.NamedTemporaryFile(prefix="tokemula-sign-") as source:
        source.write(data)
        source.flush()
        signature = run([openssl, "pkeyutl", "-sign", "-rawin", "-inkey", str(key),
                         "-in", source.name])
    if len(signature) != 64:
        raise ValueError("invalid Ed25519 signature size")
    return base64.b64encode(signature).decode("ascii")


def json_bytes(value: object) -> bytes:
    return (json.dumps(value, ensure_ascii=False, indent=2) + "\n").encode("utf8")


def file_entry(path: Path, root: Path) -> dict[str, object]:
    data = path.read_bytes()
    return {"path": path.relative_to(root).as_posix(), "sha256": hashlib.sha256(data).hexdigest(),
            "size": len(data)}


def archive_name(version: str, target: str) -> str:
    if target.endswith("apple-darwin"):
        system = "macos"
    elif "windows" in target:
        system = "windows"
    elif "linux" in target:
        system = "linux"
    else:
        raise ValueError("only macOS, Windows and Linux targets are planned")
    arch = {"aarch64": "arm64", "x86_64": "x64"}.get(target.split("-")[0])
    if arch is None:
        raise ValueError("only arm64 and x64 targets are planned")
    return f"{PRODUCT}-{version}-{system}-{arch}.tar.gz"


def write_archive(stage: Path, destination: Path) -> None:
    if destination.exists():
        raise ValueError(f"refusing to overwrite release artifact: {destination}")
    with destination.open("xb") as raw, gzip.GzipFile(fileobj=raw, mode="wb", filename="", mtime=0) as compressed, tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as archive:
        for path in sorted(stage.rglob("*")):
            if not path.is_file() or path.is_symlink():
                if path.is_dir() and not path.is_symlink():
                    continue
                raise ValueError("release payload must contain only ordinary files")
            data = path.read_bytes()
            info = tarfile.TarInfo(path.relative_to(stage).as_posix())
            info.size = len(data)
            info.mode = 0o755 if path.name in BINARIES or path.name in [n + ".exe" for n in BINARIES] else 0o644
            info.mtime = 0
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            archive.addfile(info, io.BytesIO(data))


def source_files(root: Path) -> dict[Path, bytes]:
    return {path.relative_to(root): path.read_bytes() for path in root.rglob("*")
            if path.is_file() and not any(part in {".git", "target", "__pycache__", ".DS_Store"}
                                          or part == ".env" or part.startswith(".env.")
                                          for part in path.relative_to(root).parts)}


def release_inputs(files: dict[Path, bytes]) -> dict[Path, bytes]:
    exact = {"Cargo.toml", "Cargo.lock", "LICENSE", "rust-toolchain.toml", "connector/Cargo.toml",
             "connector/build.rs", "packaging/package.py", "packaging/third_party.py", "packaging/QUICKSTART.md"}
    prefixes = ("connector/src/", "connector/assets/", "integrations/", "plugins/", "skills/")
    return {path: data for path, data in files.items()
            if path.as_posix() in exact or path.as_posix().startswith(prefixes)}


def build_release(source: Path, version: str, target: str, output: Path,
                  key: Path, cargo: str, openssl: str, target_dir: Path | None = None) -> dict:
    if not VERSION.fullmatch(version) or not TARGET.fullmatch(target):
        raise ValueError("invalid release version or Rust target")
    name = archive_name(version, target)
    if output.resolve().is_relative_to(source.resolve()) or key.resolve().is_relative_to(source.resolve()):
        raise ValueError("release artifacts and signing material must stay outside the source repository")
    output.mkdir(parents=True, exist_ok=True)
    if (output / name).exists():
        raise ValueError("choose a new candidate directory; releases are not overwritten")
    env = build_environment()
    with tempfile.TemporaryDirectory(prefix="tokemula-package-") as temporary:
        temporary = Path(temporary)
        copied = temporary / "source"
        shutil.copytree(source, copied, ignore=shutil.ignore_patterns(".git", "target", "__pycache__", ".DS_Store", ".env", ".env.*"))
        original_source_files = source_files(copied)
        manifest = copied / "connector/Cargo.toml"
        text = manifest.read_text()
        text, count = re.subn(r'(?m)^version\s*=\s*"[^"]+"', f'version = "{version}"', text, count=1)
        if count != 1:
            raise ValueError("connector package version was not found")
        manifest.write_text(text)
        skill = copied / "skills/tokemula-image/SKILL.md"
        skill_text = skill.read_text()
        if not skill_text.startswith("---\n"):
            raise ValueError("the shared Skill must have frontmatter")
        end = skill_text.index("\n---", 4)
        if "\nmetadata:" in skill_text[:end]:
            raise ValueError("the internal build marker needs an explicit existing-metadata merge")
        skill.write_text(skill_text[:end] + f'\nmetadata:\n  internal_build: "{version}"' + skill_text[end:])
        # One shared Skill source, with a non-business marker so A/B actually exercise managed updates.
        if skill.read_text().split("\n---", 1)[1] != skill_text.split("\n---", 1)[1]:
            raise ValueError("the internal marker changed Skill business content")
        build = target_dir or temporary / "target"
        env["CARGO_TARGET_DIR"] = str(build)
        # Cargo must refresh only the local package's version in its copied lockfile.
        run([cargo, "build", "--release", "--offline", "--target", target,
             "--bin", BINARIES[0], "--bin", BINARIES[1]], cwd=copied, env=env)
        signature = lambda entry: (entry["name"], entry["version"], entry.get("source"))
        original = {signature(entry) for entry in tomllib.loads((source / "Cargo.lock").read_text())["package"] if entry.get("source")}
        copied_lock = {signature(entry) for entry in tomllib.loads((copied / "Cargo.lock").read_text())["package"] if entry.get("source")}
        if original != copied_lock:
            raise ValueError("release build changed a third-party locked dependency")
        stage = temporary / "payload"
        stage.mkdir()
        suffix = ".exe" if "windows" in target else ""
        versions = {}
        for binary in BINARIES:
            destination = stage / (binary + suffix)
            shutil.copy2(build / target / "release" / (binary + suffix), destination)
            info = json.loads(run([str(destination), "--build-info"], env={**env, "TOKEMULA_IMAGE_KEY": ""}))
            if (info.get("product") != PRODUCT or info.get("binary") != binary
                    or info.get("version") != version or info.get("target") != target):
                raise ValueError(f"binary version/target mismatch: {binary}")
            versions[binary] = info
        shutil.copy2(copied / "LICENSE", stage / "LICENSE")
        shutil.copy2(copied / "packaging/QUICKSTART.md", stage / "QUICKSTART.md")
        license_summary = collect(copied, stage / "THIRD-PARTY", target)
        release = {"schema_version": 1, "product": PRODUCT, "version": version,
                   "target": target,
                   "files": [file_entry(p, stage) for p in sorted(stage.rglob("*")) if p.is_file()]}
        encoded = json_bytes(release)
        (stage / "release.json").write_bytes(encoded)
        (stage / "release.sig").write_text(sign(encoded, key, openssl) + "\n")
        destination = output / name
        write_archive(stage, destination)
        digest = hashlib.sha256(destination.read_bytes()).hexdigest()
        (output / (name + ".sha256")).write_text(f"{digest}  {name}\n")
        result = {"archive": str(destination), "version": version, "target": target,
                  "size": destination.stat().st_size, "sha256": digest,
                  "public_key": public_key(key, openssl), "binary_build_info": versions,
                  "payload_files": len(release["files"]),
                  "build_source_file_count": len(original_source_files),
                  "source_unchanged_through_build": original_source_files == source_files(source),
                  "release_inputs_unchanged_through_build": release_inputs(original_source_files) == release_inputs(source_files(source)),
                  "source_variations": [
                      {"path": "connector/Cargo.toml", "field": "package.version", "value": version},
                      {"path": "skills/tokemula-image/SKILL.md", "field": "metadata.internal_build", "value": version},
                  ],
                  "third_party": {"package_count": license_summary["package_count"],
                                  "notice": "THIRD-PARTY/NOTICE.md"}}
        (output / (name + ".metadata.json")).write_bytes(json_bytes(result))
        return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="action", required=True)
    keygen = commands.add_parser("test-key", help="create an isolated test signing key, never a product trust root")
    keygen.add_argument("--private-key", type=Path, required=True)
    keygen.add_argument("--public-key", type=Path, required=True)
    keygen.add_argument("--openssl")
    build = commands.add_parser("build", help="build, verify and sign both programs as one release")
    build.add_argument("--version", required=True)
    build.add_argument("--target", help="Rust target; defaults to this packaging machine host")
    build.add_argument("--output-dir", type=Path, required=True)
    build.add_argument("--signing-key", type=Path, required=True)
    build.add_argument("--cargo", default=str(Path.home() / ".cargo/bin/cargo"))
    build.add_argument("--openssl")
    build.add_argument("--target-dir", type=Path)
    signer = commands.add_parser("sign", help="sign exact update-manifest bytes for an isolated or authorized release source")
    signer.add_argument("--input", type=Path, required=True)
    signer.add_argument("--output", type=Path, required=True)
    signer.add_argument("--signing-key", type=Path, required=True)
    signer.add_argument("--openssl")
    args = parser.parse_args()
    openssl = openssl_path(args.openssl)
    if args.action == "test-key":
        generate_test_key(args.private_key, openssl)
        if args.public_key.exists():
            raise ValueError("refusing to replace an existing public key file")
        args.public_key.parent.mkdir(parents=True, exist_ok=True)
        args.public_key.write_text(public_key(args.private_key, openssl) + "\n")
        print("Isolated test key created; it is not a product trust root.")
    elif args.action == "sign":
        if args.output.exists():
            raise ValueError("refusing to overwrite an existing signed manifest")
        payload = args.input.read_bytes()
        args.output.write_bytes(json_bytes({"payload": base64.b64encode(payload).decode("ascii"),
                                           "signature": sign(payload, args.signing_key, openssl)}))
    else:
        source = Path(__file__).resolve().parents[1]
        target = args.target
        if target is None:
            rustc = shutil.which("rustc") or str(Path.home() / ".cargo/bin/rustc")
            details = run([rustc, "-vV"], cwd=source, env=build_environment()).decode("utf8")
            target = next(line.removeprefix("host: ") for line in details.splitlines() if line.startswith("host: "))
        result = build_release(source, args.version, target, args.output_dir.resolve(),
                               args.signing_key.resolve(), args.cargo, openssl,
                               args.target_dir.resolve() if args.target_dir else None)
        print(json.dumps({k: result[k] for k in ("archive", "version", "target", "size", "sha256")}))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, subprocess.CalledProcessError, OSError) as error:
        print(f"Packaging failed: {error}", file=sys.stderr)
        raise SystemExit(1)
