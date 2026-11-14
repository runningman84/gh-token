#!/usr/bin/env node
/**
 * Bump the project version in all versioned files.
 * Invoked by semantic-release via @semantic-release/exec with the next
 * release version as the first argument.
 */
import { readFileSync, writeFileSync } from "node:fs";

const version = process.argv[2];
if (!/^\d+\.\d+\.\d+$/.test(version ?? "")) {
  console.error(`invalid version: ${version}`);
  process.exit(1);
}

function fail(msg) {
  console.error(msg);
  process.exit(1);
}

// 1. Workspace Cargo.toml: `version = "..."` inside [workspace.package].
{
  const file = "Cargo.toml";
  const src = readFileSync(file, "utf8");
  const idx = src.indexOf("[workspace.package]");
  if (idx === -1) fail("no [workspace.package] section in Cargo.toml");
  const before = src.slice(0, idx);
  const after = src
    .slice(idx)
    .replace(/(version\s*=\s*")[^"]+(")/, `$1${version}$2`);
  if (after === src.slice(idx)) fail("failed to update version in Cargo.toml");
  writeFileSync(file, before + after);
  console.log(`updated workspace version in Cargo.toml -> ${version}`);
}

// 2. Helm chart: version + appVersion.
{
  const file = "chart/Chart.yaml";
  const src = readFileSync(file, "utf8");
  const out = src
    .replace(/^(version:).*/m, `version: ${version}`)
    .replace(/^(appVersion:).*/m, `appVersion: "${version}"`);
  if (out === src) fail(`failed to update version in ${file}`);
  writeFileSync(file, out);
  console.log(`updated chart version/appVersion in ${file} -> ${version}`);
}

// 3. Helm chart values: image tag.
{
  const file = "chart/values.yaml";
  const src = readFileSync(file, "utf8");
  const out = src.replace(/(^  tag: ").*(")/m, `$1${version}$2`);
  if (out === src) fail(`failed to update image tag in ${file}`);
  writeFileSync(file, out);
  console.log(`updated image tag in ${file} -> ${version}`);
}

// 4. package.json version.
{
  const file = "package.json";
  const pkg = JSON.parse(readFileSync(file, "utf8"));
  pkg.version = version;
  writeFileSync(file, `${JSON.stringify(pkg, null, 2)}\n`);
  console.log(`updated version in ${file} -> ${version}`);
}

console.log(`version bump complete: ${version}`);
