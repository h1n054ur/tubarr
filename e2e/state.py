#!/usr/bin/env python3
"""Private state and owned lifecycle for the real-service E2E harness."""

from contextlib import ExitStack
import fcntl
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import signal
import stat
import subprocess
import sys
import tempfile


DIRECTORY = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
FILE_FLAGS = os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC
ROLES = ("sonarr", "radarr", "tubarr", "net")
DRIVER_STARTED = b"driver-started\n"
SCRIPT = Path(__file__).resolve().with_name("run.sh")


class LifecycleError(Exception):
    def __init__(self, message, status=2):
        super().__init__(message)
        self.status = status


def diagnostic(message):
    print(f"e2e: {message}", file=sys.stderr)


def private_directory(fd, uid, mode=0o700):
    info = os.fstat(fd)
    if (not stat.S_ISDIR(info.st_mode) or info.st_uid != uid
            or stat.S_IMODE(info.st_mode) != mode):
        raise LifecycleError("unsafe application directory ownership or mode")


def private_file(info, uid):
    if (not stat.S_ISREG(info.st_mode) or info.st_uid != uid
            or stat.S_IMODE(info.st_mode) != 0o600 or info.st_nlink != 1):
        raise LifecycleError("unsafe lifecycle file type, ownership, mode or links")


def object_without_duplicates(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate key")
        result[key] = value
    return result


def reject_constant(value):
    raise ValueError("nonstandard JSON number")


class Context:
    def __init__(self, stack, create=True):
        self.stack = stack
        self.uid = os.geteuid()
        self.published = False
        state_home = os.environ.get("XDG_STATE_HOME", "")
        if not state_home or not os.path.isabs(state_home):
            home = os.environ.get("HOME", "")
            if not home or not os.path.isabs(home):
                raise LifecycleError("HOME must be absolute when XDG_STATE_HOME is unset or relative")
            state_home = home + "/.local/state"

        # Walk even generic ancestors without following links. Their usual
        # permissions are allowed, but the state home must belong to this UID.
        fd = self.hold(os.open("/", DIRECTORY))
        for part in state_home.split("/"):
            if not part or part == ".":
                continue
            fd = self.directory(fd, part, create=create, private=False)
        info = os.fstat(fd)
        if info.st_uid != self.uid or stat.S_IMODE(info.st_mode) & 0o022:
            raise LifecycleError("state home must be owned and not group/world writable")
        tubarr = self.directory(fd, "tubarr", create=create)
        self.root = self.directory(tubarr, "e2e", create=create)
        self.path = os.path.normpath(state_home + "/tubarr/e2e")

    def hold(self, fd):
        self.stack.callback(os.close, fd)
        return fd

    def directory(self, parent, name, create=False, private=True):
        if create:
            try:
                os.mkdir(name, 0o700, dir_fd=parent)
            except FileExistsError:
                pass
        fd = self.hold(os.open(name, DIRECTORY, dir_fd=parent))
        if private:
            private_directory(fd, self.uid)
        return fd

    def file(self, name, flags, create=False):
        try:
            private_file(os.stat(name, dir_fd=self.root, follow_symlinks=False), self.uid)
        except FileNotFoundError:
            if not create:
                raise
        fd = os.open(name, flags | FILE_FLAGS | (os.O_CREAT if create else 0),
                     0o600, dir_fd=self.root)
        try:
            private_file(os.fstat(fd), self.uid)
        except BaseException:
            os.close(fd)
            raise
        return fd

    def lock(self):
        self.lock_fd = self.hold(self.file("lifecycle.lock", os.O_RDWR, create=True))
        try:
            fcntl.flock(self.lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise LifecycleError("busy: another E2E lifecycle is active", 75) from None
        # Closing, not LOCK_UN, keeps the inherited worker's lock alive if its
        # supervisor exits. The persistent lock inode is never removed.

    def read_state(self):
        try:
            fd = self.file("state.json", os.O_RDONLY)
        except FileNotFoundError:
            return None
        try:
            raw = bytearray()
            while len(raw) <= 4096:
                chunk = os.read(fd, 4097 - len(raw))
                if not chunk:
                    break
                raw.extend(chunk)
            if len(raw) > 4096:
                raise LifecycleError("state exceeds 4096 bytes")
        finally:
            os.close(fd)
        try:
            record = json.loads(raw.decode("utf-8"),
                                object_pairs_hook=object_without_duplicates,
                                parse_constant=reject_constant)
        except (UnicodeError, ValueError, RecursionError):
            raise LifecycleError("state must be strict UTF-8 JSON") from None
        if (type(record) is not dict or set(record) != {"version", "uid", "run_id"}
                or type(record["version"]) is not int or record["version"] != 1
                or type(record["uid"]) is not int or record["uid"] != self.uid
                or type(record["run_id"]) is not str
                or re.fullmatch(r"[0-9a-f]{32}", record["run_id"]) is None):
            raise LifecycleError("invalid state schema, version, UID or run ID")
        return record

    def publish(self, record):
        raw = (json.dumps(record, separators=(",", ":")) + "\n").encode("utf-8")
        # mkstemp has no dir_fd argument. Use the already-open root as the cwd
        # only during publication, before any worker exists.
        cwd = os.open(".", DIRECTORY)
        temporary = None
        try:
            os.fchdir(self.root)
            fd, temporary = tempfile.mkstemp(prefix=".state-", dir=".")
            with os.fdopen(fd, "wb") as out:
                os.fchmod(out.fileno(), 0o600)
                out.write(raw)
                out.flush()
                os.fsync(out.fileno())
            os.replace(temporary, "state.json", src_dir_fd=self.root, dst_dir_fd=self.root)
            self.published = True
            temporary = None
            os.fsync(self.root)
        finally:
            if temporary is not None:
                os.unlink(temporary, dir_fd=self.root)
            os.fchdir(cwd)
            os.close(cwd)

    def environment(self, record):
        run_id = record["run_id"]
        prefix = f"tubarr-e2e-u{self.uid}-{run_id}-"
        return {
            "E2E_WORK": f"{self.path}/runs/{run_id}",
            "E2E_NET": prefix + "net",
            "E2E_SONARR_NAME": prefix + "sonarr",
            "E2E_RADARR_NAME": prefix + "radarr",
            "E2E_TUBARR_NAME": prefix + "tubarr",
            "E2E_RUN_ID": run_id,
            "E2E_OWNER_UID": str(self.uid),
        }

    def work(self, record, create=False):
        runs = self.directory(self.root, "runs", create=create)
        try:
            self.directory(runs, record["run_id"], create=create)
        except FileNotFoundError:
            if create:
                raise
        return runs


def check_worker(context):
    record = context.read_state()
    if record is None:
        raise LifecycleError("worker requires published lifecycle state")
    for key, expected in context.environment(record).items():
        if os.environ.get(key) != expected:
            raise LifecycleError("worker context does not match published state")
    descriptors = []
    for key in ("E2E_LOCK_FD", "E2E_DRIVER_FD"):
        value = os.environ.get(key, "")
        if not re.fullmatch(r"[0-9]+", value) or len(value) > 10 or int(value) < 3:
            raise LifecycleError("worker requires inherited lifecycle descriptors")
        descriptors.append(int(value))
    lock, driver = descriptors
    private_file(os.fstat(lock), context.uid)
    named = context.file("lifecycle.lock", os.O_RDWR)
    try:
        inherited_info, named_info = os.fstat(lock), os.fstat(named)
        if (inherited_info.st_dev, inherited_info.st_ino) != (named_info.st_dev, named_info.st_ino):
            raise LifecycleError("worker inherited the wrong lock inode")
        try:
            fcntl.flock(named, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            pass
        else:
            raise LifecycleError("worker lifecycle lock is not held")
        # Only the shared open-file description can reacquire the held flock.
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    finally:
        os.close(named)
    if (not stat.S_ISFIFO(os.fstat(driver).st_mode)
            or fcntl.fcntl(driver, fcntl.F_GETFL) & os.O_ACCMODE == os.O_RDONLY):
        raise LifecycleError("worker driver descriptor must be a writable pipe")
    runs = context.directory(context.root, "runs")
    context.directory(runs, record["run_id"])


def inspect_owned(kind, name, labels):
    result = subprocess.run(["docker", kind, "inspect", name], capture_output=True)
    if result.returncode:
        exact_name = re.escape(name.encode())
        absent = rb"No such " + kind.encode() + rb": " + exact_name
        if kind == "network":
            absent = rb"(?:" + absent + rb"|network " + exact_name + rb" not found)"
        missing = rb"(?:Error: |Error response from daemon: )?" + absent
        if re.fullmatch(missing, result.stderr.strip()):
            return None
        raise LifecycleError(f"cannot inspect {kind} {name}, state retained", 1)
    try:
        objects = json.loads(result.stdout)
        if type(objects) is not list or len(objects) != 1 or type(objects[0]) is not dict:
            raise ValueError()
        resource = objects[0]
        actual = resource["Labels"] if kind == "network" else resource["Config"]["Labels"]
        if not isinstance(actual, dict) or any(actual.get(key) != value for key, value in labels.items()):
            raise ValueError()
        identity = resource["Id"]
        if not isinstance(identity, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*", identity):
            raise ValueError()
        return identity
    except (ValueError, KeyError, TypeError):
        raise LifecycleError(f"unverified ownership or inspect record for {kind} {name}, state retained", 1) from None


def teardown(context, record):
    if context.read_state() != record:
        raise LifecycleError("state changed before teardown")
    runs = context.work(record)
    if not shutil.rmtree.avoids_symlink_attacks:
        raise LifecycleError("safe descriptor-relative work removal is unavailable")
    values = context.environment(record)
    labels = {"io.tubarr.e2e": "1", "io.tubarr.e2e.uid": str(context.uid),
              "io.tubarr.e2e.run": record["run_id"]}
    resources, errors = [], []
    # Complete every ownership inspection before allowing even one removal.
    for role in ROLES:
        kind = "network" if role == "net" else "container"
        name = values["E2E_NET" if role == "net" else f"E2E_{role.upper()}_NAME"]
        try:
            identity = inspect_owned(kind, name, labels)
            if identity is not None:
                resources.append((kind, identity))
        except (LifecycleError, OSError) as error:
            errors.append(str(error))
    if errors:
        raise LifecycleError("cleanup preflight failed: " + ", ".join(errors), 1)
    for kind, identity in resources:
        command = ["docker", kind, "rm"]
        if kind == "container":
            command.append("-f")
        result = subprocess.run(command + [identity], capture_output=True)
        if result.returncode:
            raise LifecycleError(f"{kind} removal failed, use e2e/run.sh down to retry", 1)
    # Revalidate paths after Docker has stopped writing into the bind mounts.
    runs = context.work(record)
    try:
        shutil.rmtree(record["run_id"], dir_fd=runs)
    except FileNotFoundError:
        pass
    if context.read_state() != record:
        raise LifecycleError("state changed during teardown")
    os.unlink("state.json", dir_fd=context.root)
    os.fsync(context.root)
    print("e2e: torn down")


class Supervisor:
    def __init__(self):
        self.interrupted = None
        self.worker = None
        for sig in (signal.SIGINT, signal.SIGTERM):
            signal.signal(sig, self.receive)

    def receive(self, sig, frame):
        if self.interrupted is None:
            self.interrupted = sig
        self.forward(sig)

    def forward(self, sig):
        # main established this private group before any child was spawned.
        previous = signal.signal(sig, signal.SIG_IGN)
        try:
            os.killpg(os.getpid(), sig)
        finally:
            signal.signal(sig, previous)

    def run(self, context, record, evidence):
        read_fd, write_fd = os.pipe2(os.O_CLOEXEC | os.O_NONBLOCK)
        try:
            if self.interrupted is not None:
                return 128 + self.interrupted, False
            env = os.environ | context.environment(record) | {
                "E2E_LOCK_FD": str(context.lock_fd), "E2E_DRIVER_FD": str(write_fd)}
            args = ["bash", str(SCRIPT), "--lifecycle-worker"]
            if evidence is not None:
                args.append(evidence)
            self.worker = subprocess.Popen(args, env=env, pass_fds=(context.lock_fd, write_fd))
            os.close(write_fd)
            write_fd = None
            # A signal may have arrived inside Popen, before worker assignment.
            if self.interrupted is not None:
                self.forward(self.interrupted)
            status = self.worker.wait()
            try:
                marker = os.read(read_fd, len(DRIVER_STARTED) + 1)
            except BlockingIOError:
                marker = b""
            return (128 - status if status < 0 else status), marker == DRIVER_STARTED
        finally:
            if self.worker is not None and self.worker.returncode is None:
                self.forward(signal.SIGTERM)
                self.worker.wait()
            os.close(read_fd)
            if write_fd is not None:
                os.close(write_fd)


def finish_run(context, record, status, driver_started, supervisor, evidence=None):
    """Single terminal path, after worker reap and before retention or teardown.

    The CI evidence row can collect sanitized output here with the validated
    context and original status, without changing worker or cleanup ownership.
    """
    if supervisor.interrupted is not None:
        status = 128 + supervisor.interrupted
    try:
        from evidence import collect_terminal
        collect_terminal(context.environment(record)["E2E_WORK"],
                         evidence or str(SCRIPT.parent.parent / "target/e2e-evidence"),
                         context.environment(record), status)
    except Exception as error:
        # A broken collector must not leak raw diagnostics or prevent teardown.
        diagnostic(f"terminal evidence collection failed ({type(error).__name__})")
    if os.environ.get("KEEP") == "1" and driver_started and supervisor.interrupted is None:
        print("e2e: left running (KEEP=1), tear down with: e2e/run.sh down")
        return status
    try:
        teardown(context, record)
    except (LifecycleError, OSError) as error:
        diagnostic(str(error))
        if supervisor.interrupted is not None:
            return 128 + supervisor.interrupted
        return status or getattr(error, "status", 1)
    return 128 + supervisor.interrupted if supervisor.interrupted is not None else status


def main(args):
    if not args or not (args[0] == "start" and len(args) <= 2
                        or args[0] in ("down", "check-worker") and len(args) == 1):
        raise LifecycleError("usage: state.py start [evidence_dir] | down")
    os.umask(0o077)
    if args[0] != "check-worker" and os.getpgrp() != os.getpid():
        os.setpgid(0, 0)
    with ExitStack() as stack:
        context = Context(stack, create=args[0] != "check-worker")
        if args[0] == "check-worker":
            check_worker(context)
            return 0
        context.lock()
        record = context.read_state()
        if args[0] == "down":
            if record is not None:
                teardown(context, record)
            return 0
        if record is not None:
            raise LifecycleError("busy: retained E2E run, recover with e2e/run.sh down", 75)
        supervisor = Supervisor()
        # Allocate the private runs parent first so down can recover even if
        # the process dies immediately after publishing, before work exists.
        context.directory(context.root, "runs", create=True)
        record = {"version": 1, "uid": context.uid, "run_id": secrets.token_hex(16)}
        status, driver_started = 1, False
        try:
            # State must identify this run before setup can have any effects.
            context.publish(record)
            context.work(record, create=True)
            status, driver_started = supervisor.run(context, record, args[1] if len(args) == 2 else None)
        except (LifecycleError, OSError) as error:
            diagnostic(str(error))
            status = getattr(error, "status", 1)
        if not context.published:
            return 128 + supervisor.interrupted if supervisor.interrupted is not None else status
        return finish_run(context, record, status, driver_started, supervisor,
                          args[1] if len(args) == 2 else None)


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except (LifecycleError, OSError) as error:
        diagnostic(str(error))
        sys.exit(getattr(error, "status", 2))
