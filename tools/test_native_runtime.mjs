// node tools/test_native_runtime.mjs <unpacked package directory>  (Windows, Node 22+)
import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { cpSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { basename, dirname, join, relative } from "node:path";
import { setTimeout as wait } from "node:timers/promises";
import { fileURLToPath } from "node:url";

assert.equal(process.platform, "win32");
assert(process.argv[2], "Supply the published-layout directory containing build-source.json");
const original = realpathSync(process.argv[2]);
const fixtures = fileURLToPath(new URL("../tests/fixtures/", import.meta.url));
const temp = realpathSync(tmpdir());
const stage = mkdtempSync(join(temp, "jumpbot-runtime-"));
const app = join(stage, "jumpbot");
const gui = join(app, "jumpbot-desktop.exe");
const cliPath = join(app, "jumpbot.exe");
const shell = join(process.env.SystemRoot, "System32/WindowsPowerShell/v1.0/powershell.exe");
const ps = (command) => execFileSync(shell, ["-NoProfile", "-NonInteractive", "-Command", command],
  { encoding: "utf8", timeout: 15000, windowsHide: true }).trim();
const hash = (path) => createHash("sha256").update(readFileSync(path)).digest("hex");
const env = Object.fromEntries(Object.entries(process.env).filter(([key]) =>
  !/^(?:PYTHON|CONDA|JUMPBOT_|WEBVIEW2_|VIRTUAL_ENV|PATH$)/i.test(key)));
env.PATH = join(process.env.SystemRoot, "System32");
env.JUMPBOT_PYTHON = join(stage, "missing-python", "python.exe");
env.JUMPBOT_NO_INPUT = "1";
env.JUMPBOT_DISABLE_CUDA = "1";
env.JUMPBOT_DISABLE_OPENCL = "1";
env.WEBVIEW2_USER_DATA_FOLDER = join(stage, "webview");
const report = { package: original, states: [], workers: [], observedProcesses: [] };
const positive = "skin-gold-play.png", negative = "purple-horizontal-fall.png";
let child, socket;
const waitFor = async (check, label, ms = 15000) => {
  const end = Date.now() + ms;
  do {
    if (await check()) return;
    await wait(100);
  } while (Date.now() < end);
  throw new Error("Timed out waiting for " + label);
};
const running = (pid) => {
  try { process.kill(pid, 0); return true; }
  catch (error) { if (error.code === "ESRCH") return false; throw error; }
};
function checkProcessTree() {
  // Limit the returned data to descendants of this GUI. Other users of Python on
  // the host are unrelated, and their command lines are not part of the test.
  const tree = JSON.parse(ps(
    `$all=Get-CimInstance Win32_Process; $pids=@(${child.pid}); $seen=@(); ` +
    "while ($pids.Count) { $next=@($all | Where-Object { $_.ParentProcessId -in $pids -and $_.ProcessId -notin $seen }); " +
    "$pids=@($next | ForEach-Object ProcessId); $seen+=$pids }; " +
    "$tree=@($all | Where-Object { $_.ProcessId -in $seen } | Select-Object Name,ProcessId,ParentProcessId," +
    "ExecutablePath,@{Name='Worker';Expression={$_.CommandLine -match '(?:^|\\s)--worker(?:$|\\s)'}}); " +
    "ConvertTo-Json -InputObject $tree -Compress"));
  for (const proc of tree) {
    assert(!/^(?:pythonw?[\d.]*|pypy[\d.]*|py)\.exe$/i.test(proc.Name), "Python process in GUI tree: " + proc.Name);
    if (!report.observedProcesses.includes(proc.Name)) report.observedProcesses.push(proc.Name);
  }
  return tree.filter((p) => p.ParentProcessId === child.pid && p.Worker);
}
async function closeWindow() {
  assert.equal(ps(`(Get-Process -Id ${child.pid}).CloseMainWindow()`), "True");
  await waitFor(() => child.exitCode !== null, "native window close");
  assert.equal(child.exitCode, 0);
}
try {
  cpSync(original, app, { recursive: true, verbatimSymlinks: true });
  const manifest = JSON.parse(readFileSync(join(app, "build-source.json"), "utf8").replace(/^\uFEFF/, ""));
  const pythonFreeEntries = () => {
    const entries = readdirSync(app, { recursive: true, withFileTypes: true });
    for (const entry of entries) {
      assert(!entry.isSymbolicLink(), "External link in package: " + entry.name);
      assert(!/^(?:\.venv.*|venv|site-packages|__pycache__|(?:python|pypy)(?:w|[0-9.])*\.(?:exe|dll|zip))$|\.(?:py|pyw|pyc|pyo|pyd)$/i.test(entry.name),
        "Python component in the unfiltered package: " + entry.name);
    }
    return entries;
  };
  const entries = pythonFreeEntries();
  const declared = new Map(Object.entries(manifest.files_sha256).map(([name, value]) => [name.replaceAll("\\", "/"), value]));
  for (const name of ["jumpbot.exe", "jumpbot-desktop.exe", "config.json", "templates/piece.png"]) assert(declared.has(name), name);
  for (const entry of entries.filter((entry) => entry.isFile())) {
    const path = join(entry.parentPath, entry.name);
    const name = relative(app, path).replaceAll("\\", "/");
    if (name === "build-source.json") continue;
    if (name === "source-version.json") {
      const version = JSON.parse(readFileSync(path, "utf8").replace(/^\uFEFF/, ""));
      assert.equal(version.build_manifest_sha256.toLowerCase(), hash(join(app, "build-source.json")));
      assert.equal(version.commit, manifest.commit);
      continue;
    }
    if (/^(?:logs|debug)\//.test(name) && !declared.has(name)) continue;
    assert.equal(hash(path).toUpperCase(), declared.get(name)?.toUpperCase(), "Package manifest: " + name);
    declared.delete(name);
  }
  assert.equal(declared.size, 0, "Manifest lists missing package files");
  report.manifest_sha256 = hash(join(app, "build-source.json"));

  // Prove the unmodified package works before changing only the UI's private config copy.
  const cli = execFileSync(cliPath, ["--config", join(app, "config.json"), "--backend", "simd",
    "--image", join(fixtures, positive), "--image", join(fixtures, negative), "--bench-repeat", "1"],
    { cwd: stage, env, encoding: "utf8", timeout: 30000, windowsHide: true });
  const rows = cli.trim().split(/\r?\n/).map((line) => JSON.parse(line));
  assert.equal(rows.length, 2);
  for (const row of rows) assert.equal(row.backend, "simd");
  const hit = rows.find((row) => basename(row.image) === positive);
  const miss = rows.find((row) => basename(row.image) === negative);
  assert(hit?.piece && Math.abs(hit.piece.x - 173) <= 3 && Math.abs(hit.target.x - 404) <= 3);
  assert.equal(miss.piece, null);
  assert.equal(miss.target, null);
  report.cli_images = rows.length;
  const configPath = join(app, "config.json");
  const config = JSON.parse(readFileSync(configPath, "utf8").replace(/^\uFEFF/, ""));
  config.window_title_pattern = "^JumpBot QA " + randomUUID() + "$";
  config.window_exe_hint = "missing-" + randomUUID() + ".exe";
  config.window_wait_s = 120;
  writeFileSync(configPath, JSON.stringify(config));

  const server = createServer();
  await new Promise((done) => server.listen(0, "127.0.0.1", done));
  const port = server.address().port;
  await new Promise((done) => server.close(done));
  env.WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=" + port;
  child = spawn(gui, [], { cwd: app, env, windowsHide: true, stdio: "ignore" });
  let page;
  await waitFor(async () => {
    assert.equal(child.exitCode, null, "Desktop exited before opening");
    try {
      const pages = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
      page = pages.find((item) => item.type === "page" && new URL(item.url).hostname === "jumpbot.localhost");
      return page;
    } catch { return false; }
  }, "packaged WebView2", 30000);
  socket = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((done, reject) => { socket.onopen = done; socket.onerror = reject; });
  let sequence = 0;
  const pending = new Map();
  socket.onmessage = ({ data }) => {
    const reply = JSON.parse(data);
    pending.get(reply.id)?.(reply);
  };
  async function evaluate(expression) {
    const id = ++sequence;
    let timer;
    try {
      const reply = await new Promise((done, reject) => {
        timer = setTimeout(() => reject(new Error("WebView evaluation timed out")), 5000);
        pending.set(id, done);
        socket.send(JSON.stringify({ id, method: "Runtime.evaluate", params: { expression, returnByValue: true } }));
      });
      assert(!reply.error && !reply.result?.exceptionDetails, JSON.stringify(reply));
      return reply.result.result.value;
    } finally { clearTimeout(timer); pending.delete(id); }
  }
  async function click(id) {
    assert(await evaluate(`(() => {const el=document.getElementById('${id}'); if(!el || el.disabled) return false; el.click(); return true})()`), id);
  }
  async function state(key) {
    await waitFor(() => evaluate(`lastPhase === ${JSON.stringify(key)}`), key);
    const visible = await evaluate("document.getElementById('phase').textContent");
    assert(visible, "Visible phase label");
    report.states.push({ state: key, label: visible });
    return checkProcessTree();
  }
  await waitFor(() => evaluate("Boolean(document.querySelector('#primary:enabled'))"), "start button");
  await evaluate("document.querySelector('#backend').value='simd'");
  for (const operation of ["stop", "close"]) {
    await click("primary");
    const active = await state("waiting_window");
    assert.equal(active.length, 1, "Expected exactly one native worker");
    const worker = active[0];
    report.workers.push(worker);
    assert.equal(worker.ExecutablePath.toLowerCase(), cliPath.toLowerCase(), "Expected the packaged sibling jumpbot.exe --worker");
    if (operation === "stop") {
      const modules = JSON.parse(ps(`$m=@((Get-Process -Id ${worker.ProcessId}).Modules | ForEach-Object FileName); ConvertTo-Json -InputObject $m -Compress`));
      assert(modules.length && !modules.some((path) => /(?:python|pypy)[0-9.]*\.dll$/i.test(basename(path))), "Unexpected Python in worker");
      assert(modules.some((path) => dirname(path).toLowerCase() === app.toLowerCase() && /^opencv_world\d+\.dll$/i.test(basename(path))), "Worker did not load the packaged OpenCV DLL");
      await click("primary"); await state("paused");
      await click("primary"); await state("waiting_window");
      await click("stop"); await state("stopped");
      await waitFor(() => evaluate("Boolean(document.querySelector('#primary:enabled'))"), "stopped UI");
    } else {
      socket.close();
      await closeWindow();
      report.states.push("已关闭");
    }
    await waitFor(() => !running(worker.ProcessId), operation + " to exit the worker");
  }
  pythonFreeEntries();
  console.log(JSON.stringify(report, null, 2));
} catch (error) {
  console.error(JSON.stringify(report, null, 2));
  throw error;
} finally {
  socket?.close();
  if (child?.pid && child.exitCode === null) {
    try { await closeWindow(); } catch { child.kill(); }
  }
  // Remove only the uniquely created temporary copy, after the native processes exit.
  assert.equal(dirname(realpathSync(stage)), temp);
  assert(basename(stage).startsWith("jumpbot-runtime-"));
  rmSync(stage, { recursive: true, maxRetries: 8, retryDelay: 250 });
}
