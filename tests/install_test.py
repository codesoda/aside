"""Offline installer contract tests: no network, source builds, or interactive demo."""
import hashlib
import io
import os
import pty
import select
import termios
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
        (self.fixtures / 'inspect').write_bytes(b'inspect fixture')
        self.mock('shasum', '''#!/bin/sh
case "$*" in
  *inspect*) printf '%s  inspect\\n' "${INSPECT_HASH:-99cf4ea2a2a1048d8e9369a6a5a11e5f84ee3f3c706e0bde072f9b2bd44e96ba}" ;;
  *) /usr/bin/shasum "$@" ;;
esac
''')
        self.mock('sha256sum', '''#!/bin/sh
case "$*" in
  *inspect*) printf '%s  inspect\\n' "${INSPECT_HASH:-99cf4ea2a2a1048d8e9369a6a5a11e5f84ee3f3c706e0bde072f9b2bd44e96ba}" ;;
  *) shasum -a 256 "$1" ;;
esac
''')
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
      *inspect-*) cp "$FIXTURES/inspect" "$dest" ;;
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

    def environment(self, **overrides):
        return dict(os.environ, HOME=str(self.home), FIXTURES=str(self.fixtures),
                    PATH=str(self.bin) + os.pathsep + os.environ['PATH'], **overrides)

    def run_installer(self, *args, **env):
        return subprocess.run(['sh', str(self.script), *args], input='', text=True,
                              capture_output=True, env=self.environment(**env), timeout=30)

    def run_pty(self, answers):
        pid, master = pty.fork()
        if pid == 0:
            attrs = termios.tcgetattr(0)
            attrs[3] |= termios.ICANON | termios.ECHO
            attrs[0] |= termios.ICRNL
            termios.tcsetattr(0, termios.TCSANOW, attrs)
            null = os.open(os.devnull, os.O_RDONLY)
            os.dup2(null, 0)  # curl | sh has no prompt input on stdin.
            os.execve('/bin/sh', ['sh', str(self.script)], self.environment())
        try:
            output = b''
            for answer in answers:
                for _ in range(15):
                    if answer[0].encode() in output:
                        break
                    readable, _, _ = select.select([master], [], [], 10)
                    self.assertTrue(readable, output.decode(errors='replace'))
                    try:
                        output += os.read(master, 4096)
                    except OSError:
                        self.fail(output.decode(errors='replace'))
                else:
                    self.fail('prompt not shown: ' + output.decode(errors='replace'))
                os.write(master, answer[1].encode() + b'\n')
            readable, _, _ = select.select([master], [], [], 2)
            if readable:
                try:
                    output += os.read(master, 4096)
                except OSError:
                    pass
            for _ in range(20):
                finished, status = os.waitpid(pid, os.WNOHANG)
                if finished:
                    break
                select.select([], [], [], 0.1)
            if not finished:
                os.kill(pid, 9)
                os.waitpid(pid, 0)
                self.fail('installer hung: ' + output.decode(errors='replace'))
            self.assertEqual(os.waitstatus_to_exitcode(status), 0,
                             output.decode(errors='replace'))
            return output.decode(errors='replace')
        finally:
            os.close(master)

    def test_verified_install_skill_and_noninteractive_demo(self):
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.home / 'calls').read_text(), '--version\nskill\n')
        self.assertNotIn('sample review?', result.stdout)
        self.assertEqual(os.readlink(self.home / '.local/bin/aside'),
                         str(self.home / '.aside/bin/aside'))
        self.assertEqual(os.readlink(self.home / '.local/bin/inspect'),
                         str(self.home / '.aside/bin/inspect'))
        self.assertEqual((self.home / '.aside/bin/inspect').read_bytes(), b'inspect fixture')
        self.assertIn('FSL-1.1-ALv2', result.stdout)

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
        app = self.root / 'app/package.json'
        app.parent.mkdir(parents=True)
        app.write_text('{"name":"aside-web"}')
        target = self.root / 'target/release'
        target.mkdir(parents=True)
        source = target / 'aside'
        source.write_bytes(self.binary)
        source.chmod(0o755)
        self.mock('cargo', '#!/bin/sh\n[ "$*" = "build --locked --release" ]\n')
        self.mock('pnpm', '#!/bin/sh\nexit 0\n')
        self.mock('curl', '''#!/bin/sh
case " $* " in
  *inspect-linux-x86_64*)
    for arg do
      if [ "$prev" = '-o' ]; then dest=$arg; fi
      prev=$arg
    done
    cp "$FIXTURES/inspect" "$dest"
    printf 200 ;;
  *) exit 99 ;;
esac
''')
        result = self.run_installer('--from-source')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.home / 'calls').read_text(), '--version\nskill\n')

    def test_checkout_still_downloads_latest_by_default(self):
        (self.root / 'Cargo.toml').write_text('[package]\nname="aside"\n')
        skill = self.root / 'skills/aside/SKILL.md'
        skill.parent.mkdir(parents=True)
        skill.write_text('name: aside\n')
        app = self.root / 'app/package.json'
        app.parent.mkdir(parents=True)
        app.write_text('{"name":"aside-web"}')
        self.mock('cargo', '#!/bin/sh\nexit 99\n')
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.home / 'calls').read_text(), '--version\nskill\n')

    def test_explicit_source_requires_checkout(self):
        result = self.run_installer('--from-source')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('requires the Aside checkout', result.stderr)

    def test_inspect_mismatch_never_installs_inspect(self):
        result = self.run_installer(INSPECT_HASH='0' * 64)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('SHA256 mismatch for inspect-linux-x86_64', result.stderr)
        self.assertFalse((self.home / '.aside/bin/inspect').exists())
        self.assertFalse((self.home / '.aside/bin/aside').exists())

    def test_inspect_link_collision_and_symlink_destination(self):
        for path in (self.home / '.local/bin/inspect', self.home / '.aside/bin/inspect'):
            with self.subTest(path=path):
                path.parent.mkdir(parents=True, exist_ok=True)
                path.symlink_to(self.root / 'protected')
                result = self.run_installer()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('refusing to replace', result.stderr)
                self.assertTrue(path.is_symlink())
                path.unlink()

    def test_inspect_collision_preserves_previous_aside_and_skips_skill(self):
        managed = self.home / '.aside/bin'
        links = self.home / '.local/bin'
        managed.mkdir(parents=True)
        links.mkdir(parents=True)
        previous = b'previous aside binary'
        (managed / 'aside').write_bytes(previous)
        (links / 'aside').symlink_to(managed / 'aside')
        (links / 'inspect').write_text('unrelated inspect')
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((managed / 'aside').read_bytes(), previous)
        self.assertEqual(os.readlink(links / 'aside'), str(managed / 'aside'))
        self.assertEqual((links / 'inspect').read_text(), 'unrelated inspect')
        self.assertFalse((self.home / 'calls').exists())

    def test_install_failure_restores_previous_binaries_and_links(self):
        managed = self.home / '.aside/bin'
        links = self.home / '.local/bin'
        managed.mkdir(parents=True)
        links.mkdir(parents=True)
        for name in ('aside', 'inspect'):
            (managed / name).write_bytes(f'previous {name}'.encode())
            (links / name).symlink_to(managed / name)
        # Force the second link operation to fail after both binaries are replaced.
        self.mock('ln', '#!/bin/sh\ncase "$*" in *inspect) exit 1 ;; esac\n/bin/ln "$@"\n')
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        for name in ('aside', 'inspect'):
            self.assertEqual((managed / name).read_bytes(), f'previous {name}'.encode())
            self.assertEqual(os.readlink(links / name), str(managed / name))
        self.assertFalse((self.home / 'calls').exists())

    def test_link_backup_move_failure_preserves_previous_install(self):
        managed = self.home / '.aside/bin'
        links = self.home / '.local/bin'
        managed.mkdir(parents=True)
        links.mkdir(parents=True)
        for name in ('aside', 'inspect'):
            (managed / name).write_bytes(f'previous {name}'.encode())
            (links / name).symlink_to(managed / name)
        self.mock('mv', '''#!/bin/sh
[ "$1" != "$FAULT_LINK" ] || exit 42
/bin/mv "$@"
''')
        for name in ('aside', 'inspect'):
            with self.subTest(failed_backup=name):
                for restored in ('aside', 'inspect'):
                    (managed / restored).write_bytes(f'previous {restored}'.encode())
                    (links / restored).unlink(missing_ok=True)
                    (links / restored).symlink_to(managed / restored)
                result = self.run_installer(FAULT_LINK=str(links / name))
                self.assertNotEqual(result.returncode, 0)
                for preserved in ('aside', 'inspect'):
                    self.assertEqual((managed / preserved).read_bytes(),
                                     f'previous {preserved}'.encode())
                    self.assertTrue((links / preserved).is_symlink())
                    self.assertEqual(os.readlink(links / preserved), str(managed / preserved))
                self.assertFalse((self.home / 'calls').exists())

    def test_link_creation_failure_restores_previous_install(self):
        managed = self.home / '.aside/bin'
        links = self.home / '.local/bin'
        managed.mkdir(parents=True)
        links.mkdir(parents=True)
        for name in ('aside', 'inspect'):
            (managed / name).write_bytes(f'previous {name}'.encode())
            (links / name).symlink_to(managed / name)
        self.mock('ln', '''#!/bin/sh
[ "$3" != "$FAULT_LINK" ] || exit 42
/bin/ln "$@"
''')
        for name in ('aside', 'inspect'):
            with self.subTest(failed_creation=name):
                for restored in ('aside', 'inspect'):
                    (managed / restored).write_bytes(f'previous {restored}'.encode())
                    (links / restored).unlink(missing_ok=True)
                    (links / restored).symlink_to(managed / restored)
                (self.home / 'calls').unlink(missing_ok=True)
                result = self.run_installer(FAULT_LINK=str(links / name))
                self.assertNotEqual(result.returncode, 0)
                for preserved in ('aside', 'inspect'):
                    self.assertEqual((managed / preserved).read_bytes(),
                                     f'previous {preserved}'.encode())
                    self.assertTrue((links / preserved).is_symlink())
                    self.assertEqual(os.readlink(links / preserved), str(managed / preserved))
                self.assertFalse((self.home / 'calls').exists())

    def test_skill_failure_rolls_back_previous_install(self):
        managed = self.home / '.aside/bin'
        links = self.home / '.local/bin'
        managed.mkdir(parents=True)
        links.mkdir(parents=True)
        for name in ('aside', 'inspect'):
            (managed / name).write_bytes(f'previous {name}'.encode())
            (links / name).symlink_to(managed / name)
        self.fixture(b'#!/bin/sh\n[ "$1" != skill ]\n')
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('embedded skill installation failed', result.stderr)
        for name in ('aside', 'inspect'):
            self.assertEqual((managed / name).read_bytes(), f'previous {name}'.encode())
            self.assertEqual(os.readlink(links / name), str(managed / name))

    def test_unsupported_target_fails_closed(self):
        self.mock('uname', '#!/bin/sh\ncase "$1" in -s) echo FreeBSD ;; -m) echo x86_64 ;; esac\n')
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('unsupported Inspect target', result.stderr)
        self.assertFalse((self.home / '.aside/bin/aside').exists())

    def test_pty_decline_uses_tty_not_stdin(self):
        output = self.run_pty([('Start an optional live sample review?', 'n')])
        self.assertIn('Start an optional live sample review?', output)
        self.assertFalse((self.home / 'agent-calls').exists())

    def test_pty_selection_launches_exact_prompt(self):
        self.mock('pi', '#!/bin/sh\nprintf "%s\\n" "$*" >> "$HOME/agent-calls"\n')
        self.mock('claude', '#!/bin/sh\nexit 99\n')
        output = self.run_pty([('Start an optional live sample review?', 'y'),
                               ('Choose an installed agent CLI', 'pi')])
        self.assertIn('Choose an installed agent CLI', output)
        self.assertEqual((self.home / 'agent-calls').read_text(),
                         'use aside to review https://github.com/cli/cli/pull/14921\n')
        self.assertEqual((self.home / 'calls').read_text(), '--version\nskill\n')

    def test_launched_agent_prefers_new_binaries_on_path(self):
        self.mock('pi', '''#!/bin/sh
[ "$(command -v aside)" = "$HOME/.aside/bin/aside" ] || exit 8
[ "$(command -v inspect)" = "$HOME/.aside/bin/inspect" ] || exit 9
printf '%s\\n' "$*" >> "$HOME/agent-calls"
''')
        self.mock('aside', '#!/bin/sh\nexit 99\n')
        self.mock('inspect', '#!/bin/sh\nexit 99\n')
        output = self.run_pty([('Start an optional live sample review?', 'y'),
                               ('Choose an installed agent CLI', 'pi')])
        self.assertIn('Choose an installed agent CLI', output)
        self.assertTrue((self.home / 'agent-calls').exists())

    def test_symlinked_install_directory_rejected(self):
        (self.home / '.aside').symlink_to(self.root, target_is_directory=True)
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('symlink', result.stderr)


if __name__ == '__main__':
    unittest.main()
