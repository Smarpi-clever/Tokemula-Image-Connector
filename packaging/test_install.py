"""Bootstrap boundary checks; native registrations use the separate acceptance run."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('install.sh')


class InstallTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='tokemula-installer-test-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.home = self.root / 'home'; self.home.mkdir()
        self.bin = self.root / 'bin'; self.bin.mkdir()
        self.env = {'HOME': str(self.home), 'TMPDIR': str(self.root),
                    'PATH': f'{self.bin}:/usr/bin:/bin:/usr/sbin:/sbin',
                    'TOKEMULA_IMAGE_KEY': '', 'LANG': 'en_US.UTF-8'}
        self.tool('uname', '#!/bin/sh\ncase "$1" in -s) echo "${TEST_OS:-Darwin}";; -m) echo "${TEST_ARCH:-arm64}";; esac\n')
        self.tool('plutil', f'#!{sys.executable}\n' + "import json,sys\na=sys.argv[1:]\nassert len(a)==8 and a[0]=='-extract' and a[2:7]==['raw','-expect','string','-o','-']\nvalue=json.load(open(a[7]))[a[1]]\nassert isinstance(value,str)\nprint(value)\n")
        self.tool('curl', '#!/bin/sh\nprintf called > "$HOME/curl-called"\nwhile [ "$#" -gt 0 ]; do\nif [ "$1" = -o ]; then printf "synthetic invalid archive" > "$2"; exit 0; fi\nshift\ndone\nexit 1\n')

    def tool(self, name, text):
        path = self.bin / name; path.write_text(text); path.chmod(0o755)

    def run_script(self, *extra):
        return subprocess.run(['/bin/sh', str(SCRIPT), '--harness', 'codex', '--client', '/usr/bin/true', *extra], env=self.env, text=True, capture_output=True, timeout=20)

    def installation(self, version='0.1.0-wb1.4'):
        root = self.home / '.local/share/tokemula-image-connector'; (root / 'bin').mkdir(parents=True)
        (root / 'installation.json').write_text(json.dumps({'product': 'tokemula-image-connector', 'current': version}))
        return root

    def test_bad_archive_is_not_extracted_or_executed(self):
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('校验失败', result.stderr)
        self.assertTrue((self.home / 'curl-called').exists())
        self.assertFalse((self.home / '.local').exists())
        self.assertEqual(list(self.root.glob('tokemula-image-install.*')), [])

    def test_unsupported_platform_stops_before_download(self):
        for os_name, arch in [('Linux', 'aarch64'), ('Darwin', 'x86_64')]:
            with self.subTest(os=os_name, arch=arch):
                self.env.update(TEST_OS=os_name, TEST_ARCH=arch)
                result = self.run_script()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('仅支持', result.stderr)
                self.assertFalse((self.home / 'curl-called').exists())

    def test_other_version_is_not_replaced(self):
        root = self.installation('0.1.0-other')
        before = (root / 'installation.json').read_bytes()
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('不会自动升级或降级', result.stderr)
        self.assertEqual((root / 'installation.json').read_bytes(), before)
        self.assertFalse((self.home / 'curl-called').exists())

    def test_recovery_state_stops_before_download(self):
        root = self.installation(); (root / 'transaction.json').write_text('{}')
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('上次安装需恢复', result.stderr)
        self.assertTrue((root / 'transaction.json').exists())
        self.assertFalse((self.home / 'curl-called').exists())

    def test_native_stdout_error_is_reported(self):
        root = self.installation()
        manager = root / 'bin/tokemula-image'
        manager.write_text('#!/bin/sh\ncase "$1" in --version) echo "tokemula-image 0.1.0-wb1.4";; health) echo SYNTHETIC_NATIVE_HEALTH_FAILURE; exit 7;; *) exit 8;; esac\n')
        manager.chmod(0o755)
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('SYNTHETIC_NATIVE_HEALTH_FAILURE', result.stderr)
        self.assertFalse((self.home / 'curl-called').exists())

    def test_harness_specific_options_do_not_mutate_other_client(self):
        result = self.run_script('--workbuddy-app', '/synthetic/WorkBuddy AI.app')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('仅用于 WorkBuddy', result.stderr)
        self.assertFalse((self.home / 'curl-called').exists())


if __name__ == '__main__':
    unittest.main()
