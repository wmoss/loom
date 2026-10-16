import test from 'node:test';
import assert from 'node:assert/strict';
import { isLockFile } from './lockFiles.ts';

test('recognizes known lock files anywhere in the tree', () => {
  for (const path of [
    'package-lock.json',
    'crates/loom/frontend/package-lock.json',
    'uv.lock',
    'python/uv.lock',
    'Cargo.lock',
    'e2e/pnpm-lock.yaml',
    'infra/.terraform.lock.hcl',
    'gradle/gradle.lockfile',
  ]) {
    assert.equal(isLockFile(path), true, path);
  }
});

test('leaves hand-edited manifests and look-alikes open', () => {
  for (const path of [
    'package.json',
    'go.mod',
    'requirements.txt',
    'Cargo.toml',
    'pyproject.toml',
    'my-package-lock.json',
    'package-lock.jsonx',
    'lockfile.rs',
  ]) {
    assert.equal(isLockFile(path), false, path);
  }
});
