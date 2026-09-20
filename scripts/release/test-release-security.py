#!/usr/bin/env python3
"""Offline release contract tests; HERDR_TEST_SBOM=1 also runs cargo-cyclonedx."""

import hashlib
import importlib.util
import itertools
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location("manifest", ROOT / "scripts/release/artifact-manifest.py")
MANIFEST = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MANIFEST)
SPEC = importlib.util.spec_from_file_location("sbom", ROOT / "scripts/release/generate-sbom.py")
SBOM = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SBOM)
VERSION = "0.1.0"


class SbomMerge(unittest.TestCase):
    def generate(self, components):
        documents = [{
            "bomFormat": "CycloneDX", "specVersion": "1.5",
            "metadata": {"component": {"bom-ref": "herdr", "name": "herdr-gpui"}},
            "components": [component],
            "dependencies": [{"ref": "herdr", "dependsOn": ["libloading"]},
                             {"ref": "libloading", "dependsOn": []}],
        } for component in components]
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / "union.json"
            with mock.patch.object(SBOM.sys, "argv", ["generate-sbom.py", str(output)]), \
                    mock.patch.object(SBOM.shutil, "which", side_effect=lambda name: f"/mock/{name}"), \
                    mock.patch.object(SBOM.subprocess, "run") as run, \
                    mock.patch.object(Path, "read_text", side_effect=[json.dumps(d) for d in documents]):
                SBOM.main()
            self.assertEqual([call.args[0][call.args[0].index("--target") + 1]
                              for call in run.call_args_list], list(SBOM.TARGETS))
            return json.loads(output.read_text())

    def test_platform_scope_union(self):
        # Includes excluded macOS build-time / required Linux runtime, every
        # ordering, optional precedence, and the absent-scope required default.
        for scopes in itertools.product(("excluded", "optional", "required", None), repeat=3):
            with self.subTest(scopes=scopes):
                components = [{"bom-ref": "libloading", "name": "libloading", "version": "0.8.9",
                               **({"scope": scope} if scope is not None else {})} for scope in scopes]
                bom = self.generate(components)
                expected = ("required" if "required" in scopes or None in scopes else
                            "optional" if "optional" in scopes else "excluded")
                self.assertEqual(bom["components"], [dict(components[0], scope=expected)])
                self.assertEqual(bom["dependencies"], [
                    {"ref": "herdr", "dependsOn": ["libloading"]},
                    {"ref": "libloading", "dependsOn": []}])

    def test_non_scope_conflicts_rejected(self):
        component = {"bom-ref": "libloading", "name": "libloading", "version": "0.8.9",
                     "purl": "pkg:cargo/libloading@0.8.9", "scope": "excluded",
                     "hashes": [{"alg": "SHA-256", "content": "a" * 64}],
                     "licenses": [{"license": {"id": "MIT"}}]}
        for field, value in (
            ("name", "different"), ("version", "0.8.8"), ("purl", "pkg:cargo/other@0.8.9"),
            ("hashes", [{"alg": "SHA-256", "content": "b" * 64}]),
            ("licenses", [{"license": {"id": "Apache-2.0"}}]),
        ):
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, "Conflicting SBOM component"):
                self.generate([component, component, dict(component, scope="required", **{field: value})])

    def test_invalid_scope_rejected(self):
        for scope in (None, "unknown"):
            with self.subTest(scope=scope), self.assertRaisesRegex(ValueError, "Invalid SBOM component scope"):
                self.generate([{"bom-ref": "libloading", "name": "libloading", "scope": scope}] * 3)


class ReleaseSecurity(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name)
        for name in MANIFEST.base_names(VERSION):
            data = name.encode()
            (self.path / name).write_bytes(data)
            for algorithm in ("sha256", "sha512"):
                checksum = hashlib.new(algorithm, data).hexdigest()
                (self.path / f"{name}.{algorithm}").write_text(f"{checksum}  {name}\n")
            for suffix in ("sig", "crt"):
                (self.path / f"{name}.{suffix}").write_text("test fixture, not a signature\n")
        self.run_manifest("create")

    def run_manifest(self, mode, success=True, directory=None):
        result = subprocess.run(["python3", str(ROOT / "scripts/release/artifact-manifest.py"),
                                 mode, VERSION, str(directory or self.path)], capture_output=True)
        self.assertEqual(result.returncode == 0, success, result.stderr.decode())
        return result.stdout.decode()

    def test_complete_set_and_homebrew(self):
        self.run_manifest("verify")
        self.assertEqual(len((self.path / "SHA256SUMS").read_text().splitlines()), 15)
        with tempfile.TemporaryDirectory() as temp:
            name = MANIFEST.base_names(VERSION)[0]
            for file in (name, "SHA256SUMS"):
                shutil.copyfile(self.path / file, Path(temp) / file)
            self.assertEqual(self.run_manifest("dmg", directory=temp).strip(),
                             hashlib.sha256(name.encode()).hexdigest())

    def test_missing_signature(self):
        (self.path / (MANIFEST.base_names(VERSION)[0] + ".sig")).unlink()
        self.run_manifest("verify", False)

    def test_extra_asset(self):
        (self.path / "unexpected").write_text("extra")
        self.run_manifest("verify", False)

    def test_corrupt_sidecar(self):
        (self.path / (MANIFEST.base_names(VERSION)[0] + ".sha512")).write_text("invalid")
        self.run_manifest("verify", False)

    def test_duplicate_manifest_entry(self):
        path = self.path / "SHA256SUMS"
        path.write_text(path.read_text() + path.read_text().splitlines()[0] + "\n")
        self.run_manifest("verify", False)

    def test_symlink(self):
        path = self.path / (MANIFEST.base_names(VERSION)[0] + ".sig")
        path.unlink()
        path.symlink_to(self.path / "SHA256SUMS")
        self.run_manifest("verify", False)

    def test_invalid_versions_and_missing_arguments(self):
        for script in ("verify-release.sh", "gpg-sign-release.sh"):
            for args in (["--version"], ["--version", "20260920.01"], ["--version", "v01.2.3"]):
                result = subprocess.run(["bash", str(ROOT / "scripts" / script), *args],
                                        capture_output=True)
                self.assertNotEqual(result.returncode, 0)

    def test_sign_action_rejects_partial_glob_before_signing(self):
        # The action has a single final run block; no YAML dependency needed.
        text = (ROOT / ".github/actions/sign-artifacts/action.yml").read_text()
        code = "\n".join(line[8:] for line in text.split("      run: |\n", 1)[1].splitlines())
        result = subprocess.run(["bash", "-c", code], cwd=self.path,
                                env=dict(os.environ, FILES_PATTERN="*.dmg *.missing"),
                                capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"missing, empty, or symlink", result.stdout)
        self.assertNotIn(b"Signing:", result.stdout)

    def test_verifier_trust_policy_and_failures(self):
        with tempfile.TemporaryDirectory() as temp:
            tools = Path(temp)
            (tools / "gh").write_text('''#!/usr/bin/env bash
set -eu
if [[ $1 == api ]]; then
    printf '%040d\\n' 1
elif [[ $1 == attestation && $2 == verify ]]; then
    [[ "$*" == *"--source-ref refs/heads/main"* ]]
    [[ "$*" == *"--source-digest 0000000000000000000000000000000000000001"* ]]
    [[ "$*" == *"--signer-digest 0000000000000000000000000000000000000001"* ]]
    [[ "$*" == *"--cert-identity https://github.com/penso/herdr-gpui/.github/workflows/release.yml@refs/heads/main"* ]]
    exit "${FAIL_ATTEST:-0}"
else
    exit 99
fi
''')
            (tools / "cosign").write_text('''#!/usr/bin/env bash
set -eu
[[ $1 == verify-blob ]]
[[ "$*" == *"--certificate-identity https://github.com/penso/herdr-gpui/.github/workflows/release.yml@refs/heads/main"* ]]
[[ "$*" == *"--certificate-oidc-issuer https://token.actions.githubusercontent.com"* ]]
[[ "$*" == *"--certificate-github-workflow-sha 0000000000000000000000000000000000000001"* ]]
exit "${FAIL_COSIGN:-0}"
''')
            (tools / "gpg").write_text('''#!/usr/bin/env bash
set -eu
[[ $1 == --homedir && -d $2 && $2 == */keyring ]]
if [[ "$*" == *"--verify"* ]]; then
    printf '[GNUPG:] VALIDSIG %s 2026-09-20 0 0 4 0 1 10 00 %040d\\n' "$TEST_SIGNER" 2
fi
''')
            for tool in tools.iterdir():
                tool.chmod(0o755)
            env = dict(os.environ, PATH=f"{tools}:{os.environ['PATH']}")
            command = ["bash", str(ROOT / "scripts/verify-release.sh"),
                       "--version", "v0.1.0", "--directory", str(self.path)]
            for extra, updates, success in (
                ([], {}, True),
                (["--sha", "2" * 40], {}, False),
                ([], {"FAIL_COSIGN": "1"}, False),
                ([], {"FAIL_ATTEST": "1"}, False),
            ):
                result = subprocess.run(command + extra, env=dict(env, **updates), capture_output=True)
                self.assertEqual(result.returncode == 0, success, result.stderr.decode())
            key = tools / "public.asc"
            key.write_text("mock public key")
            command += ["--gpg-key", str(key), "--gpg-fingerprint", "1" * 40]
            for signer, success in (("1" * 40, True), ("2" * 40, False)):
                result = subprocess.run(command, env=dict(env, TEST_SIGNER=signer), capture_output=True)
                self.assertEqual(result.returncode == 0, success, result.stderr.decode())

    @unittest.skipUnless(os.environ.get("HERDR_TEST_SBOM") == "1", "opt-in real metadata generation")
    def test_real_sbom(self):
        # Isolate generator outputs from the collaborative worktree.
        with tempfile.TemporaryDirectory() as temp:
            workspace = Path(temp)
            for name in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml"):
                shutil.copyfile(ROOT / name, workspace / name)
            shutil.copytree(ROOT / "crates", workspace / "crates",
                            ignore=shutil.ignore_patterns("target", "sbom.json"))
            shutil.copytree(ROOT / "scripts/release", workspace / "scripts/release")
            (workspace / "scripts/release/cargo-locked.sh").chmod(0o755)
            before = (workspace / "Cargo.lock").read_bytes()
            subprocess.run(["python3", "scripts/release/generate-sbom.py", "release.cdx.json"],
                           cwd=workspace, check=True)
            self.assertEqual(before, (workspace / "Cargo.lock").read_bytes())
            bom = json.loads((workspace / "release.cdx.json").read_text())
            names = {component["name"] for component in bom["components"]}
            self.assertTrue({"gpui", "metal", "wayland-client", "cc"} <= names)
            manifest = workspace / "Cargo.toml"
            text = manifest.read_text()
            version = tomllib.loads(text)["workspace"]["package"]["version"]
            manifest.write_text(text.replace(f'version = "{version}"', 'version = "999.0.0"', 1))
            result = subprocess.run(["python3", "scripts/release/generate-sbom.py", "invalid.cdx.json"],
                                    cwd=workspace, capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(b"--locked", result.stderr)
            self.assertEqual(before, (workspace / "Cargo.lock").read_bytes())


if __name__ == "__main__":
    unittest.main()
