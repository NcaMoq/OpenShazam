#!/usr/bin/env python3
"""Validate a stable release tag against the Cargo workspace version."""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path

_STABLE_TAG = re.compile(r"v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)\Z")
_DEFAULT_MANIFEST = Path(__file__).resolve().parents[1] / "Cargo.toml"


def validate_release_version(tag: str, manifest_path: Path = _DEFAULT_MANIFEST) -> str:
    match = _STABLE_TAG.fullmatch(tag)
    if match is None:
        raise ValueError(f"release tags must be stable vMAJOR.MINOR.PATCH values: {tag}")

    tag_version = ".".join(match.groups())
    with manifest_path.open("rb") as manifest_file:
        manifest = tomllib.load(manifest_file)
    try:
        workspace_version = manifest["workspace"]["package"]["version"]
    except (KeyError, TypeError) as error:
        raise ValueError("Cargo.toml must define [workspace.package].version") from error

    if workspace_version != tag_version:
        raise ValueError("release tag must match the Cargo workspace version")
    return tag_version


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: validate_release_version.py vMAJOR.MINOR.PATCH", file=sys.stderr)
        return 2
    try:
        version = validate_release_version(sys.argv[1])
    except (OSError, tomllib.TOMLDecodeError, ValueError) as error:
        print(f"Release version validation failed: {error}", file=sys.stderr)
        return 1
    print(version)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
