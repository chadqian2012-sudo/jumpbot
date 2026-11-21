"use strict";

const byId = (id) => document.getElementById(id);
const primary = byId("primary");
const stop = byId("stop");
const check = byId("check");
const backend = byId("backend");
const daemon = byId("daemon");
const theme = byId("theme");
const ledger = byId("ledger");
const phase = byId("phase");
const motion = matchMedia("(prefers-reduced-motion: reduce)");
const systemTheme = matchMedia("(prefers-color-scheme: dark)");
const flight = document.querySelector(".flight");

const states = {
  starting: ["正在启动", "正在加载配置与识别后端。"],
  waiting_window: ["等待游戏窗口", "请在微信里手动打开「跳一跳」。"],
  countdown: ["启动倒计时", "倒计时结束后开始识别画面。"],
  resuming: ["正在恢复", "重新查找窗口并采集新画面。"],
  paused: ["已暂停", "现在可以切换窗口。"],
  pausing: ["正在暂停", "正在等待当前按压结束。"],
  playing: ["运行中", "正在识别棋子与目标。"],
  acquiring: ["正在识别", "正在寻找可用的游戏画面。"],
  landing: ["正在核对落点", "本次按压已释放，正在读取落点。"],
  blocked_modal: ["窗口有弹窗", "请查看游戏窗口中的提示。"],
  dead: ["本局结束", "正在处理游戏结束画面。"],
  stopped: ["已停止", "可启动下一轮。"],
  stopping: ["正在停止", "等待脚本释放鼠标并退出。"],
  selftest: ["正在自检", "自检使用合成画面。"],
  idle: ["待机", "启动后会等待你手动打开小游戏。"],
};
const backends = { cpu: "CPU", simd: "CPU SIMD", gpu: "CUDA", cuda: "CUDA", opencl: "OpenCL" };
let ready = false;
let mode = "idle";
let commandPending = null;
let state = {};
let elapsedAt = 0;
let lastPhase = "";
let lastLog = "";

function send(action, fields = {}) {
  window.ipc?.postMessage(JSON.stringify({ action, ...fields }));
}

function refreshButtons() {
  const live = mode === "live";
  const idle = mode === "idle";
  const paused = state.state === "paused";
  primary.textContent = idle ? "启动脚本" : paused ? "恢复运行" : "暂停运行";
  primary.disabled = idle ? !ready : !live || commandPending !== null ||
    state.hotkey_registered !== true || ["starting", "stopped", "pausing", "resuming"].includes(state.state);
  stop.disabled = !live;
  check.disabled = !idle || !ready;
  backend.disabled = daemon.disabled = !idle;
}

function animate(element, name) {
  if (motion.matches) return;
  element.classList.remove(name);
  void element.offsetWidth;
  element.classList.add(name);
}

function showNotice(message) {
  const notice = byId("notice");
  notice.hidden = !message;
  notice.textContent = message || "";
  if (message) animate(notice, "changed");
}

function appendLog(line) {
  if (!line || line === lastLog) return;
  lastLog = line;
  const tail = ledger.hidden || ledger.scrollHeight - ledger.clientHeight - ledger.scrollTop < 48;
  const row = document.createElement("li");
  row.textContent = line;
  ledger.append(row);
  if (ledger.children.length > 60) ledger.firstElementChild.remove();
  ledger.hidden = false;
  byId("empty").hidden = true;
  if (tail) ledger.scrollTop = ledger.scrollHeight;
}

function renderPhase(key, note) {
  const [label, explanation] = states[key] || states.starting;
  if (key !== lastPhase) {
    phase.textContent = label;
    phase.classList.toggle("is-draft", ["idle", "paused", "stopped"].includes(key));
    phase.classList.toggle("is-alert", ["blocked_modal", "countdown", "stopping"].includes(key));
    animate(phase, "changed");
    lastPhase = key;
  }
  byId("phase-note").textContent = note || explanation;
}

const number = (n) => Number.isFinite(n) ? n.toLocaleString("zh-CN") : "—";
const percentage = (n) => Number.isFinite(n) ? `${n.toFixed(1)}%` : "—";

function renderElapsed() {
  const seconds = state.active_elapsed_s;
  const elapsed = Math.floor(seconds + (state.elapsed_running ? (performance.now() - elapsedAt) / 1000 : 0));
  byId("elapsed").textContent = Number.isFinite(elapsed)
    ? [Math.floor(elapsed / 3600), Math.floor(elapsed / 60) % 60, elapsed % 60]
      .map((part) => String(part).padStart(2, "0")).join(":")
    : "—";
}

function renderStats() {
  byId("score").textContent = number(state.score);
  byId("jumps").textContent = number(state.jumps);
  byId("hit").textContent = percentage(state.hit_rate);
  byId("land").textContent = percentage(state.landing_rate);
  byId("actual-backend").textContent = backends[state.mask_backend] || "—";
  byId("kernel").textContent = state.background_kernel === "rust" ? "Rust" : "—";
  byId("shortcuts").textContent = `${state.pause_hotkey || "Alt+P"} 暂停 / F8 停止`;
  const countdown = ["countdown", "resuming"].includes(state.state) ? state.countdown_s : null;
  byId("countdown").textContent = Number.isFinite(countdown) ? `${countdown.toFixed(1)} s` : "—";
  byId("window").textContent = !["waiting_window", "starting", "stopped"].includes(state.state) && state.window?.title ||
    "窗口未连接";
  renderElapsed();
}

window.renderJumpBot = (message) => {
  switch (message.type) {
    case "ready":
      ready = !message.error;
      showNotice(message.error);
      break;
    case "started":
      mode = message.selftest ? "selftest" : "live";
      state = {};
      commandPending = null;
      lastLog = "";
      ledger.replaceChildren();
      ledger.hidden = true;
      byId("empty").hidden = false;
      showNotice("");
      renderPhase(message.selftest ? "selftest" : "starting");
      renderStats();
      break;
    case "log":
      appendLog(message.line);
      break;
    case "status": {
      const previous = state;
      state = message.status;
      elapsedAt = performance.now();
      if (commandPending !== null && state.control_generation !== commandPending) commandPending = null;
      if (mode === "live") renderPhase(state.state, state.state === "stopped" ? state.stop_reason : "");
      if (state.hotkey_error && state.hotkey_error !== previous.hotkey_error) {
        showNotice("暂停快捷键无法注册。请停止脚本，释放快捷键后再启动。");
      }
      renderStats();
      if (state.state === "landing" && previous.state !== "landing") animate(flight, "is-jumping");
      if (Number.isFinite(state.score) && state.score !== previous.score) animate(byId("score"), "changed");
      break;
    }
    case "stopping":
      mode = "stopping";
      renderPhase("stopping");
      break;
    case "exited":
      mode = "idle";
      commandPending = null;
      state.elapsed_running = false;
      if (message.selftest) {
        renderPhase("idle");
        showNotice(message.ok ? "CPU 无输入自检通过。" : "自检失败。请查看运行日志。");
      } else {
        renderPhase("stopped", state.state === "stopped" ? state.stop_reason : "");
        if (!message.ok) showNotice(`脚本退出（代码 ${message.code ?? "未知"}）。请查看运行日志。`);
      }
      break;
    case "error":
      showNotice(message.error);
      break;
  }
  refreshButtons();
};

primary.addEventListener("click", () => {
  if (mode === "idle") {
    send("start", { backend: backend.value, daemon: daemon.checked });
  } else {
    commandPending = state.control_generation;
    send(state.state === "paused" ? "resume" : "pause");
    refreshButtons();
  }
});
stop.addEventListener("click", () => send("stop"));
check.addEventListener("click", () => send("selftest"));

function renderTheme() {
  const dark = document.documentElement.dataset.theme === "dark" ||
    (!document.documentElement.dataset.theme && systemTheme.matches);
  theme.setAttribute("aria-pressed", String(dark));
  theme.textContent = dark ? "浅色模式" : "深色模式";
}
theme.addEventListener("click", () => {
  const next = theme.getAttribute("aria-pressed") === "true" ? "light" : "dark";
  const change = () => {
    document.documentElement.dataset.theme = next;
    renderTheme();
  };
  if (!motion.matches && document.startViewTransition) document.startViewTransition(change);
  else change();
});
systemTheme.addEventListener("change", renderTheme);
renderTheme();
setInterval(() => {
  if (mode === "live" && state.elapsed_running) renderElapsed();
}, 250);
send("ready");
