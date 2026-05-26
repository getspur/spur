import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const juteDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const spurNotebookDir = resolve(juteDir, "..");
const tauriBin = process.platform === "win32" ? "tauri.cmd" : "tauri";
const tauriCli = resolve(juteDir, "node_modules", ".bin", tauriBin);
const env = {
  ...process.env,
  INIT_CWD: spurNotebookDir,
  npm_config_local_prefix: spurNotebookDir,
  PWD: spurNotebookDir,
};

const result = spawnSync(tauriCli, process.argv.slice(2), {
  cwd: spurNotebookDir,
  env,
  stdio: "inherit",
});

if (result.error) {
  console.error(`failed to run ${tauriCli}: ${result.error.message}`);
  process.exit(1);
}

process.exit(result.status ?? 1);
