"""Offline installer contract tests: no network, source builds, or interactive demo."""
import hashlib
import io
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'install.sh'


class InstallTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.home = self.root / 'home'
        self.home.mkdir()
        self.bin = self.root / 'mock-bin'
        self.bin.mkdir()
        self.script = self.root / 'install.sh'
        self.script.write_bytes(SCRIPT.read_bytes())
        self.asset = 'aside-v1.2.3-x86_64-unknown-linux-gnu.tar.gz'
        self.binary = b'#!/bin/sh\nprintf "%s\\n" "$1" >> "$HOME/calls"\n'
        self.fixture(self.binary)
        self.mock('uname', '#!/bin/sh\ncase "$1" in -s) echo Linux ;; -m) echo x86_64 ;; esac\n')
        self.mock('curl', '''#!/bin/sh
case " $* " in
  *releases/latest*) printf '%s' 'https://github.com/codesoda/aside/releases/tag/v1.2.3' ;;
  *)
    for arg do
      if [ "$prev" = '-o' ]; then dest=$arg; fi
      prev=$arg
    done
    case " $* " in
      *checksums-sha256.txt*) cp "$FIXTURES/checksums-sha256.txt" "$dest" ;;
      *) cp "$FIXTURES/archive.tar.gz" "$dest" ;;
    esac
    printf 200
  ;;
esac
''')

    def mock(self, name, content):
        path = self.bin / name
        path.write_text(content)
        path.chmod(0o755)

    def fixture(self, content, entry='aside', kind=tarfile.REGTYPE, checksum=None):
        self.fixtures = self.root / 'fixtures'
        self.fixtures.mkdir(exist_ok=True)
        archive = self.fixtures / 'archive.tar.gz'
        with tarfile.open(archive, 'w:gz') as tar:
            info = tarfile.TarInfo(entry)
            info.type = kind
            if kind == tarfile.REGTYPE:
                info.size = len(content)
                tar.addfile(info, io.BytesIO(content))
            else:
                info.linkname = '/tmp/elsewhere'
                tar.addfile(info)
        digest = checksum or hashlib.sha256(archive.read_bytes()).hexdigest()
        (self.fixtures / 'checksums-sha256.txt').write_text(f'{digest}  {self.asset}\n')

    def run_installer(self):
        env = dict(os.environ, HOME=str(self.home), FIXTURES=str(self.fixtures),
                   PATH=str(self.bin) + os.pathsep + os.environ['PATH'])
        return subprocess.run(['sh', str(self.script)], input='', text=True,
                              capture_output=True, env=env, timeout=30)

    def test_verified_install_skill_and_noninteractive_demo(self):
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.home / 'calls').read_text(), '--version\nskill\n')
        self.assertIn('explicit approval', result.stdout)
        self.assertEqual(os.readlink(self.home / '.local/bin/aside'),
                         str(self.home / '.aside/bin/aside'))

    def test_checksum_mismatch_never_installs(self):
        self.fixture(self.binary, checksum='0' * 64)
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('SHA256 mismatch', result.stderr)
        self.assertFalse((self.home / '.aside/bin/aside').exists())

    def test_traversal_and_symlink_entries_rejected(self):
        for entry, kind in [('../aside', tarfile.REGTYPE), ('aside', tarfile.SYMTYPE)]:
            with self.subTest(entry=entry, kind=kind):
                self.fixture(self.binary, entry, kind)
                result = self.run_installer()
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((self.home / '.aside/bin/aside').exists())

    def test_unrelated_link_is_not_replaced(self):
        link = self.home / '.local/bin/aside'
        link.parent.mkdir(parents=True)
        link.write_text('not ours')
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(link.read_text(), 'not ours')

    def test_duplicate_checksum_rejected(self):
        checksums = self.fixtures / 'checksums-sha256.txt'
        checksums.write_text(checksums.read_text() * 2)
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.home / '.aside/bin/aside').exists())

    def test_source_checkout_build_skips_release_download(self):
        (self.root / 'Cargo.toml').write_text('[package]\nname="aside"\n')
        skill = self.root / 'skills/aside/SKILL.md'
        skill.parent.mkdir(parents=True)
        skill.write_text('name: aside\n')
        target = self.root / 'target/release'
        target.mkdir(parents=True)
        source = target / 'aside'
        source.write_bytes(self.binary)
        source.chmod(0o755)
        self.mock('cargo', '#!/bin/sh\n[ "$*" = "build --locked --release" ]\n')
        self.mock('pnpm', '#!/bin/sh\nexit 0\n')
        self.mock('curl', '#!/bin/sh\nexit 99\n')
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.home / 'calls').read_text(), '--version\nskill\n')

    def test_symlinked_install_directory_rejected(self):
        (self.home / '.aside').symlink_to(self.root, target_is_directory=True)
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('symlink', result.stderr)


if __name__ == '__main__':
    unittest.main()
