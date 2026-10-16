// Machine-authored dependency lock files: their diffs are bulk-generated
// noise a reviewer almost never reads line by line, so the review surface
// starts them collapsed. The set is basenames only — an exact, reviewable
// list — and hand-edited manifests (`package.json`, `go.mod`,
// `requirements.txt`, `Cargo.toml`) deliberately stay out of it.
const LOCK_FILE_NAMES: ReadonlySet<string> = new Set([
  // JavaScript & TypeScript
  'package-lock.json',
  'npm-shrinkwrap.json',
  'yarn.lock',
  'pnpm-lock.yaml',
  'bun.lock',
  'bun.lockb',
  // Rust
  'Cargo.lock',
  // Python
  'uv.lock',
  'poetry.lock',
  'Pipfile.lock',
  // Go
  'go.sum',
  // Ruby, PHP, Dart, Elixir, Haskell
  'Gemfile.lock',
  'composer.lock',
  'pubspec.lock',
  'mix.lock',
  'cabal.project.freeze',
  // JVM, NuGet, Nix, Terraform, Swift
  'gradle.lockfile',
  'packages.lock.json',
  'flake.lock',
  '.terraform.lock.hcl',
  'Package.resolved',
]);

/** True when the path names a known dependency lock, anywhere in the tree. */
export function isLockFile(path: string): boolean {
  return LOCK_FILE_NAMES.has(path.slice(path.lastIndexOf('/') + 1));
}
