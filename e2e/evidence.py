"""Selected textual E2E evidence, sanitized before any persistence."""

from contextlib import contextmanager
import html
import json
import os
from pathlib import Path
import re
import stat
import subprocess
from urllib.parse import quote, quote_plus
import xml.etree.ElementTree as ET


DIRECTORY = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
LIMIT = 256 * 1024


@contextmanager
def directory(path, create=False):
    """Walk a fixed local path without following any directory symlink."""
    fd = os.open("/", DIRECTORY)
    try:
        for part in Path(os.path.abspath(path)).parts[1:]:
            if create:
                try:
                    os.mkdir(part, 0o700, dir_fd=fd)
                except FileExistsError:
                    pass
            child = os.open(part, DIRECTORY, dir_fd=fd)
            os.close(fd)
            fd = child
        info = os.fstat(fd)
        if info.st_uid != os.geteuid() or info.st_mode & 0o022:
            raise ValueError("unsafe evidence or work directory")
        yield fd
    finally:
        os.close(fd)


def read_owned(root, relative):
    with directory(os.path.join(root, os.path.dirname(relative))) as parent:
        fd = os.open(os.path.basename(relative), os.O_RDONLY | os.O_NOFOLLOW
                     | os.O_NONBLOCK | os.O_CLOEXEC, dir_fd=parent)
        with os.fdopen(fd, "rb") as source:
            info = os.fstat(source.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid() or info.st_nlink != 1:
                raise ValueError("unsafe credential source")
            raw = source.read(LIMIT + 1)
            if len(raw) > LIMIT:
                raise ValueError("credential source exceeds limit")
            return raw.decode("utf-8")


def run_secrets(work):
    values = set()
    for role in ("sonarr", "radarr", "tubarr"):
        try:
            value = read_owned(work, f"tubarr/secrets/{role}_api_key").strip()
            if value:
                values.add(value)
        except FileNotFoundError:
            pass
        if role != "tubarr":
            try:
                config = ET.fromstring(read_owned(work, f"{role}/config.xml"))
                for node in config.iter("ApiKey"):
                    if node.text and node.text.strip():
                        values.add(node.text.strip())
            except FileNotFoundError:
                pass
    return values


def redact_text(text, secrets):
    forms = set()
    for secret in secrets:
        if not secret:
            continue
        bases = {secret, html.escape(secret), html.escape(secret, quote=False),
                 html.escape(secret).replace("&#x27;", "&apos;"),
                 json.dumps(secret)[1:-1], json.dumps(secret, ensure_ascii=False)[1:-1]}
        for value in bases:
            forms.add(value)
            for encode in (lambda s: quote(s, safe=""), lambda s: quote_plus(s, safe="")):
                encoded = encode(value)
                forms.update((encoded, encoded.lower(),
                              re.sub(r"%[0-9A-F]{2}", lambda m: m[0].lower(), encoded)))
                double = encode(encoded)
                forms.update((double, double.lower(),
                              re.sub(r"%[0-9A-F]{2}", lambda m: m[0].lower(), double)))
        # Serialized JSON may itself contain an escaped representation.
        forms.update(json.dumps(value)[1:-1] for value in tuple(forms))
    for value in sorted(forms, key=len, reverse=True):
        text = text.replace(value, "[REDACTED]")
    return text


def sanitize(value, secrets):
    if isinstance(value, str):
        return redact_text(value, secrets)
    if isinstance(value, list):
        return [sanitize(item, secrets) for item in value]
    if isinstance(value, dict):
        return {redact_text(str(key), secrets): sanitize(item, secrets)
                for key, item in value.items()}
    return value


def serialize(value, secrets):
    if isinstance(value, bytes):
        value = value.decode("utf-8", "replace")
    if isinstance(value, str):
        return redact_text(value, secrets)
    return json.dumps(sanitize(value, secrets), indent=2)


def output_fd(parent, name):
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*", name):
        raise ValueError("invalid evidence filename")
    # Do not truncate an existing hard link or follow a link out of the folder.
    fd = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK,
                 0o600, dir_fd=parent)
    try:
        info = os.fstat(fd)
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid()
                or info.st_nlink != 1):
            raise ValueError("unsafe evidence file")
        os.fchmod(fd, 0o600)
        os.ftruncate(fd, 0)
        return fd
    except BaseException:
        os.close(fd)
        raise


def write_text(evidence, name, text):
    with directory(evidence, create=True) as parent:
        with os.fdopen(output_fd(parent, name), "w", encoding="utf-8") as out:
            out.write(text)


def collect_terminal(work, evidence, names, exit_code):
    """Called once after worker reap, while the owned service containers exist."""
    # This constant status contains no credentials and survives unreadable sources.
    write_text(evidence, "lifecycle.json", json.dumps({"exit_code": int(exit_code)}, indent=2))
    # Fail closed before any service inspection if the full key set is unavailable.
    secrets = run_secrets(work)
    diagnostics = []
    labels = {"io.tubarr.e2e": "1", "io.tubarr.e2e.uid": names["E2E_OWNER_UID"],
              "io.tubarr.e2e.run": names["E2E_RUN_ID"]}
    for role in ("sonarr", "radarr", "tubarr"):
        try:
            name = names[f"E2E_{role.upper()}_NAME"]
            inspected = subprocess.run(["docker", "container", "inspect", name],
                                       capture_output=True, timeout=2)
            if inspected.returncode:
                continue  # Partial setup need not have created this resource.
            objects = json.loads(inspected.stdout)
            actual = objects[0]["Config"]["Labels"]
            if len(objects) != 1 or any(actual.get(k) != v for k, v in labels.items()):
                raise ValueError("container ownership mismatch")
            # Docker bounds lines and time span. The retained byte tail is bounded too.
            logs = subprocess.run(["docker", "logs", "--tail", "400", "--since", "30m", name],
                                  capture_output=True, timeout=4)
            raw = (logs.stdout + logs.stderr)[-LIMIT:]
            text = redact_text(raw.decode("utf-8", "replace"), secrets)
            write_text(evidence, f"{role}.log", text)
            if logs.returncode:
                diagnostics.append(f"{role}: log command exited {logs.returncode}")
        except Exception as error:
            # Never print exception text before it passes the same sanitizer.
            diagnostics.append(f"{role}: {type(error).__name__}: {error}")
    if diagnostics:
        write_text(evidence, "collector.log", redact_text("\n".join(diagnostics), secrets))
