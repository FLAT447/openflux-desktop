"use strict";

const invoke = window.__TAURI__.core.invoke;

const I18N = {
  en: {
    brand: "OpenFlux",
    profiles: "Profiles",
    status: "Status",
    engineLog: "Engine log",
    btnConnect: "Connect",
    btnDisconnect: "Disconnect",
    busy: "working…",
    btnTunOn: "TUN on",
    btnTunOff: "TUN off",
    btnProxyOn: "Proxy on",
    btnProxyOff: "Proxy off",
    langBtn: "RU",
    statusProfile: "profile",
    statusEngine: "engine",
    statusTun: "tun",
    statusProxy: "proxy",
    engineStopped: "stopped",
    engineTunRunning: "tun (pid %s)",
    engineSocksRunning: "socks5 127.0.0.1:%s (pid %s)",
    engineExitRunning: "exit node (pid %s)",
    tunUp: "up",
    tunDown: "down",
    proxyOn: "on",
    proxyOff: "off",
    proxySystem: "off (system: %s)",
    noProfiles: "no profiles; use the CLI to add one",
    profilesUnavailable: "profiles unavailable",
    addProfile: "Add profile",
    fName: "Name",
    fMode: "Mode",
    fManual: "Manual (doc URL)",
    fKey: "Key (controlplane)",
    fDocUrl: "Yandex Docs URL",
    fControlUrl: "Controlplane base URL",
    fKeyToken: "Key token",
    fPort: "SOCKS5 port (default 1080)",
    fStreams: "Streams (multistream, 1-8)",
    fDns: "DNS upstream for TUN (plain ip / tls://host / https://host/path)",
    fSplitMode: "TUN split mode",
    fSplitOff: "Off",
    fSplitExclude: "Exclude (list bypasses the tunnel)",
    fSplitInclude: "Include (only list uses it)",
    fSplitDomains: "Split domains (comma-separated, *.ya.ru)",
    btnSave: "Save",
    btnCancel: "Cancel",
    delConfirm: "Delete profile '%s'?",
    importTitle: "Import from link",
    fLink: "OpenFlux link or raw base64 payload",
    btnImport: "Import",
  },
  ru: {
    brand: "OpenFlux",
    profiles: "Профили",
    status: "Состояние",
    engineLog: "Журнал движка",
    btnConnect: "Подключить",
    btnDisconnect: "Отключить",
    busy: "работаю…",
    btnTunOn: "Вкл. TUN",
    btnTunOff: "Выкл. TUN",
    btnProxyOn: "Вкл. прокси",
    btnProxyOff: "Выкл. прокси",
    langBtn: "EN",
    statusProfile: "профиль",
    statusEngine: "движок",
    statusTun: "tun",
    statusProxy: "прокси",
    engineStopped: "остановлен",
    engineTunRunning: "tun (pid %s)",
    engineSocksRunning: "socks5 127.0.0.1:%s (pid %s)",
    engineExitRunning: "exit node (pid %s)",
    tunUp: "включён",
    tunDown: "выключен",
    proxyOn: "вкл",
    proxyOff: "выкл",
    proxySystem: "выкл (системно: %s)",
    noProfiles: "нет профилей; добавьте через CLI",
    profilesUnavailable: "профили недоступны",
    addProfile: "Добавить профиль",
    fName: "Имя",
    fMode: "Режим",
    fManual: "Ручной (URL документа)",
    fKey: "Ключ (контролплейн)",
    fDocUrl: "URL Яндекс.Документов",
    fControlUrl: "Базовый URL контролплейна",
    fKeyToken: "Ключевой токен",
    fPort: "SOCKS5-порт (по умолч. 1080)",
    fStreams: "Потоки (multistream, 1-8)",
    fDns: "DNS для TUN (ip / tls://host / https://host/path)",
    fSplitMode: "Режим разделения (TUN)",
    fSplitOff: "Выкл",
    fSplitExclude: "Исключить (список в обход тунелля)",
    fSplitInclude: "Только список ходит в туннель",
    fSplitDomains: "Домены для разделения (через запятую, *.ya.ru)",
    btnSave: "Сохранить",
    btnCancel: "Отмена",
    delConfirm: "Удалить профиль '%s'?",
    importTitle: "Импорт по ссылке",
    fLink: "Ссылка OpenFlux или raw base64",
    btnImport: "Импортировать",
  },
};

function langCurrent() {
  const saved = localStorage.getItem("of-lang");
  if (saved === "en" || saved === "ru") return saved;
  return (navigator.language || "").toLowerCase().startsWith("ru") ? "ru" : "en";
}

let lang = langCurrent();
const t = (k) => I18N[lang][k] ?? I18N.en[k] ?? k;
// minimal %s formatter: t("engineSocksRunning", port, pid)
function ts(k, ...args) {
  let s = t(k);
  args.forEach((a) => { s = s.replace("%s", String(a)); });
  return s;
}

const btnConnect = document.getElementById("btn-connect");
const btnTun = document.getElementById("btn-tun");
const btnProxy = document.getElementById("btn-proxy");
const btnLang = document.getElementById("btn-lang");
const btnRefresh = document.getElementById("btn-refresh");
const profileList = document.getElementById("profile-list");
const statusRows = document.getElementById("status-rows");
const messageEl = document.getElementById("message");
const logView = document.getElementById("log-view");
const modal = document.getElementById("modal");
const importModal = document.getElementById("modal-import");
const btnAdd = document.getElementById("btn-add");
const btnImportBtn = document.getElementById("btn-import");
const fLink = document.getElementById("f-link");
const fName = document.getElementById("f-name");
const fMode = document.getElementById("f-mode");
const fDocUrl = document.getElementById("f-doc-url");
const fControlUrl = document.getElementById("f-control-url");
const fKeyToken = document.getElementById("f-key-token");
const fPort = document.getElementById("f-port");
const fStreams = document.getElementById("f-streams");
const fDns = document.getElementById("f-dns");
const fSplitMode = document.getElementById("f-split-mode");
const fSplitDomains = document.getElementById("f-split-domains");
const fManualRow = document.getElementById("f-manual-row");
const fKeyRow = document.getElementById("f-key-row");

let selectedProfile = null;
let latestStatus = null;
let pending = false;

function setMessage(text, kind) {
  messageEl.textContent = text || "";
  messageEl.className = "message" + (kind ? " " + kind : "");
}

function pill(text, kind) {
  return `<span class="pill ${kind}">${text}</span>`;
}

async function run(label, fn) {
  pending = true;
  renderStatic();
  try {
    const msg = await fn();
    setMessage(msg, "ok");
  } catch (e) {
    setMessage(String(e), "error");
  } finally {
    pending = false;
  }
  await refresh();
}

function renderStatic() {
  for (const el of document.querySelectorAll("[data-i18n]")) {
    el.textContent = t(el.dataset.i18n);
  }
  btnLang.textContent = lang === "en" ? I18N.en.langBtn : I18N.ru.langBtn;
  if (pending) {
    btnConnect.textContent = t("busy");
    btnTun.disabled = true;
    btnProxy.disabled = true;
    return;
  }
  const running = latestStatus?.engine !== null && latestStatus?.engine !== undefined;
  btnConnect.textContent = running ? t("btnDisconnect") : t("btnConnect");
  btnTun.disabled = false;
  btnProxy.disabled = false;
  if (latestStatus) {
    btnTun.textContent = latestStatus.tun_up ? t("btnTunOff") : t("btnTunOn");
    btnProxy.textContent = latestStatus.proxy_on ? t("btnProxyOff") : t("btnProxyOn");
  }
}

async function renderProfiles() {
  let ps;
  try {
    ps = await invoke("profiles");
  } catch {
    profileList.innerHTML = `<li class="empty">${t("profilesUnavailable")}</li>`;
    return;
  }
  selectedProfile = ps.find((p) => p.active)?.name ?? (ps[0]?.name ?? null);
  if (!ps.length) {
    profileList.innerHTML = `<li class="empty">${t("noProfiles")}</li>`;
    return;
  }
  profileList.textContent = "";
  for (const p of ps) {
    const li = document.createElement("li");
    li.className = p.active ? "active" : "";
    const name = document.createElement("span");
    name.textContent = p.name + (p.active ? " ✓" : "");
    const mode = document.createElement("span");
    mode.className = "mode";
    mode.textContent = p.mode;
    li.append(name, mode);
    const del = document.createElement("button");
    del.className = "del";
    del.textContent = "✕";
    del.title = ts("delConfirm", p.name);
    del.onclick = async (e) => {
      e.stopPropagation();
      if (!confirm(ts("delConfirm", p.name))) return;
      await run(`delete '${p.name}'`, () => invoke("remove_profile", { name: p.name }));
    };
    li.append(del);
    li.onclick = async () => {
      if (p.name === selectedProfile) return;
      selectedProfile = p.name;
      await run(`activate '${p.name}'`, () => invoke("set_active", { name: p.name }));
    };
    profileList.appendChild(li);
  }
}

async function renderStatus() {
  let s;
  try {
    s = await invoke("status");
  } catch {
    return;
  }
  latestStatus = s;
  const engine =
    s.engine === null
      ? pill(t("engineStopped"), "warn")
      : s.engine.mode === "tun"
        ? pill(ts("engineTunRunning", s.engine.pid), "ok")
        : s.engine.mode === "exit"
          ? pill(ts("engineExitRunning", s.engine.pid), "ok")
          : pill(ts("engineSocksRunning", s.engine.port, s.engine.pid), "ok");
  const tun = s.tun_up ? pill(t("tunUp"), "ok") : pill(t("tunDown"), "warn");
  const proxy = s.proxy_on
    ? pill(t("proxyOn"), "ok")
    : pill(
        s.proxy_system_mode && s.proxy_system_mode !== "none"
          ? ts("proxySystem", s.proxy_system_mode)
          : t("proxyOff"),
        "warn"
      );
  statusRows.innerHTML = `
    <div class="row"><span class="k">${t("statusProfile")}</span><span>${escapeHtml(s.active_profile ?? "<none>")}</span></div>
    <div class="row"><span class="k">${t("statusEngine")}</span>${engine}</div>
    <div class="row"><span class="k">${t("statusTun")}</span>${tun}</div>
    <div class="row"><span class="k">${t("statusProxy")}</span>${proxy}</div>`;
  renderStatic();
}

function escapeHtml(v) {
  return String(v).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]
  );
}

async function renderLog() {
  try {
    const text = await invoke("log_tail", { lines: 500 });
    if (text) logView.textContent = text;
    logView.scrollTop = logView.scrollHeight;
  } catch {
    /* keep last log */
  }
}

async function refresh() {
  await Promise.all([renderProfiles(), renderStatus(), renderLog()]);
}

function openAddModal() {
  fName.value = "";
  fDocUrl.value = "";
  fControlUrl.value = "";
  fKeyToken.value = "";
  fPort.value = "";
  fStreams.value = "";
  fDns.value = "";
  fSplitMode.value = "none";
  fSplitDomains.value = "";
  fMode.value = "manual";
  fManualRow.classList.remove("hidden");
  fKeyRow.classList.add("hidden");
  modal.classList.remove("hidden");
  fName.focus();
}

function closeAddModal() {
  modal.classList.add("hidden");
}

async function saveProfile() {
  const mode = fMode.value;
  const args = {
    name: fName.value.trim(),
    mode,
    docUrl: mode === "manual" ? fDocUrl.value.trim() || null : null,
    controlUrl: mode === "key" ? fControlUrl.value.trim() || null : null,
    keyToken: mode === "key" ? fKeyToken.value.trim() || null : null,
    socksPort: fPort.value ? Number(fPort.value) : null,
    streams: fStreams.value ? Number(fStreams.value) : null,
    dnsUpstream: fDns.value.trim() || null,
    splitMode: fSplitMode.value === "none" ? "none" : fSplitMode.value,
    splitDomains: fSplitDomains.value.trim() || null,
  };
  try {
    const msg = await invoke("add_profile", args);
    closeAddModal();
    setMessage(msg, "ok");
  } catch (e) {
    setMessage(String(e), "error");
  }
  await refresh();
}

btnAdd.onclick = openAddModal;
document.getElementById("btn-modal-cancel").onclick = closeAddModal;
document.getElementById("btn-modal-save").onclick = saveProfile;
fMode.onchange = () => {
  const key = fMode.value === "key";
  fManualRow.classList.toggle("hidden", key);
  fKeyRow.classList.toggle("hidden", !key);
};
modal.onclick = (e) => {
  if (e.target === modal) closeAddModal();
};
fName.onkeydown = (e) => {
  if (e.key === "Enter") saveProfile();
};

function openImportModal() {
  fLink.value = "";
  importModal.classList.remove("hidden");
  fLink.focus();
}
function closeImportModal() {
  importModal.classList.add("hidden");
}
async function saveImport() {
  const link = fLink.value.trim();
  if (!link) {
    setMessage(t("fLink") + " …", "error");
    return;
  }
  btnImportBtn.disabled = true;
  try {
    const msg = await invoke("import_link", { link });
    closeImportModal();
    setMessage(msg, "ok");
  } catch (e) {
    setMessage(String(e), "error");
  } finally {
    btnImportBtn.disabled = false;
  }
  await refresh();
}
btnImportBtn.onclick = openImportModal;
document.getElementById("btn-import-cancel").onclick = closeImportModal;
document.getElementById("btn-import-save").onclick = saveImport;
importModal.onclick = (e) => {
  if (e.target === importModal) closeImportModal();
};

btnConnect.onclick = () => {
  if (pending) return;
  const running = latestStatus?.engine != null;
  if (running) {
    run("disconnect", () => invoke("disconnect"));
  } else {
    run("connect", () => invoke("connect", { name: selectedProfile }));
  }
};
btnTun.onclick = () => {
  if (pending) return;
  run("toggle tun", () => invoke("tun_toggle"));
};
btnProxy.onclick = () => {
  if (pending) return;
  run("toggle proxy", () => invoke("proxy_toggle"));
};
btnLang.onclick = () => {
  lang = lang === "en" ? "ru" : "en";
  localStorage.setItem("of-lang", lang);
  renderStatic();
  document.documentElement.lang = lang;
};
btnRefresh.onclick = refresh;

setInterval(refresh, 2000);
renderStatic();
document.documentElement.lang = lang;
refresh().then(async () => {
  // Surfaced once: a deep-link import result when the app was opened by an openflux:// URL.
  const notice = await invoke("take_notice");
  if (notice) setMessage(notice, notice.startsWith("openflux:") ? "error" : "ok");
});