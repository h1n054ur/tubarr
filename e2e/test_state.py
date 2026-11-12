"""Exercise the real shell entry point with private state and argument-only Docker fakes.

No daemon, HTTP service, release build or external account is used. The copied
run.sh is unmodified. Only the release executable and API driver are stubs.
"""

import fcntl
import json
import os
from pathlib import Path
import re
import select
import shlex
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import unittest


SOURCE = Path(__file__).resolve().parent
LIMIT = 10
RUN_ID = "0123456789abcdef0123456789abcdef"
LABELS = ("io.tubarr.e2e", "io.tubarr.e2e.uid", "io.tubarr.e2e.run")

# This fake only records arguments and changes local JSON fixtures. It never
# forwards a command to Docker, starts an image or opens a network socket.
FAKE_DOCKER = r'''
import json, os, select, sys
from pathlib import Path

root = Path(os.environ["FAKE_DOCKER_ROOT"])
args = sys.argv[1:]
with (root / "calls.jsonl").open("a") as out:
    out.write(json.dumps(args) + "\n")
store = root / "resources.json"
resources = json.loads(store.read_text()) if store.exists() else {}

def save():
    tmp = root / ("resources." + str(os.getpid()))
    tmp.write_text(json.dumps(resources))
    os.replace(tmp, store)

def option(flag):
    return args[args.index(flag) + 1]

def labels():
    result = {}
    for i, item in enumerate(args):
        if item == "--label":
            key, value = args[i + 1].split("=", 1)
            result[key] = value
        elif item.startswith("--label="):
            key, value = item[8:].split("=", 1)
            result[key] = value
    return result

def barrier():
    if os.environ.get("FAKE_DOCKER_BLOCK") != "network":
        return
    try:
        fd = os.open(root / "claimed", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError:
        return
    os.close(fd)
    event = os.open(root / "event", os.O_WRONLY | os.O_NONBLOCK)
    release = os.open(root / "release", os.O_RDONLY | os.O_NONBLOCK)
    os.write(event, b"network-created\n")
    if not select.select([release], [], [], 8)[0]:
        sys.exit(97)
    os.read(release, 1)
    os.close(event)
    os.close(release)

is_network = args[:1] == ["network"]
command = args[1] if is_network or args[:1] == ["container"] else args[0]
if command == "create" and is_network:
    name = args[-1]
    resources[name] = {"Id": "id-" + name, "kind": "network", "Labels": labels()}
    save()
    barrier()
    print("id-" + name)
elif command == "run":
    name = option("--name")
    if os.environ.get("FAKE_DOCKER_FAIL_ROLE") and name.endswith("-" + os.environ["FAKE_DOCKER_FAIL_ROLE"]):
        sys.exit(42)
    resources[name] = {"Id": "id-" + name, "kind": "container", "Labels": labels()}
    save()
    for i, arg in enumerate(args):
        if arg in ("-v", "--volume"):
            binding = args[i + 1].split(":")
            if len(binding) > 1 and binding[1] == "/config":
                config = Path(binding[0])
                # A path is a test-created bind mount, never a host service path.
                if not config.is_relative_to(Path(os.environ["TEST_OWNED_ROOT"])):
                    sys.exit(96)
                config.mkdir(parents=True, exist_ok=True)
                (config / "config.xml").write_text("<Config><ApiKey>synthetic-test-key</ApiKey></Config>\n")
    print("id-" + name)
elif command == "inspect":
    if os.environ.get("TEST_WORKER_INFO") and Path(os.environ["TEST_WORKER_INFO"]).exists():
        info = json.loads(Path(os.environ["TEST_WORKER_INFO"]).read_text())
        # Observe reaping only, never use a PID to decide lifecycle ownership.
        with (root / "teardown.jsonl").open("a") as out:
            out.write(json.dumps({"worker_reaped": not Path("/proc", str(info["worker"])).exists()}) + "\n")
    name = args[-1]
    resource = resources.get(name)
    if resource is None:
        resource = next((x for x in resources.values() if x["Id"] == name), None)
    if os.environ.get("FAKE_DOCKER_INSPECT_ERROR"):
        print("Cannot connect to the Docker daemon", file=sys.stderr)
        sys.exit(1)
    if resource is None:
        message = ("Error response from daemon: network " + name + " not found"
                   if is_network else "Error: No such container: " + name)
        print(message, file=sys.stderr)
        sys.exit(1)
    inspected = {"Id": resource["Id"]}
    if is_network:
        inspected["Labels"] = resource["Labels"]
    else:
        inspected["Config"] = {"Labels": resource["Labels"]}
    print(json.dumps([inspected]))
elif command == "rm":
    if os.environ.get("FAKE_DOCKER_REMOVE_ERROR"):
        sys.exit(44)
    for name in list(resources):
        if name in args or resources[name]["Id"] in args:
            del resources[name]
    save()
elif command == "logs":
    print("synthetic log")
else:
    print("unexpected fake Docker command", file=sys.stderr)
    sys.exit(95)
'''


# The wrapper is a fixture-owned caller and Linux child subreaper. It lets the
# test reap an orphaned worker without making the test runner a subreaper.
# Its stdout is a control channel, never inherited by the harness or worker.
OWNED_CALLER = r'''
import ctypes, json, os, select, signal, subprocess, sys
from pathlib import Path

signal.alarm(60)
assert ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) == 0
root = Path(os.environ["TEST_OWNED_ROOT"])
sentinel_code = r"""
import os, signal, sys
def caught(sig, frame):
    with open(os.environ['TEST_CALLER_SIGNALS'], 'a') as out:
        out.write(str(sig) + '\n')
for sig in (signal.SIGINT, signal.SIGTERM):
    signal.signal(sig, caught)
print('ready', flush=True)
for line in sys.stdin:
    print('alive', flush=True)
"""
sentinel = subprocess.Popen([sys.executable, "-c", sentinel_code],
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
assert select.select([sentinel.stdout], [], [], 8)[0]
assert sentinel.stdout.readline() == "ready\n"
out = (root / "harness.stdout").open("wb")
err = (root / "harness.stderr").open("wb")
supervisor = subprocess.Popen(["bash", sys.argv[1], sys.argv[2]], stdout=out, stderr=err,
                             start_new_session=os.environ["TEST_NONLEADER"] != "1")
print(json.dumps({"supervisor": supervisor.pid, "caller_group": os.getpgrp(),
                  "sentinel": sentinel.pid}), flush=True)
try:
    for line in sys.stdin:
        command = json.loads(line)
        if command[0] == "wait":
            print(json.dumps({"status": supervisor.wait(timeout=10)}), flush=True)
        elif command[0] == "reap":
            pid, status = os.waitpid(command[1], 0)
            print(json.dumps({"reaped": pid, "status": os.waitstatus_to_exitcode(status)}), flush=True)
        elif command[0] == "sentinel":
            sentinel.stdin.write("ping\n")
            sentinel.stdin.flush()
            assert select.select([sentinel.stdout], [], [], 8)[0]
            print(json.dumps({"sentinel": sentinel.stdout.readline().strip()}), flush=True)
        elif command[0] == "exit":
            break
finally:
    # Only the explicitly spawned supervisor's own group may be separate.
    try:
        if os.getpgid(supervisor.pid) == supervisor.pid:
            os.killpg(supervisor.pid, signal.SIGKILL)
        else:
            supervisor.kill()
    except ProcessLookupError:
        pass
    supervisor.wait(timeout=10)
    sentinel.kill()
    sentinel.communicate(timeout=10)
    while True:
        try:
            os.waitpid(-1, 0)
        except ChildProcessError:
            break
    out.close()
    err.close()
'''


class LifecycleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="tubarr-state-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.home = self.root / "home"
        self.xdg = self.root / "xdg"
        self.tmp = self.root / "tmp"
        self.fake = self.root / "fake"
        self.repo = self.root / "repo"
        for folder in (self.home, self.xdg, self.tmp, self.fake, self.repo / "e2e"):
            folder.mkdir(parents=True, mode=0o700)
        for name in ("run.sh", "state.py"):
            if (SOURCE / name).exists():
                shutil.copy2(SOURCE / name, self.repo / "e2e" / name)
        binary = self.repo / "target/release/tubarr"
        binary.parent.mkdir(parents=True)
        binary.write_text("#!/bin/sh\nexit 0\n")
        binary.chmod(0o700)
        (self.repo / "e2e/drive.py").write_text('''
import json, os, select, signal, sys
from pathlib import Path
if os.environ.get("TEST_DRIVER_BLOCK"):
    if os.environ.get("TEST_DRIVER_SIGNAL_RECEIPT"):
        def received(sig, frame):
            Path(os.environ["TEST_DRIVER_SIGNAL_RECEIPT"]).write_text(json.dumps({
                "driver": os.getpid(), "signal": sig}))
            sys.exit(128 + sig)
        for sig in (signal.SIGINT, signal.SIGTERM):
            signal.signal(sig, received)
    if os.environ.get("TEST_WORKER_INFO"):
        Path(os.environ["TEST_WORKER_INFO"]).write_text(json.dumps({
            "driver": os.getpid(), "worker": os.getppid(), "group": os.getpgrp()}))
    event = os.open(os.environ["TEST_DRIVER_EVENT"], os.O_WRONLY | os.O_NONBLOCK)
    release = os.open(os.environ["TEST_DRIVER_RELEASE"], os.O_RDONLY | os.O_NONBLOCK)
    os.write(event, b"driver-running\\n")
    if not select.select([release], [], [], 8)[0]:
        sys.exit(97)
    os.read(release, 1)
    os.close(event)
    os.close(release)
sys.exit(int(os.environ.get("TEST_DRIVER_RC", "0")))
''')
        docker = self.fake / "docker"
        docker.write_text("#!" + sys.executable + "\n" + FAKE_DOCKER)
        docker.chmod(0o700)
        # Do not inherit service credentials, Docker contexts or operator options.
        self.env = {
            "PATH": str(self.fake) + ":" + os.defpath,
            "HOME": str(self.home),
            "XDG_STATE_HOME": str(self.xdg),
            "TMPDIR": str(self.tmp),
            "FAKE_DOCKER_ROOT": str(self.fake),
            "TEST_OWNED_ROOT": str(self.root),
            "PYTHONDONTWRITEBYTECODE": "1",
            "LANG": "C.UTF-8",
        }
        self.processes = []
        self.owned_groups = set()
        self.addCleanup(self.stop_processes)

    @property
    def app(self):
        return self.xdg / "tubarr/e2e"

    @property
    def state(self):
        return self.app / "state.json"

    def stop_processes(self):
        for proc in self.processes:
            if proc.stdin is not None and proc.poll() is None:
                try:
                    proc.stdin.write(b'["exit"]\n')
                    proc.stdin.flush()
                    proc.wait(timeout=LIMIT)
                except (BrokenPipeError, subprocess.TimeoutExpired):
                    pass
        for group in self.owned_groups:
            assert group > 1 and group != os.getpgrp()
            try:
                os.killpg(group, signal.SIGKILL)
            except ProcessLookupError:
                pass
        for proc in self.processes:
            # A dead supervisor may still have a child in its process group.
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            proc.communicate(timeout=LIMIT)

    def caller_reply(self, proc, command=None):
        if command is not None:
            proc.stdin.write((json.dumps(command) + "\n").encode())
            proc.stdin.flush()
        self.assertTrue(select.select([proc.stdout], [], [], LIMIT)[0], "owned caller deadline")
        line = proc.stdout.readline()
        self.assertTrue(line, "owned caller exited before its reply")
        return json.loads(line)

    def blocked_worker(self, nonleader=False):
        for name in ("driver-event", "driver-release"):
            os.mkfifo(self.root / name, 0o600)
        event = os.open(self.root / "driver-event", os.O_RDWR | os.O_NONBLOCK)
        release = os.open(self.root / "driver-release", os.O_RDWR | os.O_NONBLOCK)
        self.addCleanup(os.close, event)
        self.addCleanup(os.close, release)
        info_path = self.root / "worker.json"
        env = self.env | {
            "KEEP": "1", "TEST_DRIVER_BLOCK": "1", "TEST_NONLEADER": str(int(nonleader)),
            "TEST_DRIVER_EVENT": str(self.root / "driver-event"),
            "TEST_DRIVER_RELEASE": str(self.root / "driver-release"),
            "TEST_DRIVER_SIGNAL_RECEIPT": str(self.root / "driver-signal.json"),
            "TEST_WORKER_INFO": str(info_path),
            "TEST_CALLER_SIGNALS": str(self.root / "caller-signals"),
        }
        caller = subprocess.Popen(
            [sys.executable, "-c", OWNED_CALLER, str(self.repo / "e2e/run.sh"), str(self.root / "evidence")],
            cwd=self.repo, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, start_new_session=True,
        )
        self.processes.append(caller)
        identity = self.caller_reply(caller)
        self.assertEqual(identity["caller_group"], caller.pid)
        supervisor = identity["supervisor"]
        self.assertGreater(supervisor, 1)
        self.assertNotEqual(supervisor, os.getpid())
        # This group can only be established by this tracked owned child.
        self.owned_groups.add(supervisor)
        self.assertTrue(select.select([event], [], [], LIMIT)[0], "blocked worker not reached")
        self.assertEqual(os.read(event, 4096), b"driver-running\n")
        worker = json.loads(info_path.read_text())
        return caller, identity, worker, release

    def tree_snapshot(self, root):
        return {str(path.relative_to(root)): (path.lstat().st_mode, path.read_bytes() if path.is_file() else None)
                for path in root.rglob("*")}

    def launch(self, *args, extra=None, pass_fds=()):
        env = self.env | (extra or {})
        proc = subprocess.Popen(
            ["bash", str(self.repo / "e2e/run.sh"), *map(str, args)],
            cwd=self.repo, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            start_new_session=True,
            pass_fds=pass_fds,
        )
        self.processes.append(proc)
        return proc

    def finish(self, proc):
        out, err = proc.communicate(timeout=LIMIT)
        return subprocess.CompletedProcess(proc.args, proc.returncode, out, err)

    def run_cli(self, *args, extra=None):
        return self.finish(self.launch(*args, extra=extra))

    def calls(self):
        path = self.fake / "calls.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def resources(self):
        path = self.fake / "resources.json"
        return json.loads(path.read_text()) if path.exists() else {}

    def seed(self, data=None, mode=0o600):
        (self.xdg / "tubarr").mkdir(mode=0o700, exist_ok=True)
        self.app.mkdir(mode=0o700, exist_ok=True)
        (self.app / "runs").mkdir(mode=0o700, exist_ok=True)
        if data is None:
            data = {"version": 1, "uid": os.geteuid(), "run_id": RUN_ID}
        self.state.write_bytes(data if isinstance(data, bytes) else json.dumps(data).encode())
        self.state.chmod(mode)
        return self.state.read_bytes()

    def record(self):
        self.assertTrue(self.state.is_file(), "real start did not publish per-user JSON state")
        raw = self.state.read_bytes()
        self.assertLessEqual(len(raw), 4096)
        record = json.loads(raw)
        self.assertEqual(set(record), {"version", "uid", "run_id"})
        self.assertEqual(record["version"], 1)
        self.assertEqual(record["uid"], os.geteuid())
        self.assertRegex(record["run_id"], r"^[0-9a-f]{32}$")
        self.assertEqual(stat.S_IMODE(self.state.stat().st_mode), 0o600)
        for folder in (self.xdg / "tubarr", self.app, self.app / "runs"):
            self.assertEqual(stat.S_IMODE(folder.stat().st_mode), 0o700)
        return record

    def retained(self):
        result = self.run_cli(self.root / "evidence", extra={"KEEP": "1"})
        self.assertEqual(result.returncode, 0, result.stderr)
        return self.record()

    def barrier_start(self, extra=None):
        for name in ("event", "release"):
            os.mkfifo(self.fake / name, 0o600)
        event = os.open(self.fake / "event", os.O_RDWR | os.O_NONBLOCK)
        release = os.open(self.fake / "release", os.O_RDWR | os.O_NONBLOCK)
        self.addCleanup(os.close, event)
        self.addCleanup(os.close, release)
        proc = self.launch(self.root / "evidence", extra={"FAKE_DOCKER_BLOCK": "network"} | (extra or {}))
        self.assertTrue(select.select([event], [], [], LIMIT)[0], "network-create barrier not reached")
        self.assertEqual(os.read(event, 4096), b"network-created\n")
        return proc, release

    def assert_invalid_untouched(self, before):
        for args in (("down",), (self.root / "evidence",)):
            result = self.run_cli(*args)
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertEqual(self.state.read_bytes(), before)
            self.assertEqual(self.calls(), [])

    def test_empty_down_is_idempotent(self):
        self.assertEqual(self.run_cli("down").returncode, 0)
        self.assertEqual(self.run_cli("down").returncode, 0)
        self.assertEqual(self.calls(), [])

    def test_extra_arguments_and_direct_worker_entry_are_rejected(self):
        for args in (("down", "extra"), (self.root / "evidence", "extra"), ("--lifecycle-worker",)):
            with self.subTest(args=args):
                result = self.run_cli(*args)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertEqual(self.calls(), [])

    def test_driver_exit_status_and_normal_cleanup_are_preserved(self):
        result = self.run_cli(self.root / "evidence", extra={"TEST_DRIVER_RC": "23"})
        self.assertEqual(result.returncode, 23, result.stderr)
        self.assertEqual(self.resources(), {})
        self.assertFalse(self.state.exists())

    def test_success_without_keep_cleans_own_run(self):
        self.assertEqual(self.run_cli(self.root / "evidence").returncode, 0)
        self.assertEqual(self.resources(), {})
        self.assertFalse(self.state.exists())

    def test_isolated_legacy_source_proof(self):
        script = (self.repo / "e2e/run.sh").read_text()
        match = re.search(r"(?m)^down\(\) \{\n.*?^\}", script, re.S)
        if '. "$state_file"' not in script:
            self.assertNotRegex(script, r"(?m)^\s*(?:source|\.)\s+.*state")
            return
        self.assertIsNotNone(match)
        sentinel = self.root / "harmless-source-proof"
        victim = self.root / "legacy-owned-work"
        victim.mkdir()
        legacy = self.tmp / "tubarr-e2e.state"
        legacy.write_text("touch " + shlex.quote(str(sentinel)) + "\nwork=" + shlex.quote(str(victim)) + "\n")
        snippet = "set -eu\nstate_file=" + shlex.quote(str(legacy)) + "\nnet=synthetic\n" + match[0] + "\ndown\n"
        result = subprocess.run(["bash", "-c", snippet], env=self.env, capture_output=True, timeout=LIMIT)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(sentinel.exists(), "old sourced state should demonstrate harmless execution")
        self.assertFalse(victim.exists())

    def test_legacy_state_is_never_executed_or_removed(self):
        for command in ("down", "start"):
            with self.subTest(command=command):
                sentinel = self.root / ("executed-" + command)
                victim = self.root / ("victim-" + command)
                victim.mkdir()
                legacy = self.tmp / "tubarr-e2e.state"
                payload = "touch " + shlex.quote(str(sentinel)) + "\nwork=" + shlex.quote(str(victim)) + "\n"
                legacy.write_text(payload)
                result = self.run_cli("down" if command == "down" else self.root / "evidence")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertFalse(sentinel.exists(), "mutable legacy state was executed")
                self.assertTrue(victim.is_dir(), "legacy work path was followed")
                self.assertEqual(legacy.read_text(), payload)

    def test_malformed_oversized_and_non_strict_json_are_rejected(self):
        good = {"version": 1, "uid": os.geteuid(), "run_id": RUN_ID}
        cases = [b"{", b" " * 4097, b"\xff", b"[]", b"null",
                 b'{"version":NaN}', b'{"version":Infinity}',
                 json.dumps(good).encode() + b" null",
                 ('{"version":1,"version":1,"uid":%d,"run_id":"%s"}' % (os.geteuid(), RUN_ID)).encode()]
        cases += [json.dumps(good | delta).encode() for delta in (
            {"version": True}, {"uid": True}, {"uid": os.geteuid() + 1},
            {"version": 1.0}, {"uid": float(os.geteuid())},
            {"version": 2}, {"run_id": "../victim"}, {"run_id": "A" * 32},
            {"run_id": "a" * 33}, {"work": str(self.root / "victim")},
        )]
        for case in cases:
            with self.subTest(case=case[:100]):
                self.assert_invalid_untouched(self.seed(case))

    def test_state_shell_text_never_executes(self):
        sentinel = self.root / "new-state-executed"
        before = self.seed(("touch " + shlex.quote(str(sentinel)) + "\n").encode())
        self.assert_invalid_untouched(before)
        self.assertFalse(sentinel.exists())

    def test_arbitrary_cleanup_path_is_rejected(self):
        victim = self.root / "outside-app-owned-victim"
        victim.mkdir()
        (victim / "keep").write_text("keep")
        before = self.seed({"version": 1, "uid": os.geteuid(), "run_id": RUN_ID, "work": str(victim)})
        self.assert_invalid_untouched(before)
        self.assertEqual((victim / "keep").read_text(), "keep")

    def test_world_readable_or_writable_state_rejected(self):
        for mode in (0o644, 0o666):
            with self.subTest(mode=oct(mode)):
                self.assert_invalid_untouched(self.seed(mode=mode))

    def test_symlink_state_rejected_without_following(self):
        before = self.seed()
        target = self.root / "state-target"
        self.state.rename(target)
        self.state.symlink_to(target)
        self.assert_invalid_untouched(before)
        self.assertTrue(self.state.is_symlink())

    def test_fifo_state_rejected_without_blocking_parser(self):
        self.seed()
        self.state.unlink()
        os.mkfifo(self.state, 0o600)
        for args in (("down",), (self.root / "evidence",)):
            result = self.run_cli(*args)
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertTrue(stat.S_ISFIFO(self.state.lstat().st_mode))
            self.assertEqual(self.calls(), [])

    def test_hardlinked_state_rejected(self):
        before = self.seed()
        os.link(self.state, self.root / "linked-state")
        self.assert_invalid_untouched(before)

    def test_unsafe_application_directory_rejected(self):
        before = self.seed()
        self.app.chmod(0o777)
        self.assert_invalid_untouched(before)
        self.assertEqual(stat.S_IMODE(self.app.stat().st_mode), 0o777)

    def test_unsafe_lock_rejected(self):
        before = self.seed()
        lock = self.app / "lifecycle.lock"
        lock.write_text("")
        lock.chmod(0o666)
        self.assert_invalid_untouched(before)

    def test_symlink_lock_rejected(self):
        before = self.seed()
        target = self.root / "lock-target"
        target.write_text("do not change")
        target.chmod(0o600)
        (self.app / "lifecycle.lock").symlink_to(target)
        self.assert_invalid_untouched(before)
        self.assertEqual(target.read_text(), "do not change")

    def test_existing_kernel_lock_rejects_down_without_reading_state(self):
        self.seed(b"not-json-and-must-not-be-read-while-busy")
        lock = self.app / "lifecycle.lock"
        fd = os.open(lock, os.O_RDWR | os.O_CREAT | os.O_EXCL, 0o600)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            result = self.run_cli("down")
            self.assertEqual(result.returncode, 75, result.stderr)
            self.assertEqual(self.calls(), [])
            self.assertEqual(self.state.read_bytes(), b"not-json-and-must-not-be-read-while-busy")
        finally:
            os.close(fd)

    def test_symlink_state_home_ancestor_rejected(self):
        before = self.seed()
        real = self.root / "real-xdg"
        self.xdg.rename(real)
        self.xdg.symlink_to(real, target_is_directory=True)
        self.assert_invalid_untouched(before)

    def test_valid_maximum_length_state_can_be_recovered(self):
        before = self.seed()
        self.state.write_bytes(before + b" " * (4096 - len(before)))
        self.assertEqual(self.run_cli("down").returncode, 0)
        self.assertFalse(self.state.exists(), "valid bounded state was not cleaned")

    def test_state_publication_is_atomic_and_private(self):
        site = self.root / "publication-observer"
        site.mkdir()
        # Observe the real os.replace call without replacing its effect. The
        # lifecycle still runs through the actual shell and actual helper.
        (site / "sitecustomize.py").write_text(r'''
import json, os, select, stat
from pathlib import Path
original = os.replace
def observed(src, dst, *args, **kwargs):
    if Path(dst).name == "state.json":
        fd = os.open(src, os.O_RDONLY, dir_fd=kwargs.get("src_dir_fd"))
        info = os.fstat(fd)
        assert stat.S_ISREG(info.st_mode) and stat.S_IMODE(info.st_mode) == 0o600
        assert info.st_uid == os.geteuid() and info.st_nlink == 1
        raw = os.read(fd, 4097)
        os.close(fd)
        assert len(raw) <= 4096 and set(json.loads(raw)) == {"version", "uid", "run_id"}
        event = os.open(os.environ["TEST_PUBLISH_EVENT"], os.O_WRONLY | os.O_NONBLOCK)
        release = os.open(os.environ["TEST_PUBLISH_RELEASE"], os.O_RDONLY | os.O_NONBLOCK)
        os.write(event, b"complete-private-temp\n")
        assert select.select([release], [], [], 8)[0], "publication barrier expired"
        os.read(release, 1)
        os.close(event)
        os.close(release)
    return original(src, dst, *args, **kwargs)
os.replace = observed
''')
        for name in ("publish-event", "publish-release"):
            os.mkfifo(self.root / name, 0o600)
        event = os.open(self.root / "publish-event", os.O_RDWR | os.O_NONBLOCK)
        release = os.open(self.root / "publish-release", os.O_RDWR | os.O_NONBLOCK)
        self.addCleanup(os.close, event)
        self.addCleanup(os.close, release)
        proc = self.launch(self.root / "evidence", extra={
            "KEEP": "1", "PYTHONPATH": str(site),
            "TEST_PUBLISH_EVENT": str(self.root / "publish-event"),
            "TEST_PUBLISH_RELEASE": str(self.root / "publish-release"),
        })
        ready = select.select([event, proc.stdout], [], [], LIMIT)[0]
        self.assertIn(event, ready, "start never used atomic state publication")
        self.assertEqual(os.read(event, 4096), b"complete-private-temp\n")
        self.assertFalse(self.state.exists(), "state was visible before atomic replacement")
        self.assertEqual(self.resources(), {}, "Docker ran before state publication")
        os.write(release, b"x")
        self.assertEqual(self.finish(proc).returncode, 0)
        self.record()
        self.assertEqual(self.run_cli("down").returncode, 0)

    def test_symlink_application_directory_rejected(self):
        self.seed()
        target = self.root / "real-app"
        self.app.rename(target)
        self.app.symlink_to(target, target_is_directory=True)
        self.assert_invalid_untouched((target / "state.json").read_bytes())

    def test_foreign_filesystem_owner_rejected_without_chown(self):
        before = self.seed()
        site = self.root / "identity-fixture"
        site.mkdir()
        (site / "sitecustomize.py").write_text(
            "import os\nos.geteuid = os.getuid = lambda: " + str(os.geteuid() + 1) + "\n"
        )
        result = self.run_cli("down", extra={"PYTHONPATH": str(site)})
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(self.state.read_bytes(), before)
        self.assertEqual(self.calls(), [])

    def test_xdg_default_and_relative_fallback(self):
        for xdg in ("", "relative-not-allowed"):
            with self.subTest(xdg=xdg):
                result = self.run_cli(self.root / "evidence", extra={"KEEP": "1", "XDG_STATE_HOME": xdg})
                self.assertEqual(result.returncode, 0, result.stderr)
                default = self.home / ".local/state/tubarr/e2e/state.json"
                self.assertTrue(default.is_file(), "default HOME state was not published")
                self.assertFalse((self.repo / xdg / "tubarr/e2e/state.json").is_file())
                self.assertEqual(self.run_cli("down", extra={"XDG_STATE_HOME": xdg}).returncode, 0)

    def test_keep_then_down_is_owned_and_idempotent(self):
        record = self.retained()
        run_id = record["run_id"]
        prefix = "tubarr-e2e-u%d-%s-" % (os.geteuid(), run_id)
        resources = self.resources()
        self.assertEqual(set(resources), {prefix + role for role in ("net", "sonarr", "radarr", "tubarr")})
        for resource in resources.values():
            self.assertEqual(resource["Labels"], dict(zip(LABELS, ("1", str(os.geteuid()), run_id))))
        work = self.app / "runs" / run_id
        self.assertTrue(work.is_dir())
        self.assertEqual(stat.S_IMODE(work.stat().st_mode), 0o700)
        lock = self.app / "lifecycle.lock"
        lock_inode = lock.stat().st_ino
        before = self.state.read_bytes()
        call_count = len(self.calls())
        busy = self.run_cli(self.root / "second-evidence")
        self.assertEqual(busy.returncode, 75, busy.stderr)
        self.assertIn(b"busy", busy.stderr.lower() + busy.stdout.lower())
        self.assertEqual(self.calls()[call_count:], [])
        self.assertEqual(self.state.read_bytes(), before)
        self.assertEqual(self.resources(), resources)
        self.assertEqual(self.run_cli("down").returncode, 0)
        self.assertFalse(work.exists())
        self.assertFalse(self.state.exists())
        self.assertEqual(self.resources(), {})
        self.assertEqual(self.run_cli("down").returncode, 0)
        # The lock inode must survive teardown, avoiding split-lock races.
        self.assertTrue(lock.is_file())
        self.assertEqual(lock.stat().st_ino, lock_inode)
        self.assertEqual(stat.S_IMODE(lock.stat().st_mode), 0o600)

    def test_overlap_start_and_down_reject_without_teardown(self):
        proc, release = self.barrier_start(extra={"KEEP": "1"})
        before = self.resources()
        self.assertEqual(len(before), 1)
        for args in ((self.root / "other-evidence",), ("down",)):
            with self.subTest(args=args):
                call_count = len(self.calls())
                result = self.run_cli(*args)
                self.assertEqual(result.returncode, 75, result.stderr)
                self.assertEqual(self.resources(), before)
                self.assertEqual(self.calls()[call_count:], [])
        os.write(release, b"x")
        self.assertEqual(self.finish(proc).returncode, 0)
        self.assertEqual(self.run_cli("down").returncode, 0)

    def test_driver_phase_keeps_exclusive_lifecycle_lock(self):
        for name in ("driver-event", "driver-release"):
            os.mkfifo(self.root / name, 0o600)
        event = os.open(self.root / "driver-event", os.O_RDWR | os.O_NONBLOCK)
        release = os.open(self.root / "driver-release", os.O_RDWR | os.O_NONBLOCK)
        self.addCleanup(os.close, event)
        self.addCleanup(os.close, release)
        proc = self.launch(self.root / "evidence", extra={
            "TEST_DRIVER_BLOCK": "1", "TEST_DRIVER_EVENT": str(self.root / "driver-event"),
            "TEST_DRIVER_RELEASE": str(self.root / "driver-release"),
        })
        self.assertTrue(select.select([event], [], [], LIMIT)[0], "driver barrier not reached")
        self.assertEqual(os.read(event, 4096), b"driver-running\n")
        before = self.resources()
        self.assertEqual(len(before), 4)
        for args in ((self.root / "other-evidence",), ("down",)):
            with self.subTest(args=args):
                count = len(self.calls())
                result = self.run_cli(*args)
                self.assertEqual(result.returncode, 75, result.stderr)
                self.assertEqual(self.resources(), before)
                self.assertEqual(self.calls()[count:], [])
        os.write(release, b"x")
        self.assertEqual(self.finish(proc).returncode, 0)
        self.assertEqual(self.resources(), {})
        self.assertFalse(self.state.exists())

    def test_wrong_resource_owner_or_run_never_deleted(self):
        record = self.retained()
        original = self.resources()
        work = self.app / "runs" / record["run_id"]
        work_before = self.tree_snapshot(work)
        for name in original:
            for label, value in ((LABELS[1], str(os.geteuid() + 1)), (LABELS[2], "f" * 32), (LABELS[0], "0")):
                with self.subTest(resource=name, label=label):
                    resources = json.loads(json.dumps(original))
                    resources[name]["Labels"][label] = value
                    (self.fake / "resources.json").write_text(json.dumps(resources))
                    before = self.state.read_bytes()
                    count = len(self.calls())
                    result = self.run_cli("down")
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(self.resources(), resources)
                    self.assertEqual(self.state.read_bytes(), before)
                    self.assertTrue(work.is_dir())
                    self.assertEqual(self.tree_snapshot(work), work_before)
                    calls = self.calls()[count:]
                    self.assertFalse(any("rm" in call for call in calls))
                    inspections = [call for call in calls if "inspect" in call]
                    self.assertCountEqual(inspections, [
                        [resource["kind"], "inspect", key] for key, resource in original.items()])
        (self.fake / "resources.json").write_text(json.dumps(original))
        self.assertEqual(self.run_cli("down").returncode, 0)

    def test_cleanup_uses_inspected_ids_and_ignores_unrelated_resources(self):
        self.retained()
        owned = self.resources()
        resources = owned | {"unrelated-owned-test-fixture": {"Id": "id-unrelated", "kind": "container", "Labels": {}}}
        (self.fake / "resources.json").write_text(json.dumps(resources))
        count = len(self.calls())
        self.assertEqual(self.run_cli("down").returncode, 0)
        self.assertEqual(self.resources(), {"unrelated-owned-test-fixture": resources["unrelated-owned-test-fixture"]})
        calls = self.calls()[count:]
        removals = [call for call in calls if "rm" in call]
        removed_ids = {arg for call in removals for arg in call if arg.startswith("id-")}
        self.assertEqual(removed_ids, {resource["Id"] for resource in owned.values()})
        self.assertCountEqual(
            [arg for call in removals for arg in call[call.index("rm") + 1:] if not arg.startswith("-")],
            [resource["Id"] for resource in owned.values()])
        first_remove = next(i for i, call in enumerate(calls) if "rm" in call)
        self.assertCountEqual([call for call in calls[:first_remove] if "inspect" in call], [
            [resource["kind"], "inspect", name] for name, resource in owned.items()])
        network_index = next(i for i, call in enumerate(calls) if call[:2] == ["network", "rm"])
        for resource in owned.values():
            remove_index = next(i for i, call in enumerate(calls) if "rm" in call and resource["Id"] in call)
            if resource["kind"] == "container":
                self.assertLess(remove_index, network_index)
            else:
                self.assertEqual(remove_index, network_index)

    def test_cleanup_error_keeps_recoverable_state(self):
        self.retained()
        before = self.state.read_bytes()
        for option in ("FAKE_DOCKER_INSPECT_ERROR", "FAKE_DOCKER_REMOVE_ERROR"):
            with self.subTest(option=option):
                result = self.run_cli("down", extra={option: "1"})
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.state.read_bytes(), before)
        self.assertEqual(self.run_cli("down").returncode, 0)

    def test_work_symlink_rejected_without_following(self):
        record = self.retained()
        work = self.app / "runs" / record["run_id"]
        work.rename(self.root / "parked-owned-work")
        victim = self.root / "work-link-victim"
        victim.mkdir()
        (victim / "keep").write_text("keep")
        work.symlink_to(victim, target_is_directory=True)
        self.assertNotEqual(self.run_cli("down").returncode, 0)
        self.assertEqual((victim / "keep").read_text(), "keep")
        self.assertTrue(self.state.exists())

    def test_missing_release_executable_cleans_and_allows_recovery(self):
        (self.repo / "target/release/tubarr").unlink()
        result = self.run_cli(self.root / "evidence", extra={"KEEP": "1"})
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(b"build first: cargo build --release", result.stderr)
        state_after_setup = self.state.exists()
        work_after_setup = list((self.app / "runs").iterdir())
        recovery = self.run_cli("down")
        self.assertEqual(recovery.returncode, 0, recovery.stderr)
        self.assertFalse(state_after_setup, "missing executable left retained state")
        self.assertEqual(work_after_setup, [], "missing executable left run work")
        self.assertEqual(self.resources(), {})
        self.assertFalse(self.state.exists())
        self.assertEqual(list((self.app / "runs").iterdir()), [])
        self.assertEqual(self.run_cli("down").returncode, 0)

    def test_partial_setup_failure_cleans_even_with_keep(self):
        result = self.run_cli(self.root / "evidence", extra={"KEEP": "1", "FAKE_DOCKER_FAIL_ROLE": "radarr"})
        self.assertEqual(result.returncode, 42, result.stderr)
        self.assertEqual(self.resources(), {}, "partial setup leaked fake Docker resources")
        self.assertFalse(self.state.exists())
        self.assertEqual(self.run_cli("down").returncode, 0)

    def interrupted(self, sig):
        proc, release = self.barrier_start(extra={"KEEP": "1"})
        os.killpg(proc.pid, sig)
        result = self.finish(proc)
        self.assertEqual(result.returncode, 128 + sig, result.stderr)
        self.assertEqual(self.resources(), {}, "catchable interruption leaked resources")
        self.assertFalse(self.state.exists())
        self.assertEqual(self.run_cli("down").returncode, 0)

    def test_term_cleanup_then_down(self):
        self.interrupted(signal.SIGTERM)

    def test_int_cleanup_then_down(self):
        self.interrupted(signal.SIGINT)

    def test_sigkill_leaves_state_for_explicit_down(self):
        proc, release = self.barrier_start()
        os.killpg(proc.pid, signal.SIGKILL)
        self.finish(proc)
        self.record()
        before = self.resources()
        self.assertTrue(before)
        count = len(self.calls())
        self.assertEqual(self.run_cli(self.root / "another-evidence").returncode, 75)
        self.assertEqual(self.calls()[count:], [])
        self.assertEqual(self.resources(), before)
        self.assertEqual(self.run_cli("down").returncode, 0)
        self.assertEqual(self.resources(), {})
        self.assertFalse(self.state.exists())

    def supervisor_only_signal(self, sig, nonleader=False):
        caller, identity, worker, release = self.blocked_worker(nonleader=nonleader)
        supervisor = identity["supervisor"]
        self.assertNotEqual(worker["worker"], supervisor, "entry did not supervise a separate worker")
        self.assertEqual(worker["group"], supervisor, "supervisor did not establish its own group")
        self.assertNotEqual(worker["group"], identity["caller_group"])
        record = self.record()
        self.assertEqual(len(self.resources()), 4)
        os.kill(supervisor, sig)
        reply = self.caller_reply(caller, ["wait"])
        self.assertEqual(reply["status"], 128 + sig)
        # The bounded supervisor wait includes the driver's receipt. Its
        # watchdog exit alone cannot prove that signal forwarding happened.
        receipt = self.root / "driver-signal.json"
        self.assertTrue(receipt.is_file(), "blocked driver did not receive the signal")
        self.assertEqual(json.loads(receipt.read_text()), {"driver": worker["driver"], "signal": sig})
        self.assertEqual(self.resources(), {})
        self.assertFalse(self.state.exists())
        self.assertFalse((self.app / "runs" / record["run_id"]).exists())
        observations = self.fake / "teardown.jsonl"
        self.assertTrue(observations.is_file(), "no cleanup preflight observed")
        audited = [json.loads(line) for line in observations.read_text().splitlines()]
        self.assertEqual(len(audited), 4)
        self.assertTrue(all(item["worker_reaped"] for item in audited), "teardown preceded worker reap")
        self.assertEqual(self.caller_reply(caller, ["sentinel"]), {"sentinel": "alive"})
        self.assertFalse((self.root / "caller-signals").exists(), "caller sentinel received a signal")
        self.assertEqual(self.run_cli("down").returncode, 0)

    def test_supervisor_pid_only_int_reaps_worker_before_cleanup(self):
        self.supervisor_only_signal(signal.SIGINT)

    def test_supervisor_pid_only_term_reaps_worker_before_cleanup(self):
        self.supervisor_only_signal(signal.SIGTERM)

    def test_nonleader_supervisor_isolates_group_and_preserves_caller(self):
        self.supervisor_only_signal(signal.SIGTERM, nonleader=True)

    def test_supervisor_pid_only_sigkill_worker_holds_lock_until_reaped(self):
        caller, identity, worker, release = self.blocked_worker()
        self.assertNotEqual(worker["worker"], identity["supervisor"])
        self.assertEqual(worker["group"], identity["supervisor"])
        self.record()
        state_before = self.state.read_bytes()
        resources_before = self.resources()
        work_before = self.tree_snapshot(self.app / "runs")
        os.kill(identity["supervisor"], signal.SIGKILL)
        # Only wait for the supervisor, never for inherited harness pipe EOF.
        self.assertEqual(self.caller_reply(caller, ["wait"]), {"status": -signal.SIGKILL})
        for args in ((self.root / "second-evidence",), ("down",)):
            with self.subTest(args=args):
                count = len(self.calls())
                result = self.run_cli(*args)
                self.assertEqual(result.returncode, 75, result.stderr)
                self.assertIn(b"busy", result.stdout.lower() + result.stderr.lower())
                self.assertEqual(self.calls()[count:], [])
                self.assertEqual(self.resources(), resources_before)
                self.assertEqual(self.state.read_bytes(), state_before)
                self.assertEqual(self.tree_snapshot(self.app / "runs"), work_before)
        os.write(release, b"x")
        self.assertEqual(self.caller_reply(caller, ["reap", worker["worker"]]),
                         {"reaped": worker["worker"], "status": 0})
        self.assertEqual(self.resources(), resources_before)
        self.assertEqual(self.state.read_bytes(), state_before)
        self.assertEqual(self.run_cli("down").returncode, 0)
        self.assertEqual(self.resources(), {})
        self.assertFalse(self.state.exists())
        self.assertEqual(list((self.app / "runs").iterdir()), [])

    def guarded_worker_rejected(self, corruption):
        self.seed()
        work = self.app / "runs" / RUN_ID
        work.mkdir(mode=0o700)
        lock = os.open(self.app / "lifecycle.lock", os.O_RDWR | os.O_CREAT | os.O_EXCL, 0o600)
        wrong = os.open(self.root / "wrong-lock", os.O_RDWR | os.O_CREAT | os.O_EXCL, 0o600)
        read_fd, driver_fd = os.pipe()
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            prefix = "tubarr-e2e-u%d-%s-" % (os.geteuid(), RUN_ID)
            env = {"E2E_WORK": str(work), "E2E_RUN_ID": RUN_ID,
                   "E2E_OWNER_UID": str(os.geteuid()), "E2E_NET": prefix + "net",
                   "E2E_SONARR_NAME": prefix + "sonarr", "E2E_RADARR_NAME": prefix + "radarr",
                   "E2E_TUBARR_NAME": prefix + "tubarr", "E2E_LOCK_FD": str(lock),
                   "E2E_DRIVER_FD": str(driver_fd)}
            if corruption == "wrong-lock-inode":
                env["E2E_LOCK_FD"] = str(wrong)
            elif corruption == "non-pipe-driver":
                env["E2E_DRIVER_FD"] = str(wrong)
            else:
                env["E2E_NET"] = prefix + "different"
            before = self.tree_snapshot(self.app)
            proc = self.launch("--lifecycle-worker", self.root / "evidence", extra=env,
                               pass_fds=(lock, wrong, driver_fd))
            result = self.finish(proc)
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertEqual(self.calls(), [], "invalid worker reached Docker")
            self.assertEqual(self.tree_snapshot(self.app), before)
        finally:
            for fd in (lock, wrong, read_fd, driver_fd):
                os.close(fd)

    def test_guarded_worker_rejects_wrong_lock_inode(self):
        self.guarded_worker_rejected("wrong-lock-inode")

    def test_guarded_worker_rejects_non_pipe_driver_descriptor(self):
        self.guarded_worker_rejected("non-pipe-driver")

    def test_guarded_worker_rejects_mismatched_derived_value(self):
        self.guarded_worker_rejected("mismatched-derived-value")

    def test_keep_driver_failure_is_recoverable(self):
        result = self.run_cli(self.root / "evidence", extra={"KEEP": "1", "TEST_DRIVER_RC": "23"})
        self.assertEqual(result.returncode, 23, result.stderr)
        self.record()
        self.assertEqual(len(self.resources()), 4)
        self.assertEqual(self.run_cli("down").returncode, 0)


if __name__ == "__main__":
    unittest.main()
