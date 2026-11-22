"""CI's `changes` job: which jobs a change needs. Each test runs the job's own
script, taken from ci.yml, on a small repository."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

WORKFLOW = Path(__file__).resolve().parent / 'ci.yml'


def classify_script():
    """The `run` script of the step that classifies the changed files."""
    lines = WORKFLOW.read_text().splitlines()
    step = next(i for i, line in enumerate(lines) if line.strip() == '- name: Classify the changed files')
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
        # Git as the runner has it: no configuration but its own defaults.
        # And the runner's locale, which the script must not count on.
        self.env = {'PATH': os.environ['PATH'], 'HOME': temporary.name, 'LC_ALL': 'C.UTF-8',
                    'GIT_CONFIG_GLOBAL': os.devnull, 'GIT_CONFIG_SYSTEM': os.devnull,
                    'GIT_AUTHOR_NAME': 'A test', 'GIT_AUTHOR_EMAIL': 'test@example.org',
                    'GIT_COMMITTER_NAME': 'A test', 'GIT_COMMITTER_EMAIL': 'test@example.org'}
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
        """What the job reports for a pull request (`base`, `head`) or a push
        (`before`, `after`)."""
        self.output.write_text('')
        env = dict(self.env, GITHUB_OUTPUT=str(self.output),
                   EVENT='pull_request' if 'head' in event else 'push',
                   PR_BASE=event.get('base', ''), PR_HEAD=event.get('head', ''),
                   BEFORE=event.get('before', ''), AFTER=event.get('after', ''))
        self.log = subprocess.run(
            ['bash', '--noprofile', '--norc', '-eo', 'pipefail', '-c', classify_script()],
            cwd=self.repo, env=env, check=True, capture_output=True, text=True).stdout
        return dict(line.split('=', 1) for line in self.output.read_text().splitlines())

    def test_docs_alone_need_no_job(self):
        head = self.commit({'docs/VISION.md': 'The vision, amended\n', 'README.md': 'CrowbarOS, again\n'})
        self.assertEqual(self.needs(base=self.first, head=head),
                         {'code': 'false', 'compositor': 'false', 'boot': 'false'})

    def test_a_lab_script_needs_the_code_jobs_alone(self):
        head = self.commit({'lab/void/guest.py': 'print("another guest")\n'})
        self.assertEqual(self.needs(base=self.first, head=head),
                         {'code': 'true', 'compositor': 'false', 'boot': 'false'})

    def test_the_compositor_needs_its_job_and_the_boot(self):
        head = self.commit({'compositor/src/main.rs': 'fn main() { loop {} }\n'})
        self.assertEqual(self.needs(base=self.first, head=head),
                         {'code': 'true', 'compositor': 'true', 'boot': 'true'})

    def test_code_moved_into_docs_is_still_a_change_to_the_code(self):
        # Git names a moved file by where it went. Read that way, Base losing
        # a file to docs/ was a change to docs alone, and no job ran.
        head = self.commit(moves=[('base/void_base.py', 'docs/void_base.py')])
        self.assertEqual(self.needs(base=self.first, head=head),
                         {'code': 'true', 'compositor': 'false', 'boot': 'true'})

    def test_a_push_that_moves_code_into_docs_is_read_the_same(self):
        after = self.commit(moves=[('compositor/src/main.rs', 'compositor/docs/main.rs')])
        self.assertEqual(self.needs(before=self.first, after=after),
                         {'code': 'true', 'compositor': 'true', 'boot': 'true'})

    def test_a_file_git_would_name_in_quotes_is_read_by_its_path(self):
        # Git puts a name with a letter outside ASCII, a quote, or a
        # backslash in quotes, and a path that starts with a quote is not
        # under compositor/ or base/.
        for name in ('café.rs', 'a"quote".rs', 'back\\slash.rs', 'a tab\there.rs'):
            with self.subTest(name=name):
                before = self.git('rev-parse', 'HEAD')
                added = self.commit({f'compositor/src/{name}': 'fn main() {}\n' * 20})
                self.assertEqual(self.needs(base=before, head=added),
                                 {'code': 'true', 'compositor': 'true', 'boot': 'true'})
                moved = self.commit(moves=[(f'compositor/src/{name}', f'docs/{name}.md')])
                self.assertEqual(self.needs(before=added, after=moved),
                                 {'code': 'true', 'compositor': 'true', 'boot': 'true'})

    def test_a_file_whose_name_is_not_text_is_read_by_its_path(self):
        # A name that ends partway through a UTF-8 character. Read as text,
        # the byte could take the NUL after it, and the file with it.
        head = self.commit({os.fsdecode(b'base/half-a-letter-\xc2'): 'print("Base")\n'})
        self.assertEqual(self.needs(base=self.first, head=head),
                         {'code': 'true', 'compositor': 'false', 'boot': 'true'})
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
                         {'code': 'true', 'compositor': 'true', 'boot': 'true'})
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
                         {'code': 'true', 'compositor': 'true', 'boot': 'true'})


if __name__ == '__main__':
    unittest.main()
