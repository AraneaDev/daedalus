"""Exercise the production graph gate with a disabled, renamed optional dependency."""

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


class PackageGraphTests(unittest.TestCase):
    def test_disabled_optional_production_edge_is_rejected(self):
        dependencies = {
            "daedalus-proto": [],
            "daedalus-vt": ["daedalus-proto"],
            "daedalusd": ["daedalus-vt", "daedalus-proto"],
            "daedalus-ctl": ["daedalus-proto"],
            "daedalus-ui": ["daedalus-proto"],
        }
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "Cargo.toml").write_text(
                '[workspace]\nmembers = ["crates/*"]\nresolver = "2"\n'
            )
            for name, deps in dependencies.items():
                crate = root / "crates" / name
                (crate / "src").mkdir(parents=True)
                (crate / "src/lib.rs").touch()
                (crate / "Cargo.toml").write_text(
                    f'[package]\nname = "{name}"\nversion = "0.1.0"\nedition = "2021"\n'
                    "[dependencies]\n"
                    + "".join(f'{dep} = {{ path = "../{dep}" }}\n' for dep in deps)
                )
            manifest = root / "crates/daedalus-ui/Cargo.toml"
            with manifest.open("a") as ui:
                ui.write(
                    'daemon = { package = "daedalusd", path = "../daedalusd", '
                    'optional = true }\n[features]\nspecial = ["dep:daemon"]\n'
                )
            metadata = subprocess.check_output(
                ["cargo", "metadata", "--offline", "--format-version", "1"], cwd=root
            )
            result = subprocess.run(
                [sys.executable, Path(__file__).with_name("check_package_graph.py")],
                input=metadata,
                capture_output=True,
            )
            self.assertEqual(result.returncode, 1)
            self.assertIn(b"('daedalus-ui', 'daedalusd')", result.stderr)
