import { expect, test } from "bun:test";
import { YAML } from "bun";
import { existsSync, readFileSync } from "node:fs";
import { relative, resolve } from "node:path";

const root = resolve(import.meta.dir, "..");
const read = (path: string) => readFileSync(resolve(root, path), "utf8");
const workflow = (path: string): any => {
  expect(existsSync(resolve(root, path)), `missing workflow ${path}`).toBe(true);
  return YAML.parse(read(path));
};
const list = (value: any): string[] => value == null ? [] : Array.isArray(value) ? value : [value];
const runs = (job: any) => (job.steps ?? []).map((s: any) => s.run ?? "").join("\n");
const steps = (job: any) => job.steps ?? [];
const closure = (jobs: any, id: string, seen = new Set<string>()): Set<string> => {
  for (const dep of list(jobs[id]?.needs)) {
    expect(jobs[dep], `undefined dependency ${dep}`).toBeDefined();
    if (!seen.has(dep)) { seen.add(dep); closure(jobs, dep, seen); }
  }
  return seen;
};

function core(jobs: any) {
  const rust = runs(jobs.check);
  for (const cmd of ["cargo fmt --all --check", "cargo clippy --all-targets --locked -- -D warnings",
                     "cargo test --all --locked", "--locked check advisories licenses bans sources"])
    expect(rust).toContain(cmd);
  const web = runs(jobs.web);
  for (const cmd of ["bun install --frozen-lockfile", "bun scripts/licenses.ts check",
                     "bun run typecheck", "bun run test", "bun run build"])
    expect(web).toContain(cmd);
  expect(web).not.toContain("bun run lint");
  for (const id of ["check", "web", "harness"]) {
    expect(jobs[id]).toBeDefined();
    expect(jobs[id]["timeout-minutes"]).toBeGreaterThan(0);
    expect(jobs[id]["continue-on-error"]).toBeUndefined();
    expect(runs(jobs[id])).not.toMatch(/\|\|\s*true/);
    for (const step of steps(jobs[id])) expect(step["continue-on-error"]).toBeUndefined();
  }
  expect(runs(jobs.harness)).toContain("test_artifacts.py");
  expect(runs(jobs.harness)).toContain("test_state.py");
  expect(runs(jobs.harness)).toContain("scripts/check-workflows.test.ts");
}

test("ordinary public PR, push and reusable core checks are unprivileged and pinned", () => {
  const ci = workflow(".github/workflows/ci.yml");
  for (const trigger of ["push", "pull_request", "workflow_call"]) expect(Object.hasOwn(ci.on, trigger)).toBe(true);
  expect(ci.on.push.branches).toEqual(["main"]);
  expect(ci.on.pull_request_target).toBeUndefined();
  expect(ci.on.workflow_run).toBeUndefined();
  expect(ci.on.pull_request?.paths).toBeUndefined();
  expect(ci.permissions).toEqual({ contents: "read" });
  expect(ci.concurrency["cancel-in-progress"]).toBe(true);
  expect(ci.concurrency.group).toBe("ci-${{ github.workflow }}-${{ github.ref }}");
  const release = workflow(".github/workflows/release.yml");
  expect(release.concurrency["cancel-in-progress"]).toBe(false);
  for (const ref of ["refs/tags/v0.1.1", "refs/heads/main"]) {
    const evaluated = (group: string) => group
      .replaceAll("${{ github.workflow }}", release.name)
      .replaceAll("${{ github.ref }}", ref)
      .toLowerCase();
    expect(evaluated(ci.concurrency.group))
      .not.toBe(evaluated(release.concurrency.group));
  }
  core(ci.jobs);
  for (const [id, name] of [["check", "Rust checks"], ["web", "Web checks"], ["harness", "Harness checks"]]) {
    const job = ci.jobs[id];
    expect(job.name).toBe(name);
    expect(job["runs-on"]).toBe("ubuntu-24.04");
    expect(job.if).toBeUndefined();
    expect(job.secrets).toBeUndefined();
    expect(job.permissions ?? ci.permissions).toEqual({ contents: "read" });
    expect(JSON.stringify(job)).not.toContain("secrets.");
    for (const step of steps(job)) {
      if (step.uses) expect(step.uses).toMatch(/@[0-9a-f]{40}$/);
      if (step.uses?.includes("checkout")) {
        expect(step.with?.ref).toBe("${{ github.sha }}");
        expect(step.with?.["persist-credentials"]).toBe(false);
      }
      if (step.uses?.includes("setup-bun")) expect(String(step.with?.["bun-version"])).toBe("1.4.2");
    }
  }
  expect(read("rust-toolchain.toml")).toContain('channel = "1.99.0"');
});

test("public publication waits for exact-SHA reusable checks and real hosted E2E", () => {
  const release = workflow(".github/workflows/release.yml");
  expect(release.jobs.checks?.uses).toBe("./.github/workflows/ci.yml");
  expect(release.jobs.checks.secrets).toBeUndefined();
  const e2e = release.jobs.e2e;
  expect(e2e["runs-on"]).toBe("ubuntu-24.04");
  expect(e2e["timeout-minutes"]).toBeGreaterThan(0);
  expect(e2e["continue-on-error"]).toBeUndefined();
  expect(e2e.name).toBe("Real E2E");
  expect(e2e.permissions ?? release.permissions).toEqual({ contents: "read" });
  for (const step of steps(e2e)) expect(step["continue-on-error"]).toBeUndefined();
  expect(closure(release.jobs, "e2e").has("checks")).toBe(true);
  expect(runs(e2e)).toContain("cargo build --release --locked");
  expect(runs(e2e)).toContain("e2e/run.sh");
  expect(runs(e2e)).not.toMatch(/\|\|\s*true|continue-on-error|TEST_DRIVER|FAKE_DOCKER/);
  for (const id of ["build", "merge", "release"]) {
    const before = closure(release.jobs, id);
    expect(before.has("checks"), id).toBe(true);
    expect(before.has("e2e"), id).toBe(true);
    expect(release.jobs[id]["continue-on-error"]).toBeUndefined();
    expect(release.jobs[id].if ?? "").not.toMatch(/always\(\)|failure\(\)|cancelled\(\)/);
  }
  for (const id of ["e2e", "build"]) {
    for (const step of steps(release.jobs[id])) if (step.uses?.includes("checkout"))
      expect(step.with?.ref).toBe("${{ github.sha }}");
  }
  const upload = steps(e2e).filter((s: any) => s.uses?.includes("upload-artifact"));
  expect(upload).toHaveLength(1);
  expect(upload[0].if).toBe("${{ failure() || cancelled() }}");
  expect(upload[0].with.path).toBe("${{ runner.temp }}/tubarr-e2e-sanitized");
  expect(upload[0].with["retention-days"]).toBeGreaterThan(0);
  expect(upload[0].with["retention-days"]).toBeLessThanOrEqual(3);
});

test.skipIf(!existsSync(resolve(root, ".forgejo/workflows/ci.yml")))("private publication has supported same-workflow success dependencies", () => {
  const ci = workflow(".forgejo/workflows/ci.yml");
  expect(ci.on.workflow_run).toBeUndefined();
  expect(Object.hasOwn(ci.on, "push")).toBe(true);
  expect(ci.on.push?.branches).toBeUndefined();
  expect(ci.on.push?.tags).toBeUndefined();
  expect(Object.hasOwn(ci.on, "pull_request")).toBe(true);
  core(ci.jobs);
  for (const id of ["check", "web", "harness"]) {
    for (const step of steps(ci.jobs[id])) {
      if (step.uses) expect(step.uses).toMatch(/@[0-9a-f]{40}$/);
      if (step.uses?.includes("checkout")) {
        expect(step.with?.ref).toBe("${{ github.sha }}");
        expect(step.with?.["persist-credentials"]).toBe(false);
      }
    }
  }
  const image = ci.jobs.image;
  expect(image).toBeDefined();
  for (const id of ["check", "web", "harness"]) expect(closure(ci.jobs, "image").has(id)).toBe(true);
  expect(image.if).toContain("success()");
  expect(image.if).toContain("refs/heads/main");
  expect(image.if).toContain("refs/tags/v");
  expect(image.if).not.toContain("always()");
  expect(image["runs-on"]).toBe("docker");
  expect(existsSync(resolve(root, ".forgejo/workflows/image.yml"))).toBe(false);
});

test("public workflow scripts and evidence code survive the clean export", () => {
  const excluded = existsSync(resolve(root, "scripts/public-mirror/exclude.txt"))
    ? read("scripts/public-mirror/exclude.txt").split("\n").filter(s => s && !s.startsWith("#"))
    : [".forgejo/**", "scripts/ci/**", "scripts/public-mirror/**"];
  for (const path of ["scripts/check-workflows.test.ts", "e2e/test_artifacts.py", "e2e/evidence.py"])
    expect(excluded.some(pattern => new Bun.Glob(pattern).match(path)), path).toBe(false);
  for (const file of [".github/workflows/ci.yml", ".github/workflows/release.yml"]) {
    const wf = workflow(file);
    for (const job of Object.values(wf.jobs) as any[]) {
      expect(runs(job)).not.toContain("scripts/ci/");
      for (const step of steps(job)) {
        const directory = step["working-directory"] ?? job.defaults?.run?.["working-directory"]
          ?? wf.defaults?.run?.["working-directory"] ?? ".";
        for (const match of (step.run ?? "").matchAll(/\b((?:scripts|e2e)\/[A-Za-z0-9_./-]+\.(?:py|sh|ts))\b/g)) {
          const path = relative(root, resolve(root, directory, match[1]));
          expect(existsSync(resolve(root, path)), path).toBe(true);
          expect(excluded.some(pattern => new Bun.Glob(pattern).match(path)), path).toBe(false);
        }
      }
    }
  }
});
