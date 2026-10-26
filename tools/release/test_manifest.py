"""Public build metadata must count production changes, not CI/test upkeep."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("manifest.py").resolve()


class MetadataTests(unittest.TestCase):
    def test_ci_and_test_only_commits_do_not_increase_the_build_number(self):
        for changed in (".github/workflows/release.yml", "tools/release/smoke.ps1",
                        "apps/desktop/src/example.test.ts", "crates/mail/tests/example.rs"):
            with self.subTest(path=changed), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)

                def run(*args):
                    return subprocess.check_output(args, cwd=root, text=True).strip()

                def write(name, content):
                    path = root / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text(content)

                run("git", "init", "-q")
                run("git", "config", "user.name", "Fixture")
                run("git", "config", "user.email", "fixture@example.invalid")
                write("apps/desktop/package.json", '{"version":"0.2.0"}')
                write("apps/desktop/src-tauri/tauri.conf.json", '{"version":"0.2.0"}')
                write("apps/desktop/src-tauri/Cargo.toml", '[package]\nversion="0.2.0"\n')
                run("git", "add", ".")
                run("git", "commit", "-qm", "App starts")
                write(changed, "Only CI or tests changed\n")
                run("git", "add", ".")
                run("git", "commit", "-qm", "Maintain checks")
                run("python3", str(SCRIPT), "metadata", "--output", "info.json")
                info = json.loads((root / "info.json").read_text())
                self.assertEqual(info["version"], "0.2.0+1")
                self.assertEqual([c["subject"] for c in info["build"]["commits"]], ["App starts"])


if __name__ == "__main__":
    unittest.main()
