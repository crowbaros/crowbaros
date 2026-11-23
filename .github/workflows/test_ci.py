"""CI's scripts, each taken from ci.yml and run on a small repository: the
`changes` job, which says which jobs a change needs, and the boot job's step
that gives the files its cached build was made from an old time."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

# Git as the runner has it: no configuration but its own defaults.
GIT_ENV = {'GIT_CONFIG_GLOBAL': os.devnull, 'GIT_CONFIG_SYSTEM': os.devnull,
           'GIT_AUTHOR_NAME': 'A test', 'GIT_AUTHOR_EMAIL': 'test@example.org',
           'GIT_COMMITTER_NAME': 'A test', 'GIT_COMMITTER_EMAIL': 'test@example.org'}

WORKFLOW = Path(__file__).resolve().parent / 'ci.yml'


def step_script(name):
    """The `run` script of the step with this name."""
    lines = WORKFLOW.read_text().splitlines()
    step = next(i for i, line in enumerate(lines) if line.strip() == f'- name: {name}')
    run = next(i for i in range(step, len(lines)) if lines[i].strip() == 'run: |')
    indent = len(lines[run]) - len(lines[run].lstrip()) + 2
    script = []
    for line in lines[run + 1:]:
        if line.strip() and len(line) - len(line.lstrip()) < indent:
            break
        script.append(line[indent:])
    return '\n'.join(script)


class Changes(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.repo = Path(temporary.name) / 'repo'
        self.repo.mkdir()
        self.output = Path(temporary.name) / 'output'
        # And the runner's locale, which the script must not count on.
        self.env = {'PATH': os.environ['PATH'], 'HOME': temporary.name, 'LC_ALL': 'C.UTF-8', **GIT_ENV}
        self.git('init', '-q', '-b', 'main')
        self.first = self.commit({'README.md': 'CrowbarOS\n', 'docs/VISION.md': 'The vision\n',
                                  'base/void_base.py': 'print("Base")\n' * 20,
                                  'compositor/src/main.rs': 'fn main() {}\n' * 20,
                                  'lab/void/guest.py': 'print("the guest")\n' * 20})

    def git(self, *args):
        return subprocess.run(['git', *args], cwd=self.repo, env=self.env, check=True,
                              capture_output=True, text=True).stdout.strip()

    def commit(self, files=(), moves=()):
        """Commits these files' contents and these moves, and returns the commit."""
        for path, text in dict(files).items():
            (self.repo / path).parent.mkdir(parents=True, exist_ok=True)
            (self.repo / path).write_text(text)
        for old, new in moves:
            (self.repo / new).parent.mkdir(parents=True, exist_ok=True)
            self.git('mv', old, new)
        self.git('add', '-A')
        self.git('commit', '-q', '-m', 'a change')
        return self.git('rev-parse', 'HEAD')

    def needs(self, **event):
        """What the job reports for a pull request (`base`, `head`), a push
        (`before`, `after`), or the week's run (`event='schedule'`)."""
        self.output.write_text('')
        env = dict(self.env, GITHUB_OUTPUT=str(self.output),
                   EVENT=event.get('event', 'pull_request' if 'head' in event else 'push'),
                   PR_BASE=event.get('base', ''), PR_HEAD=event.get('head', ''),
                   BEFORE=event.get('before', ''), AFTER=event.get('after', ''))
        self.log = subprocess.run(
            ['bash', '--noprofile', '--norc', '-eo', 'pipefail', '-c', step_script('Classify the changed files')],
            cwd=self.repo, env=env, check=True, capture_output=True, text=True).stdout
        return dict(line.split('=', 1) for line in self.output.read_text().splitlines())

    def test_docs_alone_need_no_job(self):
        head = self.commit({'docs/VISION.md': 'The vision, amended\n', 'README.md': 'CrowbarOS, again\n'})
        self.assertEqual(self.needs(base=self.first, head=head),
                         {'code': 'false', 'compositor': 'false', 'boot': 'false', 'agent': 'false'})

    def test_a_lab_script_needs_the_code_jobs_alone(self):
        head = self.commit({'lab/void/guest.py': 'print("another guest")\n'})
        self.assertEqual(self.needs(base=self.first, head=head),
                         {'code': 'true', 'compositor': 'false', 'boot': 'false', 'agent': 'false'})

    def test_the_compositor_needs_its_job_and_the_boot(self):
        head = self.commit({'compositor/src/main.rs': 'fn main() { loop {} }\n'})
        self.assertEqual(self.needs(base=self.first, head=head),
                         {'code': 'true', 'compositor': 'true', 'boot': 'true', 'agent': 'false'})

    def test_the_harness_needs_its_job_and_the_boot(self):
        head = self.commit({'agent/src/main.ts': 'await listen();\n'})
        self.assertEqual(self.needs(base=self.first, head=head),
                         {'code': 'true', 'compositor': 'false', 'boot': 'true', 'agent': 'true'})

    def test_what_pins_node_and_runs_the_harnesss_job_needs_that_job(self):
        # Base's configuration pins the Node.js the Harness runs on, and
        # void_base.py is the command the job runs.
        for path in ('base/config.json', 'base/void_base.py'):
            with self.subTest(path=path):
                before = self.git('rev-parse', 'HEAD')
                head = self.commit({path: '{"node_url": "another"}\n'})
                self.assertEqual(self.needs(base=before, head=head),
                                 {'code': 'true', 'compositor': 'false', 'boot': 'true', 'agent': 'true'})

    def test_base_apart_from_those_needs_the_boot_and_not_the_harnesss_job(self):
        head = self.commit({'base/host/image.sh': 'set -eu\n'})
        self.assertEqual(self.needs(base=self.first, head=head),
                         {'code': 'true', 'compositor': 'false', 'boot': 'true', 'agent': 'false'})

    def test_code_moved_into_docs_is_still_a_change_to_the_code(self):
        # Git names a moved file by where it went. Read that way, Base losing
        # a file to docs/ was a change to docs alone, and no job ran.
        head = self.commit(moves=[('base/void_base.py', 'docs/void_base.py')])
        self.assertEqual(self.needs(base=self.first, head=head),
                         {'code': 'true', 'compositor': 'false', 'boot': 'true', 'agent': 'true'})

    def test_a_push_that_moves_code_into_docs_is_read_the_same(self):
        after = self.commit(moves=[('compositor/src/main.rs', 'compositor/docs/main.rs')])
        self.assertEqual(self.needs(before=self.first, after=after),
                         {'code': 'true', 'compositor': 'true', 'boot': 'true', 'agent': 'false'})

    def test_a_file_git_would_name_in_quotes_is_read_by_its_path(self):
        # Git puts a name with a letter outside ASCII, a quote, or a
        # backslash in quotes, and a path that starts with a quote is not
        # under compositor/ or base/.
        for name in ('café.rs', 'a"quote".rs', 'back\\slash.rs', 'a tab\there.rs'):
            with self.subTest(name=name):
                before = self.git('rev-parse', 'HEAD')
                added = self.commit({f'compositor/src/{name}': 'fn main() {}\n' * 20})
                self.assertEqual(self.needs(base=before, head=added),
                                 {'code': 'true', 'compositor': 'true', 'boot': 'true', 'agent': 'false'})
                moved = self.commit(moves=[(f'compositor/src/{name}', f'docs/{name}.md')])
                self.assertEqual(self.needs(before=added, after=moved),
                                 {'code': 'true', 'compositor': 'true', 'boot': 'true', 'agent': 'false'})

    def test_a_file_whose_name_is_not_text_is_read_by_its_path(self):
        # A name that ends partway through a UTF-8 character. Read as text,
        # the byte could take the NUL after it, and the file with it.
        head = self.commit({os.fsdecode(b'base/half-a-letter-\xc2'): 'print("Base")\n'})
        self.assertEqual(self.needs(base=self.first, head=head),
                         {'code': 'true', 'compositor': 'false', 'boot': 'true', 'agent': 'false'})
        self.assertIn('code: base/half-a-letter-?\n', self.log)

    def test_no_files_name_writes_what_the_runner_would_read_as_a_command(self):
        # The runner reads `::` at the start of a log line, and `##[`
        # anywhere in one, as a command. One that hid or set the job's
        # outputs would leave every job skipped, and the gate takes a
        # skipped job as passed.
        head = self.commit({'compositor/src/a\n::add-mask::true': 'fn main() {}\n',
                            'compositor/src/b\n##[set-output name=code;]false': 'fn main() {}\n',
                            '::add-mask::false': 'a file at the top, named as a command\n'})
        self.assertEqual(self.needs(base=self.first, head=head),
                         {'code': 'true', 'compositor': 'true', 'boot': 'true', 'agent': 'false'})
        self.assertIn('code: compositor/src/a???add-mask??true\n', self.log)
        self.assertIn('code: ??add-mask??false\n', self.log)
        self.assertNotIn('::', self.log)
        self.assertNotIn('##[', self.log)

    def test_a_git_that_fails_fails_the_job(self):
        # With no list of files there is nothing to say no job is needed.
        with self.assertRaises(subprocess.CalledProcessError):
            self.needs(base=self.first, head='0' * 40)

    def test_a_push_with_no_commit_before_it_runs_every_job(self):
        self.assertEqual(self.needs(before='0' * 40, after=self.first),
                         {'code': 'true', 'compositor': 'true', 'boot': 'true', 'agent': 'true'})

    def test_the_weeks_run_runs_every_job(self):
        head = self.commit({'docs/VISION.md': 'The vision, amended\n'})
        self.assertEqual(self.needs(event='schedule', after=head),
                         {'code': 'true', 'compositor': 'true', 'boot': 'true', 'agent': 'true'})


# The old time the step gives a file, and a stand-in for prepare-gpui.py.
OLD = 946684800
PREPARE = 'from pathlib import Path\nPath("apps/shell/gpui-source").mkdir(exist_ok=True)\n' \
          'Path("apps/shell/gpui-source/lib.rs").write_text("GPUI\\n")\n'


class CachedBuildTimes(unittest.TestCase):
    """The boot job's step that lets Cargo reuse the build it restored."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.origin = Path(temporary.name) / 'origin'
        self.repo = Path(temporary.name) / 'repo'
        self.env = {'PATH': os.environ['PATH'], 'HOME': temporary.name, 'LC_ALL': 'C.UTF-8', **GIT_ENV}
        self.origin.mkdir()
        self.git(self.origin, 'init', '-q', '-b', 'main')
        self.built = self.commit({'compositor/src/main.rs': 'fn main() {}\n',
                                  'compositor/src/wire.rs': 'pub struct Wire;\n',
                                  'base/helpers/src/old.rs': 'pub fn old() {}\n',
                                  'apps/shell/packaging/prepare-gpui.py': PREPARE,
                                  'apps/shell/patches/still.patch': 'a patch\n'})

    def git(self, cwd, *args):
        return subprocess.run(['git', *args], cwd=cwd, env=self.env, check=True,
                              capture_output=True, text=True).stdout.strip()

    def commit(self, files=(), removed=()):
        for path, text in dict(files).items():
            (self.origin / path).parent.mkdir(parents=True, exist_ok=True)
            (self.origin / path).write_text(text)
        for path in removed:
            self.git(self.origin, 'rm', '-q', path)
        self.git(self.origin, 'add', '-A')
        self.git(self.origin, 'commit', '-q', '-m', 'a change')
        return self.git(self.origin, 'rev-parse', 'HEAD')

    def run_step(self, restored):
        """Checks out main's head as the runner does, then runs the step."""
        self.git(self.origin.parent, 'clone', '-q', '--depth=1', f'file://{self.origin}', str(self.repo))
        self.git(self.repo, 'remote', 'set-url', 'origin', f'file://{self.origin}')
        return subprocess.run(
            ['bash', '-e', '-c', step_script('Give the files the cached build was made from an old time')],
            cwd=self.repo, env=dict(self.env, RESTORED=restored), check=True,
            capture_output=True, text=True).stdout

    def mtime(self, path):
        return int((self.repo / path).stat().st_mtime)

    def test_only_what_changed_since_the_cached_build_looks_new(self):
        self.commit({'compositor/src/main.rs': 'fn main() { loop {} }\n'},
                    removed=['base/helpers/src/old.rs'])
        out = self.run_step(f'base-build-0123abcd-{self.built}')
        self.assertIn('Files changed since', out)
        self.assertEqual(self.mtime('compositor/src/wire.rs'), OLD)
        self.assertEqual(self.mtime('apps/shell/gpui-source/lib.rs'), OLD)
        self.assertGreater(self.mtime('compositor/src/main.rs'), OLD)
        self.assertFalse((self.repo / 'base/helpers/src/old.rs').exists())

    def test_a_changed_patch_leaves_gpui_looking_new(self):
        self.commit({'apps/shell/patches/still.patch': 'another patch\n'})
        self.run_step(f'base-build-0123abcd-{self.built}')
        self.assertGreater(self.mtime('apps/shell/gpui-source/lib.rs'), OLD)
        self.assertGreater(self.mtime('apps/shell/patches/still.patch'), OLD)
        self.assertEqual(self.mtime('compositor/src/main.rs'), OLD)

    def test_no_cached_build_leaves_every_file_new(self):
        self.commit({'compositor/src/main.rs': 'fn main() { loop {} }\n'})
        out = self.run_step('')
        self.assertIn('builds from the start', out)
        self.assertGreater(self.mtime('compositor/src/wire.rs'), OLD)

    def test_a_cached_build_from_a_commit_out_of_reach_leaves_every_file_new(self):
        self.commit({'compositor/src/main.rs': 'fn main() { loop {} }\n'})
        out = self.run_step('base-build-0123abcd-' + 'f' * 40)
        self.assertIn('builds from the start', out)
        self.assertGreater(self.mtime('compositor/src/wire.rs'), OLD)


if __name__ == '__main__':
    unittest.main()
