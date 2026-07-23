import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { resolve } from "node:path";

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const read = (path) => readFileSync(resolve(root, path), "utf8");
const fail = (message) => {
  console.error(`version alignment failed: ${message}`);
  process.exitCode = 1;
};
const git = (...args) =>
  execFileSync("git", args, { cwd: root, encoding: "utf8" }).trim();

const cargo = read("Cargo.toml");
const workspaceVersion = cargo.match(
  /\[workspace\.package\][\s\S]*?\nversion\s*=\s*"([^"]+)"/,
)?.[1];

if (!workspaceVersion) {
  fail("Cargo.toml has no [workspace.package].version");
  process.exit();
}
if (!/^0\.0\.[1-9]\d*$/.test(workspaceVersion)) {
  fail(`default release line must be 0.0.N (N >= 1), got ${workspaceVersion}`);
}

for (const manifest of ["client/Cargo.toml", "server/Cargo.toml", "updater/Cargo.toml", "ops/Cargo.toml"]) {
  if (!/^version\.workspace\s*=\s*true$/m.test(read(manifest))) {
    fail(`${manifest} must inherit version.workspace = true`);
  }
}

for (const manifest of [
  "package.json",
  "client/frontend/package.json",
  "server/frontend/package.json",
  "shared/package.json",
]) {
  const value = JSON.parse(read(manifest)).version;
  if (value !== workspaceVersion) {
    fail(`${manifest} is ${String(value)}, expected ${workspaceVersion}`);
  }
}

const lock = read("Cargo.lock");
for (const name of ["nuntius-client", "nuntius-server", "nuntius-updater", "nuntius-ops"]) {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const version = lock.match(
    new RegExp(`name = "${escaped}"\\nversion = "([^"]+)"`),
  )?.[1];
  if (version !== workspaceVersion) {
    fail(`Cargo.lock ${name} is ${String(version)}, expected ${workspaceVersion}`);
  }
}

const bunLock = read("bun.lock");
for (const workspace of ["client/frontend", "server/frontend", "shared"]) {
  const escaped = workspace.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const version = bunLock.match(
    new RegExp(`"${escaped}": \\{[\\s\\S]*?"version": "([^"]+)"`),
  )?.[1];
  if (version !== workspaceVersion) {
    fail(`bun.lock ${workspace} is ${String(version)}, expected ${workspaceVersion}`);
  }
}

const productPath = (path) =>
  /^(Cargo\.(toml|lock)|package\.json|bun\.lock)$/.test(path) ||
  /^(client|server|shared|updater|ops)\/(src\/|frontend\/src\/|Cargo\.toml|package\.json|api\/openapi\.yaml)/.test(
    path,
  );

try {
  const worktreeChanges = git("diff", "--name-only", "HEAD")
    .split("\n")
    .filter(Boolean);
  const base = worktreeChanges.length > 0 ? "HEAD" : "HEAD^";
  const changedPaths =
    worktreeChanges.length > 0
      ? worktreeChanges
      : git("diff", "--name-only", base, "HEAD").split("\n").filter(Boolean);
  const previousCargo = git("show", `${base}:Cargo.toml`);
  const previousVersion = previousCargo.match(
    /\[workspace\.package\][\s\S]*?\nversion\s*=\s*"([^"]+)"/,
  )?.[1];
  const previousPatch = previousVersion?.match(/^0\.0\.([1-9]\d*)$/);
  if (changedPaths.some(productPath) && previousPatch) {
    const expected = `0.0.${Number(previousPatch[1]) + 1}`;
    if (workspaceVersion !== expected) {
      fail(
        `product changes require one patch increment from ${previousVersion} to ${expected}, got ${workspaceVersion}`,
      );
    }
  }
} catch {
  console.warn("version alignment warning: Git parent unavailable; patch increment check skipped");
}

if (!process.exitCode) {
  console.log(`Nuntius product version ${workspaceVersion} is aligned`);
}
