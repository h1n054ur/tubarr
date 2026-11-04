// Licences of the web UI's npm packages, read from node_modules after `bun install`.
//
//   bun scripts/licenses.ts check            every package, dev tools too: fails on a licence not in ALLOWED
//   bun scripts/licenses.ts notices <file>   the runtime packages bundled into web/dist, with their licence
//                                            texts, for the image's third-party notices
//
// Like deny.toml for the Rust crates: a package under any other licence fails the check until someone
// reads its terms and adds it here on purpose.
import { existsSync, readdirSync, readFileSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'

const ALLOWED = new Set([
  'MIT',
  'ISC',
  'Apache-2.0',
  'BSD-2-Clause',
  'BSD-3-Clause',
  '0BSD',
  'BlueOak-1.0.0',
  'CC0-1.0',
  'CC-BY-4.0',
  'MPL-2.0',
  'OFL-1.1',
  'Python-2.0',
  'Unlicense',
])

type Pkg = { name: string; version: string; license: string; dir: string }

const root = join(import.meta.dir, '..')

function readJson(path: string): Record<string, unknown> {
  return JSON.parse(readFileSync(path, 'utf8'))
}

function licenseOf(manifest: Record<string, unknown>): string {
  const license = manifest.license
  if (typeof license === 'string') return license
  if (license && typeof license === 'object' && 'type' in license) return String(license.type)
  if (Array.isArray(manifest.licenses)) return manifest.licenses.map((l: { type: string }) => l.type).join(' OR ')
  return 'UNKNOWN'
}

/** Node's lookup: the nearest node_modules/<name> from `from` up to the web root. */
function resolve(name: string, from: string): string | null {
  for (let dir = from; ; dir = dirname(dir)) {
    const candidate = join(dir, 'node_modules', name)
    if (existsSync(join(candidate, 'package.json'))) return candidate
    if (dir === root || dir === dirname(dir)) return null
  }
}

/** Every package reachable from the root's dependencies (and devDependencies with `dev`). */
function walk(dev: boolean): Pkg[] {
  const manifest = readJson(join(root, 'package.json'))
  const seen = new Map<string, Pkg>()
  const queue: [string, string, boolean][] = []
  const push = (deps: unknown, from: string, optional: boolean) => {
    for (const name of Object.keys((deps as Record<string, string>) ?? {})) queue.push([name, from, optional])
  }
  push(manifest.dependencies, root, false)
  if (dev) push(manifest.devDependencies, root, false)
  while (queue.length > 0) {
    const [name, from, optional] = queue.shift()!
    const dir = resolve(name, from)
    if (dir === null) {
      // An optional dependency for another platform is not installed here.
      if (optional) continue
      throw new Error(`${name} (needed from ${from}) is not in node_modules: run bun install first`)
    }
    if (seen.has(dir)) continue
    const pkg = readJson(join(dir, 'package.json'))
    seen.set(dir, { name, version: String(pkg.version), license: licenseOf(pkg), dir })
    push(pkg.dependencies, dir, false)
    push(pkg.optionalDependencies, dir, true)
    push(pkg.peerDependencies, dir, true)
  }
  return [...seen.values()].sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version))
}

/** An SPDX expression passes when one side of every OR has all its AND parts allowed. */
function allowed(expression: string): boolean {
  const bare = expression.replace(/[()]/g, ' ')
  return bare.split(/\s+OR\s+/).some((alt) => alt.split(/\s+AND\s+/).every((id) => ALLOWED.has(id.trim())))
}

function licenseText(dir: string): string {
  const file = readdirSync(dir).find((f) => /^(licen[cs]e|copying)(\.|$)/i.test(f))
  return file ? readFileSync(join(dir, file), 'utf8').trim() : '(no licence file in the package)'
}

const [mode, out] = process.argv.slice(2)
if (mode === 'check') {
  const packages = walk(true)
  const bad = packages.filter((p) => !allowed(p.license))
  for (const p of bad) console.error(`not allowed: ${p.name}@${p.version} ${p.license}`)
  console.log(`${packages.length} packages, ${bad.length} with a licence not allowed`)
  process.exit(bad.length === 0 ? 0 : 1)
} else if (mode === 'notices' && out) {
  const sections = walk(false).map(
    (p) => `${p.name} ${p.version} (${p.license})\n${'-'.repeat(72)}\n${licenseText(p.dir)}\n`,
  )
  writeFileSync(out, `Web UI (npm packages bundled into the UI)\n${'='.repeat(72)}\n\n${sections.join('\n')}`)
} else {
  console.error('usage: bun scripts/licenses.ts check | notices <file>')
  process.exit(2)
}
