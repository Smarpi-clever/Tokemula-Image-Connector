#!/usr/bin/env python3
"""Collect published license notices for the selected target's production graph."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess


_LICENSE_ORDER = [
    "MIT", "MIT-0", "ISC", "0BSD", "BSD-2-Clause", "BSD-3-Clause", "Zlib",
    "Unicode-3.0", "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "BSL-1.0",
    "CC0-1.0", "Unlicense",
]


def selected_license(expression: str) -> str:
    """Resolve SPDX OR choices while retaining every mandatory AND constituent."""
    expression = expression.replace("MIT/Apache-2.0", "MIT OR Apache-2.0")
    tokens = re.findall(r"\(|\)|[A-Za-z0-9.+-]+", expression)
    if " ".join(tokens).replace(" ", "") != re.sub(r"\s", "", expression):
        raise ValueError(f"Unsupported license expression: {expression}")
    position = 0

    def atom():
        nonlocal position
        if position >= len(tokens):
            raise ValueError(f"Incomplete license expression: {expression}")
        token = tokens[position]
        position += 1
        if token == "(":
            result = choice()
            if position >= len(tokens) or tokens[position] != ")":
                raise ValueError(f"Unbalanced license expression: {expression}")
            position += 1
            return result
        if position < len(tokens) and tokens[position] == "WITH":
            if position + 1 >= len(tokens):
                raise ValueError(f"Incomplete exception: {expression}")
            token += " WITH " + tokens[position + 1]
            position += 2
        if token not in _LICENSE_ORDER:
            raise ValueError(f"License needs an explicit distribution decision: {token}")
        return [token]

    def conjunction():
        nonlocal position
        result = atom()
        while position < len(tokens) and tokens[position] == "AND":
            position += 1
            result += atom()
        return list(dict.fromkeys(result))

    def choice():
        nonlocal position
        alternatives = [conjunction()]
        while position < len(tokens) and tokens[position] == "OR":
            position += 1
            alternatives.append(conjunction())
        return min(alternatives, key=lambda values: (max(_LICENSE_ORDER.index(v) for v in values), len(values)))

    result = choice()
    if position != len(tokens):
        raise ValueError(f"Unconsumed license expression: {expression}")
    return " AND ".join(result)


def _run(workspace: Path, *arguments: str) -> str:
    tool = os.environ.get("CARGO") or shutil.which("cargo") or str(Path.home() / ".cargo/bin/cargo")
    return subprocess.check_output([tool, *arguments], cwd=workspace, text=True)


def _legal_files(root: Path, declared: str | None) -> list[Path]:
    paths = {
        path for path in root.rglob("*")
        if path.is_file() and path.name.upper().startswith(
            ("LICENSE", "LICENCE", "COPYING", "COPYRIGHT", "NOTICE")
        )
    }
    if declared:
        path = (root / declared).resolve()
        if not path.is_relative_to(root.resolve()) or not path.is_file():
            raise ValueError(f"Invalid declared license file in {root.name}")
        paths.add(path)
    if not paths:
        raise ValueError(f"No published license text found for {root.name}")
    if any(path.is_symlink() or not path.resolve().is_relative_to(root.resolve()) for path in paths):
        raise ValueError(f"License path leaves the published package: {root.name}")
    return sorted(paths)


def collect(workspace_root: Path, output_dir: Path, target: str) -> dict:
    workspace = workspace_root.resolve()
    metadata = json.loads(_run(
        workspace, "metadata", "--locked", "--offline", "--format-version", "1",
        "--filter-platform", target,
    ))
    # cargo metadata unifies dev features; cargo tree excludes their activation as well as edges.
    tree = _run(workspace, "tree", "--locked", "--offline", "--target", target,
                "--edges", "normal,build", "--prefix", "none", "--format", "{p}")
    identities = {match.groups() for line in tree.splitlines()
                  if (match := re.match(r"^(\S+) v(\S+)", line))}
    packages = [package for package in metadata["packages"]
                if (package["name"], package["version"]) in identities
                and package["id"] not in metadata["workspace_members"]]
    if not packages:
        raise ValueError("The selected target has no third-party production dependencies")
    if output_dir.exists() and any(output_dir.iterdir()):
        raise ValueError("Third-party output directory must be new or empty")
    output_dir.mkdir(parents=True, exist_ok=True)
    records = []
    for package in sorted(packages, key=lambda value: (value["name"], value["version"])):
        expression = package.get("license")
        if not expression:
            raise ValueError(f"Missing license declaration: {package['name']}")
        selection = selected_license(expression)
        root = Path(package["manifest_path"]).parent
        texts = _legal_files(root, package.get("license_file"))
        directory = output_dir / "crates" / f"{package['name']}-{package['version']}"
        filenames = []
        for source in texts:
            relative = source.relative_to(root)
            destination = directory / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, destination)
            if destination.stat().st_size == 0:
                raise ValueError(f"Empty license text: {package['name']}/{relative}")
            filenames.append(destination.relative_to(output_dir).as_posix())
        records.append({
            "name": package["name"], "version": package["version"],
            "source": package.get("source"), "repository": package.get("repository"),
            "declared_license": expression, "selected_license": selection,
            "texts": filenames,
        })

    rustc = os.environ.get("RUSTC") or shutil.which("rustc") or str(Path.home() / ".cargo/bin/rustc")
    sysroot = Path(subprocess.check_output([rustc, "--print", "sysroot"], cwd=workspace, text=True).strip())
    rust_version = subprocess.check_output([rustc, "--version"], cwd=workspace, text=True).strip()
    rust_legal = sysroot / "share/doc/rust"
    copyright_file = rust_legal / "COPYRIGHT-library.html"
    if not copyright_file.is_file():
        raise ValueError("The Rust standard-library copyright notices are missing from the toolchain")
    rust_output = output_dir / "rust-standard-library"
    rust_output.mkdir()
    shutil.copyfile(copyright_file, rust_output / copyright_file.name)
    if not (rust_legal / "licenses").is_dir():
        raise ValueError("The toolchain's referenced license texts are missing")
    shutil.copytree(rust_legal / "licenses", rust_output / "licenses")

    summary = {"target": target, "package_count": len(records), "packages": records,
               "rust_standard_library": {"toolchain": rust_version,
                                         "notice": "rust-standard-library/COPYRIGHT-library.html"}}
    (output_dir / "inventory.json").write_text(json.dumps(summary, indent=2, ensure_ascii=False) + "\n")
    notice = [
        "# Third-party notices\n",
        f"Target: `{target}`. This inventory contains {len(records)} normal/build Cargo dependencies; "
        "dependencies used only for development tests are excluded.\n",
        "The selected SPDX expression records the chosen permissive OR branches; every mandatory "
        "AND term remains. The published legal files are retained in full, including bundled-code "
        "and copyright notices. Apache license grants and any applicable NOTICE files are included.\n",
        "Rust standard-library notices supplied with the build toolchain are in "
        "[rust-standard-library/COPYRIGHT-library.html](rust-standard-library/COPYRIGHT-library.html). "
        "Native operating-system libraries remain subject to the operating system's distribution.\n",
        "| Package | Declared license | Selected license | Published legal text |\n",
        "|---|---|---|---|\n",
    ]
    for record in records:
        links = ", ".join(f"[{Path(path).name}]({path})" for path in record["texts"])
        notice.append(f"| {record['name']} {record['version']} | {record['declared_license']} | "
                      f"{record['selected_license']} | {links} |\n")
    notice.append("\nSource package versions, registry/repository references and text paths are retained in "
                  "[inventory.json](inventory.json). No source checkout or Cargo installation is needed to read these notices.\n")
    (output_dir / "NOTICE.md").write_text("\n".join(notice))
    return summary


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("workspace", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--target", required=True)
    args = parser.parse_args()
    result = collect(args.workspace, args.output, args.target)
    print(json.dumps({"target": result["target"], "package_count": result["package_count"]}))
