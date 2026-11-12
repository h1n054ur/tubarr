"""Real evidence writers with synthetic credentials and no service requests."""

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import signal
import tempfile
import unittest
from unittest.mock import patch
from urllib.parse import quote, quote_plus
import xml.etree.ElementTree as ET

import test_state


SOURCE = Path(__file__).resolve().parent
# Deliberate punctuation exercises URL encoding, XML and JSON escaping.
KEYS = ("test-only-tubarr+/=&\"<>key", "test-only-sonarr+/=&key", "test-only-radarr+/=&key")


def variants(value):
    encoded = quote(value, safe="")
    return {value, encoded, quote_plus(value), quote(encoded, safe=""),
            encoded.lower(), json.dumps(value)[1:-1],
            value.replace("&", "&amp;").replace('"', "&quot;").replace("<", "&lt;").replace(">", "&gt;")}


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="evidence-", dir=os.environ.get("TMPDIR"))
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.work, self.evidence = self.root / "work", self.root / "sanitized"
        secrets = self.work / "tubarr/secrets"
        secrets.mkdir(parents=True)
        self.evidence.mkdir()
        (self.work / "data").mkdir()
        for name, value in zip(("tubarr", "sonarr", "radarr"), KEYS):
            (secrets / (name + "_api_key")).write_text(value)
        env = {"E2E_WORK": str(self.work), "E2E_EVIDENCE": str(self.evidence),
               "E2E_TUBARR": "http://test.invalid", "E2E_TUBARR_INNER": "http://test.invalid:8480",
               "E2E_SONARR": "http://test.invalid", "E2E_RADARR": "http://test.invalid"}
        self.env = patch.dict(os.environ, env)
        self.env.start()
        self.addCleanup(self.env.stop)
        spec = importlib.util.spec_from_file_location("artifact_driver_under_test", SOURCE / "drive.py")
        self.driver = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.driver)

    def assert_clean(self, text):
        for key in KEYS:
            for candidate in variants(key):
                self.assertFalse(candidate in text, "credential representation persisted")

    def test_terminal_source_read_failure_keeps_original_status_without_service_logs(self):
        spec = importlib.util.spec_from_file_location("artifact_evidence_under_test", SOURCE / "evidence.py")
        helper = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(helper)
        sonarr = self.work / "sonarr"
        sonarr.mkdir()
        malformed = "<Config><ApiKey>" + KEYS[1]
        (sonarr / "config.xml").write_text(malformed)
        with self.assertRaises(ET.ParseError) as invalid_source:
            ET.fromstring(malformed)
        source_error = str(invalid_source.exception)
        names = {"E2E_OWNER_UID": str(os.geteuid()), "E2E_RUN_ID": "synthetic-source-failure",
                 "E2E_SONARR_NAME": "synthetic-sonarr", "E2E_RADARR_NAME": "synthetic-radarr",
                 "E2E_TUBARR_NAME": "synthetic-tubarr"}
        with patch.object(helper.subprocess, "run", side_effect=AssertionError(
                "source-read failure must not begin container inspection or log collection")) as run:
            try:
                helper.collect_terminal(self.work, self.evidence, names, 42)
            except ET.ParseError:
                pass
            run.assert_not_called()
        for path in self.evidence.rglob("*"):
            if path.is_file():
                text = path.read_text()
                self.assert_clean(text)
                self.assertFalse(source_error in text, "source exception text persisted")
        for role in ("sonarr", "radarr", "tubarr"):
            self.assertFalse((self.evidence / (role + ".log")).exists(), "service log persisted")
        lifecycle = self.evidence / "lifecycle.json"
        self.assertTrue(lifecycle.is_file(), "source-read failure lost original lifecycle status")
        status = json.loads(lifecycle.read_text())["exit_code"]
        self.assertIs(type(status), int)
        self.assertEqual(status, 42)

    def test_save_never_persists_raw_bytes_then_scrubs_them(self):
        written = io.StringIO()

        class Sink:
            def __enter__(self):
                return written

            def __exit__(self, *args):
                return False

        # Observe bytes given to the actual writer, not just its eventual file contents.
        # A write-raw-then-rewrite implementation must still fail this guard.
        with patch.object(self.driver, "open", create=True, return_value=Sink()):
            self.driver.save("newznab.xml", "<url>" + quote(KEYS[0], safe="") + "</url>")
        self.assertTrue(written.getvalue(), "save did not use its inspected persistence boundary")
        self.assert_clean(written.getvalue())

    def test_actual_save_redacts_xml_json_errors_and_encoded_urls_before_write(self):
        payload = "\n".join(form for key in KEYS for form in variants(key))
        for name, value in (("newznab.xml", ('<enclosure url="http://test.invalid/?apikey=' + payload + '"/>').encode()),
                            ("nested.json", {"safe": "kept", "nested": [{"url": payload}]}),
                            ("errors.log", "request failed: " + payload)):
            with self.subTest(name=name):
                self.driver.save(name, value)
                text = (self.evidence / name).read_text()
                self.assert_clean(text)
                self.assertIn("[REDACTED]", text)
                self.assertTrue(text, "do not discard all evidence")
        self.assertEqual(json.loads((self.evidence / "nested.json").read_text())["safe"], "kept")

    def test_actual_collect_redacts_nested_fields_and_exception_text(self):
        payload = " ".join(form for key in KEYS for form in variants(key))

        class Arr:
            def __init__(self, name):
                self.name = name

            def get(self, path):
                if path == "/health":
                    raise RuntimeError("lookup unavailable " + payload)
                return [{"fields": [{"name": "apiKey", "value": KEYS[0]},
                                    {"name": "baseUrl", "value": payload}], "detail": payload}]

        self.driver.collect(Arr("sonarr"), Arr("radarr"))
        self.assertGreaterEqual(len(list(self.evidence.glob("*.json"))), 10)
        for path in self.evidence.glob("*.json"):
            with self.subTest(file=path.name):
                self.assert_clean(path.read_text())

    def test_step_logs_and_results_never_contain_credentials(self):
        detail = " ".join(form for key in KEYS for form in variants(key))
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            self.driver.step("synthetic failure", False, detail)
        self.driver.save("results.json", self.driver.results)
        self.assert_clean(output.getvalue())
        self.assertIn("[REDACTED]", output.getvalue())
        self.assert_clean((self.evidence / "results.json").read_text())

    def test_unreachable_metadata_and_failed_collection_cannot_turn_driver_green(self):
        def unavailable(*args):
            raise ConnectionError("metadata unavailable " + quote(KEYS[0], safe=""))

        output = io.StringIO()
        with patch.object(self.driver, "run", unavailable), patch.object(self.driver, "collect", unavailable), \
                contextlib.redirect_stdout(output):
            try:
                self.driver.main()
            except SystemExit as result:
                status = result.code
            except ConnectionError:
                status = 1
            else:
                status = 0
        self.assertNotEqual(status, 0)
        self.assertTrue((self.evidence / "results.json").is_file(), "collector failure lost driver results")
        self.assert_clean(output.getvalue())
        self.assert_clean((self.evidence / "results.json").read_text())


    def test_real_driver_metadata_lookup_failure_is_a_failed_gate(self):
        class Arr:
            def __init__(self, *args):
                pass

            def get(self, path):
                if path == "/qualityprofile":
                    return [{"name": "Any", "id": 1}]
                raise ConnectionError("external metadata unavailable")

        output = io.StringIO()
        with patch.object(self.driver, "Arr", Arr), patch.object(self.driver, "setup_arr", return_value=(1, 2, 3)), \
                patch.object(self.driver, "call", return_value={"status": "ok"}), \
                patch.object(self.driver, "collect", return_value=None), contextlib.redirect_stdout(output):
            with self.assertRaises(SystemExit) as result:
                self.driver.main()
        self.assertNotEqual(result.exception.code, 0)
        records = json.loads((self.evidence / "results.json").read_text())
        self.assertTrue(any(not row["ok"] and "metadata unavailable" in row["detail"] for row in records))


class TerminalEvidenceTests(unittest.TestCase):
    def setUp(self):
        # Reuse only B's fixture plumbing, not its test cases or stub API driver.
        self.harness = test_state.LifecycleTests("test_empty_down_is_idempotent")
        self.harness.setUp()
        self.addCleanup(self.harness.doCleanups)
        for path in SOURCE.glob("*.py"):
            if not path.name.startswith("test_"):
                shutil.copy2(path, self.harness.repo / "e2e" / path.name)
        docker = self.harness.fake / "docker"
        docker.write_text(docker.read_text().replace(
            'print("synthetic log")',
            'assert args[-1] in resources, "logs requested after teardown"\n'
            '    print("synthetic log http://test.invalid/?apikey=synthetic-test-key")'))

    def assert_terminal(self, status, logs_available=True):
        evidence = self.harness.root / "evidence"
        path = evidence / "lifecycle.json"
        self.assertTrue(path.is_file(), "terminal collector did not persist available evidence before teardown")
        self.assertEqual(json.loads(path.read_text())["exit_code"], status)
        self.assertFalse(self.harness.state.exists())
        self.assertEqual(self.harness.resources(), {})
        logs = list(evidence.glob("*.log"))
        if logs_available:
            self.assertTrue(any("synthetic log" in p.read_text() for p in logs), "available service logs lost")
        for path in evidence.rglob("*"):
            if path.is_file():
                self.assertFalse(b"synthetic-test-key" in path.read_bytes(), "setup API key leaked")
        self.assertFalse(any(p.suffix in {".db", ".toml"} or "api_key" in p.name
                             for p in evidence.rglob("*") if p.is_file()))

    def test_setup_failure_keeps_exit_and_collects_available_logs(self):
        result = self.harness.run_cli(self.harness.root / "evidence", extra={"FAKE_DOCKER_FAIL_ROLE": "radarr"})
        self.assertEqual(result.returncode, 42)
        self.assert_terminal(42)

    def test_early_missing_binary_still_collects_terminal_status(self):
        (self.harness.repo / "target/release/tubarr").unlink()
        result = self.harness.run_cli(self.harness.root / "evidence")
        self.assertNotEqual(result.returncode, 0)
        self.assert_terminal(result.returncode, logs_available=False)

    def test_collection_failure_does_not_replace_original_setup_status(self):
        docker = self.harness.fake / "docker"
        docker.write_text(docker.read_text().replace(
            'print("synthetic log http://test.invalid/?apikey=synthetic-test-key")',
            'sys.exit(57)'))
        result = self.harness.run_cli(self.harness.root / "evidence", extra={"FAKE_DOCKER_FAIL_ROLE": "radarr"})
        self.assertEqual(result.returncode, 42)
        self.assert_terminal(42, logs_available=False)

    def test_interrupt_collects_before_cleanup_and_keeps_signal_status(self):
        proc, _ = self.harness.barrier_start()
        os.kill(proc.pid, signal.SIGTERM)
        result = self.harness.finish(proc)
        self.assertEqual(result.returncode, 143)
        self.assert_terminal(143, logs_available=False)


if __name__ == "__main__":
    unittest.main()
