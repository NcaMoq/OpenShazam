#!/usr/bin/env python3
"""Unit tests for stable release tag and Cargo version validation."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from validate_release_version import validate_release_version


class ValidateReleaseVersionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.manifest_path = Path(self.temporary_directory.name) / "Cargo.toml"
        self.manifest_path.write_text(
            '[workspace.package]\nversion = "1.2.3"\n', encoding="utf-8"
        )

    def tearDown(self) -> None:
        self.temporary_directory.cleanup()

    def test_accepts_matching_stable_version(self) -> None:
        self.assertEqual(validate_release_version("v1.2.3", self.manifest_path), "1.2.3")

    def test_rejects_non_stable_or_non_canonical_tags(self) -> None:
        for tag in ("v01.2.3", "v1.2", "v1.2.3-rc.1", "1.2.3"):
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                validate_release_version(tag, self.manifest_path)

    def test_rejects_tag_that_does_not_match_workspace_version(self) -> None:
        with self.assertRaisesRegex(ValueError, "must match"):
            validate_release_version("v1.2.4", self.manifest_path)

    def test_rejects_missing_workspace_version(self) -> None:
        self.manifest_path.write_text("[package]\nversion = '1.2.3'\n", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "must define"):
            validate_release_version("v1.2.3", self.manifest_path)


if __name__ == "__main__":
    unittest.main()
