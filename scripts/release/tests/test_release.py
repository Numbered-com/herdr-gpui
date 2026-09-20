"""Run: python3 -m unittest discover -s scripts/release/tests -v"""
import json
import os
from pathlib import Path
import subprocess
import shutil
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
SCRIPTS = ROOT / "scripts/release"


def build_identity(worktree=False):
    return (b"not executable\0HERDR_BUILD_IDENTITY_V1\nworktree="
            + (b"1\nbranch=feature/test\npr=42\n\0" if worktree else b"0\nbranch=\npr=\n\0"))


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="herdr-release-test-")
        self.addCleanup(self.temp.cleanup)
        self.work = Path(self.temp.name)
        self.env = {"PATH": os.environ["PATH"], "HOME": str(self.work)}
        self.notices = self.work / "third party notices.txt"
        self.notices.write_text("Third-party license text\nCopyright example\n")

    def run_script(self, name, *args, success=True):
        result = subprocess.run(
            ["bash", str(getattr(self, "scripts", SCRIPTS) / name), *map(str, args)],
            env=self.env, capture_output=True, text=True, timeout=30,
        )
        if success:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0)
        return result

    def test_cask(self):
        first = self.run_script("render-cask.sh", "1.2.3", "AB" * 32).stdout
        self.assertEqual(first, self.run_script("render-cask.sh", "1.2.3", "ab" * 32).stdout)
        self.assertIn('version "1.2.3"', first)
        self.assertIn('sha256 "' + "ab" * 32 + '"', first)
        self.assertIn("penso/herdr-gpui/releases/download/v#{version}/Herdr-#{version}-universal-apple-darwin.dmg", first)
        self.assertIn('depends_on macos: ">= :sequoia"', first)
        for version in ["v1.2.3", "1.2", "01.2.3", "1.2.3-rc1", "1.2.3+build", "1.2.3\n", "$(id)"]:
            self.run_script("render-cask.sh", version, "ab" * 32, success=False)
        for sha in ["abc", "g" * 64, "a" * 65, "a" * 63 + "\n"]:
            self.run_script("render-cask.sh", "1.2.3", sha, success=False)

    def test_linux_archive(self):
        binary = self.work / "input binary"
        binary.write_bytes(build_identity())
        result = self.run_script("package-linux.sh", "1.2.3", "x86_64-unknown-linux-gnu", binary, self.work, self.notices)
        with tarfile.open(result.stdout.strip()) as archive:
            base = "Herdr-1.2.3-x86_64-unknown-linux-gnu/"
            files = {m.name: m for m in archive.getmembers() if m.isfile()}
            self.assertEqual(set(files), {base + p for p in [
                "bin/herdr-gpui", "share/applications/herdr-gpui.desktop",
                "share/icons/hicolor/1024x1024/apps/herdr-gpui.png",
                "share/licenses/herdr-gpui/LICENSE-APACHE", "share/licenses/herdr-gpui/NOTICE.md",
                "share/licenses/herdr-gpui/LICENSE", "share/licenses/herdr-gpui/NOTICE",
                "share/licenses/herdr-gpui/LICENSE-octicons",
                "share/licenses/herdr-gpui/THIRD-PARTY-NOTICES.txt",
            ]})
            self.assertEqual(files[base + "bin/herdr-gpui"].mode & 0o777, 0o755)
            self.assertEqual(archive.extractfile(base + "share/icons/hicolor/1024x1024/apps/herdr-gpui.png").read(), (ROOT / "assets/icons/herdr-1024.png").read_bytes())
            self.assertEqual(archive.extractfile(base + "share/licenses/herdr-gpui/THIRD-PARTY-NOTICES.txt").read(), self.notices.read_bytes())
            for source in ("LICENSE", "NOTICE", "assets/icons/LICENSE-octicons", "crates/herdr-protocol/NOTICE.md"):
                self.assertEqual(archive.extractfile(base + "share/licenses/herdr-gpui/" + Path(source).name).read(), (ROOT / source).read_bytes())
        self.run_script("package-linux.sh", "1.2.3", "x86_64-unknown-linux-gnu", binary, self.work, self.notices, success=False)
        self.run_script("package-linux.sh", "1.2.3", "bad-target", binary, self.work, self.notices, success=False)

    def test_packaging_requires_notices(self):
        binary = self.work / "binary"
        binary.touch()
        for script, inputs in [("package-linux.sh", ["x86_64-unknown-linux-gnu", binary]),
                               ("package-macos.sh", [binary, binary])]:
            self.run_script(script, "1.2.3", *inputs, self.work, success=False)
            for notices in [self.work / "missing", binary]:
                result = self.run_script(script, "1.2.3", *inputs, self.work, notices, success=False)
                self.assertIn("Nonempty third-party notices", result.stderr)
        self.assertFalse((self.work / "Herdr.app").exists())
        self.assertFalse(list(self.work.glob("*.tar.gz")))

    def mock_tools(self):
        tools = self.work / "tools"
        tools.mkdir()
        mock = tools / "mock.py"
        mock.write_bytes((SCRIPTS / "tests/mock-tool.py").read_bytes())
        mock.chmod(0o755)
        for tool in ["security", "openssl", "plutil", "lipo", "ditto", "xcrun", "hdiutil", "codesign", "spctl"]:
            (tools / tool).symlink_to(mock)
        self.env.update(PATH=str(tools) + os.pathsep + self.env["PATH"], MOCK_LOG=str(self.work / "log"))

    def test_unsigned_assembly(self):
        self.mock_tools()
        arm, intel = self.work / "arm64", self.work / "x86_64"
        arm.write_bytes(build_identity())
        intel.write_bytes(build_identity())
        self.run_script("package-macos.sh", "1.2.3", arm, intel, self.work, self.notices)
        app = self.work / "Herdr.app"
        self.assertEqual({str(p.relative_to(app)) for p in app.rglob("*") if p.is_file()}, {
            "Contents/MacOS/Herdr", "Contents/Info.plist", "Contents/Resources/Herdr.icns",
            "Contents/Resources/LICENSE-APACHE", "Contents/Resources/NOTICE.md",
            "Contents/Resources/LICENSE", "Contents/Resources/NOTICE", "Contents/Resources/LICENSE-octicons",
            "Contents/Resources/THIRD-PARTY-NOTICES.txt",
        })
        self.assertEqual((app / "Contents/Resources/THIRD-PARTY-NOTICES.txt").read_bytes(), self.notices.read_bytes())
        self.assertEqual((app / "Contents/Resources/Herdr.icns").read_bytes(), (ROOT / "assets/icons/Herdr.icns").read_bytes())
        for source in ("LICENSE", "NOTICE", "assets/icons/LICENSE-octicons", "crates/herdr-protocol/NOTICE.md"):
            self.assertEqual((app / "Contents/Resources" / Path(source).name).read_bytes(), (ROOT / source).read_bytes())
        self.run_script("package-macos.sh", "1.2.3", arm, intel, self.work, self.notices, success=False)

    def test_worktree_icons_and_mismatched_architectures(self):
        self.mock_tools()
        # Distinct fixture artwork proves selection without generating real icons.
        fixture = self.work / "packaging checkout"
        for path in ["scripts/release/common.sh", "scripts/release/package-macos.sh",
                     "scripts/release/package-linux.sh", "scripts/release/build-icon.py",
                     "scripts/release/herdr-gpui.desktop", "assets/macos/Info.plist",
                     "LICENSE", "NOTICE", "assets/icons/LICENSE-octicons",
                     "crates/herdr-protocol/LICENSE-APACHE", "crates/herdr-protocol/NOTICE.md"]:
            destination = fixture / path
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / path, destination)
        for name in ["Herdr-worktree.icns", "herdr-worktree-1024.png"]:
            (fixture / "assets/icons" / name).write_bytes(b"red fixture " + name.encode())
        self.scripts = fixture / "scripts/release"
        arm, intel = self.work / "arm64", self.work / "x86_64"
        arm.write_bytes(build_identity(True))
        intel.write_bytes(build_identity())
        result = self.run_script("package-macos.sh", "1.2.3", arm, intel, self.work, self.notices, success=False)
        self.assertIn("different build identities", result.stderr)
        intel.write_bytes(build_identity(True))
        self.run_script("package-macos.sh", "1.2.3", arm, intel, self.work, self.notices)
        self.assertEqual((self.work / "Herdr.app/Contents/Resources/Herdr.icns").read_bytes(), (fixture / "assets/icons/Herdr-worktree.icns").read_bytes())
        result = self.run_script("package-linux.sh", "1.2.3", "aarch64-unknown-linux-gnu", arm, self.work, self.notices)
        with tarfile.open(result.stdout.strip()) as archive:
            self.assertEqual(archive.extractfile("Herdr-1.2.3-aarch64-unknown-linux-gnu/share/icons/hicolor/1024x1024/apps/herdr-gpui.png").read(), (fixture / "assets/icons/herdr-worktree-1024.png").read_bytes())

    def test_missing_malformed_conflicting_identity_fails_closed(self):
        binary = self.work / "binary"
        for data in [b"old binary", build_identity() + build_identity(True),
                     build_identity().replace(b"pr=\n", b"pr=0\n"),
                     build_identity().replace(b"worktree=0", b"worktree=9")]:
            binary.write_bytes(data)
            self.run_script("package-linux.sh", "1.2.3", "x86_64-unknown-linux-gnu", binary, self.work, self.notices, success=False)
            self.assertFalse(list(self.work.glob("*.tar.gz")))

    def test_signing_success_and_fail_closed(self):
        self.mock_tools()
        app = self.work / "unsigned.app"
        (app / "Contents/MacOS").mkdir(parents=True)
        (app / "Contents/MacOS/Herdr").write_text("never run")
        self.env.update(
            MACOS_CERTIFICATE_P12_BASE64="ZHVtbXk=", MACOS_CERTIFICATE_PASSWORD="dummy",
            APPLE_API_PRIVATE_KEY="dummy p8", APPLE_API_KEY_ID="dummy",
            APPLE_API_ISSUER_ID="dummy", MACOS_SIGNING_IDENTITY="Developer ID Application: Dummy",
        )
        output = self.work / "Herdr-1.2.3-universal-apple-darwin.dmg"
        for response in ['{"status":"Invalid"}', '{"status":"In Progress"}', '{}', 'not json',
                         '{"status":"Invalid"}\n{"status":"Accepted"}', '{"status":"Accepted"}']:
            with self.subTest(response=response):
                self.env["MOCK_NOTARY_JSON"] = response
                accepted = response == '{"status":"Accepted"}'
                self.run_script("sign-macos.sh", "1.2.3", app, self.work, success=accepted)
                self.assertEqual(output.exists(), accepted)
                self.assertFalse(list(self.work.glob(".herdr-sign.*")))
        output.unlink()
        for tool in ["codesign", "spctl", "hdiutil", "xcrun"]:
            self.env["MOCK_FAIL"] = tool
            self.run_script("sign-macos.sh", "1.2.3", app, self.work, success=False)
            self.assertFalse(output.exists())
            self.assertFalse(list(self.work.glob(".herdr-sign.*")))
        calls = [json.loads(line) for line in (self.work / "log").read_text().splitlines()]
        restores = [c for c in calls if c[:5] == ["security", "list-keychains", "-d", "user", "-s"]]
        self.assertTrue(restores)
        self.assertTrue(all(c[5:] == ["/mock/login keychain-db", "/mock/system.keychain"] for c in restores))
        signs = [c for c in calls if c[:2] == ["codesign", "--force"]]
        self.assertTrue(any(c[-1].endswith("/Contents/MacOS/Herdr") for c in signs))
        self.assertTrue(any(c[-1].endswith("/Herdr.app") for c in signs))
        self.assertTrue(all("--timestamp" in c for c in signs))
        self.assertTrue(all("runtime" in c for c in signs if not c[-1].endswith(".dmg")))
        del self.env["APPLE_API_PRIVATE_KEY"]
        before = (self.work / "log").read_text()
        self.run_script("sign-macos.sh", "1.2.3", app, self.work, success=False)
        self.assertEqual((self.work / "log").read_text(), before)


if __name__ == "__main__":
    unittest.main()
