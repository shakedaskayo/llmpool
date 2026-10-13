// llmpool window. Talks to the daemon through one Tauri command, `api`,
// which forwards to the daemon's HTTP API.

const invoke = window.__TAURI__.core.invoke;
const api = (method, path, body) => invoke("api", { method, path, body: body ?? null });
// The same interface is served by a team's server to its administrators.
const SERVER = window.LLMPOOL_HOST === "server";

const state = {
  accounts: [],
  sessions: [],
  events: [],
  issues: [],
  stats: {},
  config: {},
  team: { state: "off" },
  report: null, // the gateway's requests and what they add up to, on a server's page
  reportFilter: { who: "", provider: "", account: "", result: "" },
  server: {}, // what a team's server says about itself, when this page is its own
  agents: { harnesses: [] },
  providers: [],
  adding: false, // whether the add-account dialog is open
  addKind: "claude", // which kind of account the add panel shows: claude, agent or key
  keyProvider: "openai",
  remotePolicy: {},
  loginItem: null, // true, false, or null while unknown
  setupLog: "",
  login: { state: "idle" },
  schema: { triggers: [], conditions: [], picks: [], home: "" },
  policy: { settings: {}, rule: [] },
  policyText: "",
  policyView: "rules", // or "toml"
  policyError: null,
  results: {}, // rule index -> { text, fired }
  dirty: false,
  offline: null, // message while the daemon cannot be reached
};

const $ = (id) => document.getElementById(id);
const esc = (value) =>
  String(value ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
const options = (values, selected, labels = {}) =>
  values.map((v) => `<option value="${esc(v)}"${v === selected ? " selected" : ""}>${esc(labels[v] ?? v)}</option>`).join("");
const plural = (n, word) => `${n} ${word}${n === 1 ? "" : "s"}`;
const short = (path) => (state.schema.home && path.startsWith(state.schema.home) ? "~" + path.slice(state.schema.home.length) : path);

// Activity lines as a person would say them: no prefix, home as `~`.
const tidy = (message) => (state.schema.home ? message.replaceAll(state.schema.home, "~") : message).replace(/^llmpool: /, "");

// How long something took, in the unit a person would say it in.
const took = (ms) => (ms == null ? "–" : ms >= 1000 ? `${(ms / 1000).toFixed(1)} s` : `${Math.round(ms)} ms`);

// A browser's own words for a dead connection help nobody.
const unreachable = (e) => `⚠ ${/failed to fetch|networkerror|load failed/i.test(String(e)) ? "Cannot reach llmpool" : String(e).replace(/^Error: /, "")}`;

// ---- feedback ---------------------------------------------------------------

function toast(text, kind = "ok") {
  const el = document.createElement("div");
  el.className = `toast ${kind}`;
  el.textContent = String(text).replace(/^Error: /, "");
  // What went wrong stays until it has been read; a press puts it away.
  if (kind === "error") {
    el.setAttribute("role", "alert");
    el.tabIndex = 0;
    el.title = "Dismiss";
    el.addEventListener("click", () => el.remove());
    el.addEventListener("keydown", (e) => ["Enter", " ", "Escape"].includes(e.key) && el.remove());
  }
  $("toasts").append(el);
  setTimeout(() => el.remove(), kind === "error" ? 60000 : 4000);
}

async function attempt(work, success) {
  try {
    await work();
    if (success) toast(success);
    return true;
  } catch (e) {
    toast(e, "error");
    return false;
  }
}

async function copy(text) {
  // Browsers only hand out the clipboard over https; say what to do instead.
  if (!navigator.clipboard) return toast("This page cannot reach the clipboard here: select the text and copy it.", "error");
  await attempt(() => navigator.clipboard.writeText(text), "Copied");
}

// ---- usage ------------------------------------------------------------------

function resetsIn(at) {
  if (!at) return "";
  const minutes = Math.floor((at * 1000 - Date.now()) / 60000);
  if (minutes <= 0) return "";
  const days = Math.floor(minutes / 1440);
  const hours = Math.floor((minutes % 1440) / 60);
  return "resets in " + (days > 0 ? `${days}d ${hours}h` : hours > 0 ? `${hours}h ${minutes % 60}m` : `${minutes}m`);
}

// A level is always a colour and a word, never a colour alone.
const levelOf = (p) => (p >= 100 ? ["bad", "limit reached"] : p >= 90 ? ["bad", "near limit"] : p >= 70 ? ["warn", "filling up"] : ["", "plenty left"]);

function meter(usage, name, title) {
  const w = usage?.windows?.[name];
  if (!w) return `<span class="muted">no reading yet</span>`;
  const p = Math.round(w.used_percent);
  const [level, word] = levelOf(p);
  const reset = resetsIn(w.resets_at);
  return `<div class="meter ${level}" role="meter" aria-valuemin="0" aria-valuemax="100" aria-valuenow="${Math.min(100, p)}"
      aria-label="${esc(title)}: ${p}% used, ${word}${reset ? ", " + reset : ""}">
    <div class="track"><div class="fill" style="width:${Math.min(100, p)}%"></div></div>
    <span class="value">${p}%</span><small>${esc(reset || word)}</small></div>`;
}

// Usage over time as a small line: enough to see a climb or a reset at a glance.
function sparkline(history, label) {
  const points = (history ?? []).slice(-24).map((p) => p[1]);
  if (points.length < 2) return "";
  const w = 80, h = 22;
  const step = w / (points.length - 1);
  const path = points.map((v, i) => `${i ? "L" : "M"}${(i * step).toFixed(1)} ${(h - 1 - (Math.min(100, v) / 100) * (h - 2)).toFixed(1)}`).join(" ");
  const last = Math.round(points[points.length - 1]);
  const [level] = levelOf(last);
  return `<svg class="spark ${level}" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}" role="img" aria-label="${esc(label)}: from ${Math.round(points[0])}% to ${last}% over the last ${points.length} readings">
    <path d="${path}" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" stroke-linecap="round"/></svg>`;
}

// ---- pieces every page uses ---------------------------------------------------

const pageHead = (title, sub, actions = "") =>
  `<header class="head"><div><h2>${title}</h2><p class="muted">${sub}</p></div><div class="actions">${actions}</div></header>`;

// A stable colour per provider or agent, so the eye learns which is which.
const hue = (text) => [...String(text)].reduce((h, c) => (h * 31 + c.charCodeAt(0)) % 360, 7);
// The provider's or agent's own mark where there is one; its initial otherwise.
const avatar = (label, key) => {
  const mark = window.LLMPOOL_ICONS?.[key];
  const inner = mark
    ? `<svg viewBox="0 0 24 24" width="17" height="17" fill="currentColor" fill-rule="evenodd"><path d="${mark}"/></svg>`
    : esc((String(label).replace(/^[a-z-]+:/, "")[0] ?? "?").toUpperCase());
  return `<span class="avatar${mark ? " brand" : ""}" style="--h:${hue(key)}" aria-hidden="true">${inner}</span>`;
};

// One usage window with its name in front, for cards that show two.
const windowRow = (usage, name, title) => `<div class="win"><span class="cap">${name}</span>${meter(usage, name, title)}</div>`;

// ---- pool -------------------------------------------------------------------

// A login runs out on a known day; say so while there is still time to act.
function expiryBadge(a) {
  if (!a.login_expires_at || a.health !== "ok") return "";
  const days = Math.floor((a.login_expires_at * 1000 - Date.now()) / 86400000);
  if (days > 7) return "";
  return `<span class="badge warn">⚠ login expires ${days <= 0 ? "today" : `in ${plural(days, "day")}`}</span>`;
}

function accountBadge(a) {
  if (a.shared && a.health !== "ok") return `<span class="badge">team</span> <span class="badge warn">in use by a teammate</span>`;
  if (a.shared) return `<span class="badge">team</span>`;
  if (!a.enabled) return `<span class="badge">⏸ disabled</span>`;
  if (a.health !== "ok") return `<span class="badge bad">⚠ needs a new login</span>`;
  if (a.is_default) return `<span class="badge ok">default login</span>`;
  return "";
}

// What is missing, each with the button that fixes it.
function issuesPanel() {
  // A server has no agent of its own to be connected to.
  if (SERVER) return "";
  if (state.login.state === "waiting") {
    const url = state.login.url ? `<p class="muted">If no page opened: <a href="#" data-act="copy" data-text="${esc(state.login.url)}">copy the sign-in link</a></p>` : "";
    return `<div class="callout" role="status"><strong>◌ Waiting for you to finish signing in in your browser…</strong>${url}
      <div class="copy">
        <input class="wide" id="login-code" placeholder="code, only if the page shows one" aria-label="Sign-in code, only if the page shows one" />
        <button data-act="login-code">Send code</button>
        <button class="quiet" data-act="login-cancel">Cancel the sign-in</button>
      </div></div>`;
  }
  return state.issues
    .map((issue, i) => {
      const label = { add_current: "Add this login to the pool", login: issue.email ? `Sign in as ${issue.email}` : "Sign in" }[issue.action];
      const button = label ? `<div class="copy"><button class="primary" data-act="fix" data-i="${i}">${esc(label)}</button></div>` : "";
      const command = issue.action === "install" ? `<div class="copy"><button class="primary" data-act="goto-setup">Open Settings to set it up</button></div>` : "";
      // Not being set up yet is a step to take, not something gone wrong.
      if (issue.action === "install") return `<div class="callout" role="status"><strong>${esc(issue.text).replace(/`([^`]+)`/g, "<code>$1</code>")}</strong>${command}</div>`;
      return `<div class="callout bad" role="alert"><strong>⚠ ${esc(issue.text).replace(/`([^`]+)`/g, "<code>$1</code>")}</strong>${button}${command}</div>`;
    })
    .join("");
}

const AGENT_NAMES = { claude: "Claude Code" };
const agentTitle = (name) => AGENT_NAMES[name] ?? state.agents.harnesses.find((h) => h.name === name)?.title ?? state.providers.find((p) => p.name === name)?.title ?? name;
const SECRET_VAR = /(_KEY|_TOKEN|_ACCESS_KEY_ID)$/;

function addClaude() {
  const hasDefault = state.accounts.some((a) => a.is_default);
  const waiting = state.login.state === "waiting";
  return `<div class="steps">
      <div>
        <h3>Sign in to another account</h3>
        <p class="muted">Opens Claude's sign-in page in your browser. llmpool never sees your password; it takes the login once you are done.</p>
        <div class="copy">
          <input class="wide" id="login-email" type="email" placeholder="email (optional)" aria-label="Email of the account to sign in to, optional" />
          <input class="tags" id="login-tags" placeholder="tags, e.g. work" aria-label="Tags for the new account, comma separated, optional" />
          <button class="primary" data-act="login"${waiting ? " disabled" : ""}>Sign in…</button>
        </div>
      </div>
      <div>
        <h3>The login Claude Code uses now</h3>
        <p class="muted">${hasDefault ? "Already in the pool. Press again after logging Claude Code in to a different account." : "One press. Nothing about your Claude Code setup changes."}</p>
        <div class="copy"><button data-act="capture">Add current login</button></div>
      </div>
    </div>`;
}

// Agents that keep a login of their own: take the one each has now.
function addAgent() {
  const rows = state.agents.harnesses
    .filter((h) => h.has_login)
    .map((h) => {
      const who = h.login_as ? ` as <strong>${esc(h.login_as)}</strong>` : "";
      const status = !h.logged_in
        ? `<span class="muted">${h.installed ? "not signed in" : "not found on this Mac"}</span>`
        : h.login_in_pool
          ? `<span class="ok" aria-hidden="true">✓</span> signed in${who}, in the pool`
          : `signed in${who} <button class="primary" data-act="add-agent" data-name="${esc(h.name)}">Add this login</button>`;
      const more = h.installed ? `<a href="#" class="muted" data-act="copy" data-text="llmpool add login ${esc(h.name)} --new">copy the command for a second account</a>` : "";
      return `<tr><td><strong>${esc(h.title)}</strong></td><td>${status}</td><td class="actions">${more}</td></tr>`;
    })
    .join("");
  return `<p class="muted">Agents that sign in themselves keep that login in their own files. llmpool takes a copy, and starts each session on the account you or your rules pick, with the rest of the agent's settings and history shared.</p>
    <table><caption class="skip">Agents with a login of their own</caption><tbody>${rows}</tbody></table>
    <p class="muted">An agent that is not listed: describe it in <code>harnesses.toml</code> (see <code>llmpool agents</code>), or give it an API key.</p>`;
}

// A field with its name above it, where it stays once the field is filled.
const field = (label, control) => `<label class="field"><span>${esc(label)}</span>${control}</label>`;

// A key for any provider; the agent reads it from its environment.
function addKey() {
  const known = state.providers.find((p) => p.name === state.keyProvider);
  const options = state.providers.map((p) => `<option value="${esc(p.name)}"${p.name === state.keyProvider ? " selected" : ""}>${esc(p.title)}</option>`).join("");
  const fields = known
    ? known.env.map((name) => field(name, `<input class="wide keyvar" data-var="${esc(name)}" type="${SECRET_VAR.test(name) ? "password" : "text"}" autocomplete="off" />`)).join("")
    : `${field("Provider", `<input class="tags" id="key-provider" placeholder="e.g. together" />`)}
       ${field("Variable the agent reads", `<input class="wide" id="key-var" placeholder="e.g. TOGETHER_API_KEY" />`)}
       ${field("Key", `<input class="wide" id="key-value" type="password" autocomplete="off" />`)}`;
  return `<p class="muted">Works with any agent that reads its key from the environment (Codex CLI, OpenCode, Gemini CLI, Grok, Aider, your own scripts). The key is stored in your keychain and handed to the agent llmpool starts.</p>
    <div class="copy">
      ${field("Provider", `<select data-act="key-provider">${options}<option value=""${known ? "" : " selected"}>Another provider…</option></select>`)}
      ${fields}
    </div>
    <div class="copy">
      ${field("Name (optional)", `<input class="tags" id="key-label" />`)}
      ${field("Tags (optional)", `<input class="tags" id="key-tags" placeholder="e.g. work" />`)}
      <button class="primary" data-act="add-key">Add key</button>
    </div>`;
}

// A subscription login is made by signing in with the agent itself, which
// lives on a person's machine: one command there puts it straight in here.
function addFromMachine() {
  const address = state.server.public_url ?? location.origin;
  const line = (text) => `<div class="copy"><code>${esc(text)}</code><button class="quiet" data-act="copy" data-text="${esc(text)}">Copy</button></div>`;
  return `<p class="muted">Run this on a machine with llmpool and the agent installed. It signs in to the account in your browser and puts the login straight into this pool. Nothing is exported, and no file changes hands.</p>
    ${line(`llmpool team connect ${address}`)}
    ${line("llmpool add claude --to-team")}
    <p class="muted">An account that is already in your own llmpool: ${`<code>llmpool team share &lt;account&gt;</code>`}, or <strong>Share with team</strong> on the account in the app. Another agent's login: <code>llmpool add login codex --new --to-team</code>. The account appears here as soon as it arrives.</p>`;
}

// Logins cannot be signed in to on a server; they arrive as an exported file.
function addFile() {
  return `<p class="muted">For moving a whole pool at once. On a machine that has it, run <code>llmpool export pool.age</code> and choose a passphrase, then pick that file here. It is opened on the server and never stored as a file.</p>
    <div class="copy">
      <input id="import-file" type="file" aria-label="Exported pool file" />
      <input class="wide" id="import-pass" type="password" autocomplete="off" placeholder="passphrase" aria-label="Passphrase of the file" />
      <button class="primary" data-act="import-file">Import</button>
    </div>`;
}

function addPanel() {
  // Signing an agent in happens on a person's own machine; a server takes
  // keys directly and logins as a file exported from such a machine.
  const kinds = SERVER
    ? [
        ["key", "API key · any provider"],
        ["machine", "A login, from your machine"],
        ["file", "A pool file"],
      ]
    : [
        ["claude", "Claude Code login"],
        ["agent", "Another agent's login"],
        ["key", "API key · any provider"],
      ];
  if (!kinds.some(([kind]) => kind === state.addKind)) state.addKind = kinds[0][0];
  const body = { claude: addClaude, agent: addAgent, key: addKey, file: addFile, machine: addFromMachine }[state.addKind]();
  return `<div class="callout">
    <div role="group" aria-label="Kind of account" class="seg">${kinds.map(([kind, label]) => `<button data-act="add-kind" data-kind="${kind}" aria-pressed="${state.addKind === kind}">${label}</button>`).join("")}</div>
    ${body}
  </div>`;
}

function statTiles() {
  const logins = state.accounts.filter((a) => a.provider === "claude" && a.enabled && a.health === "ok");
  const near = logins.filter((a) => Object.values(a.usage?.windows ?? {}).some((w) => w.used_percent >= 90)).length;
  const free = state.stats.headroom_5h;
  const tile = (value, label, level = "") => `<div class="tile ${level}"><strong>${value}</strong><span>${label}</span></div>`;
  return `<div class="tiles">
    ${tile(state.accounts.length, state.accounts.length === 1 ? "account" : "accounts")}
    ${tile(state.sessions.length, state.sessions.length === 1 ? "session" : "sessions")}
    ${free == null ? tile("–", "5-hour allowance free") : tile(`${Math.round(free)}%`, "5-hour allowance free", free < 15 ? "bad" : free < 35 ? "warn" : "ok")}
    ${tile(near, "near the limit", near ? "warn" : "")}
    ${tile(state.stats.switches_24h ?? 0, "switches, last 24 h")}
  </div>`;
}

function accountCard(a) {
  const claude = a.provider === "claude";
  const live = a.enabled && a.health === "ok";
  const used = a.sessions.length ? `in use by ${plural(a.sessions.length, "session")}` : "idle";
  const kind = claude ? "" : a.kind === "oauth" ? `login of ${esc(agentTitle(a.provider))} · ` : "key · ";
  const tags = a.shared
    ? a.tags.map((t) => `<span class="chip">${esc(t)}</span>`).join("")
    : `<input class="tags" data-act="tags" data-id="${esc(a.id)}" value="${esc(a.tags.join(", "))}" placeholder="add tags" aria-label="Tags for ${esc(a.label)}, comma separated" />`;
  const usage = claude
    ? `${windowRow(a.usage, "5h", "5-hour window")}${windowRow(a.usage, "7d", "7-day window")}`
    : `<span class="muted">usage is not reported to llmpool</span>`;
  const actions = a.shared
    ? `<span class="muted">managed by your team</span>`
    : `${a.health !== "ok" && !SERVER ? `<button class="primary" data-act="relogin" data-id="${esc(a.id)}" aria-label="Sign in again as ${esc(a.label)}">Sign in again</button>` : ""}
       ${!SERVER && state.team.state === "connected" && state.team.admin ? `<button class="quiet" data-act="share" data-id="${esc(a.id)}" ${a.kind === "oauth" ? `data-confirm="Move it to the team?"` : ""} title="${a.kind === "oauth" ? "Moves this login to the team's pool; you then get it from there" : "Copies this key to the team's pool"}" aria-label="Add ${esc(a.label)} to the team's pool">Share with team</button>` : ""}
       <button class="quiet" data-act="toggle" data-id="${esc(a.id)}" aria-label="${a.enabled ? "Disable" : "Enable"} ${esc(a.label)}">${a.enabled ? "Disable" : "Enable"}</button>
       <button class="quiet danger" data-act="remove" data-id="${esc(a.id)}" data-confirm="Really remove?" aria-label="Remove ${esc(a.label)}">Remove</button>`;
  return `<li class="card acct${live ? "" : " off"}">
      ${avatar(a.label, a.provider)}
      <div class="who"><div class="name"><strong>${esc(a.label)}</strong> ${accountBadge(a)} ${expiryBadge(a)}</div>
        <div class="sub">${kind}${used}</div></div>
      <div class="tagcell">${tags}</div>
      <div class="usage">${usage}</div>
      <div class="trend">${claude ? sparkline(a.history, `5-hour usage of ${a.label}`) : ""}</div>
      <div class="actions">${actions}</div>
    </li>`;
}

function renderPool() {
  if (state.login.state === "waiting") state.adding = false;
  const groups = new Map();
  for (const a of state.accounts) groups.set(a.provider, [...(groups.get(a.provider) ?? []), a]);
  const order = [...groups.keys()].sort((x, y) => (y === "claude") - (x === "claude") || x.localeCompare(y));
  const lists = order
    .map((provider) => `<section class="group" aria-label="${esc(agentTitle(provider))} accounts">
        <h3>${esc(agentTitle(provider))} <span class="count">${groups.get(provider).length}</span></h3>
        <ul class="cards">${groups.get(provider).map(accountCard).join("")}</ul></section>`)
    .join("");
  $("pool").innerHTML = `${pageHead("Accounts", SERVER ? "What this server lends to its members." : "Every login and key in your pool, and how much of each is used.", `<button class="primary" data-act="add-open">Add account</button>`)}
    ${issuesPanel()}
    ${
      lists
        ? `${statTiles()}${lists}<p class="muted foot">Tags group accounts for rules, e.g. “use an account tagged work in ~/work”. Each agent only ever runs on accounts of its own provider.</p>`
        : `<div class="empty"><h3>Your pool is empty</h3><p>Add a login your coding agent already has, or a key for any provider.</p><button class="primary" data-act="add-open">Add account</button></div>`
    }
    <dialog id="add" aria-labelledby="add-title">
      <header><h3 id="add-title">Add an account</h3><button class="quiet" data-act="add-close" aria-label="Close">✕</button></header>
      ${addPanel()}
    </dialog>`;
  const dialog = $("add");
  if (state.adding && !dialog.open) dialog.showModal();
}

// ---- sessions ---------------------------------------------------------------

const EVENT_ICONS = { swap: "⇄", assign: "▶", adopt: "▶", end: "■", notice: "ℹ", login: "⚠", policy_error: "⚠", agent: "·" };

// A server's Requests page: what went through the gateway, who sent it, on
// which account, and what the decision model had to say.
function renderRequests() {
  const r = state.report;
  if (!state.server.settings?.gateway) {
    return ($("sessions").innerHTML = `${pageHead("Requests", "What members' agents send through this server.")}
      <div class="empty"><h3>The gateway is off</h3><p>Turn it on and members' agents send their requests here, where each is put on an account from the pool and recorded.</p><button class="primary" data-act="goto-server">Open the Server page</button></div>`);
  }
  if (!r) return ($("sessions").innerHTML = pageHead("Requests", "Loading…"));
  const f = state.reportFilter;
  const n = (x) => Number(x ?? 0).toLocaleString();
  const t = r.totals;
  const tile = (value, label, level = "") => `<div class="tile ${level}"><strong>${value}</strong><span>${label}</span></div>`;
  const peak = Math.max(1, ...r.per_minute);
  const bars = r.per_minute.map((v, i) => `<rect x="${i * 10 + 1}" y="${44 - Math.round((v / peak) * 40)}" width="8" height="${Math.max(v ? 2 : 0, Math.round((v / peak) * 40))}" rx="1.5"><title>${v} request${v === 1 ? "" : "s"}, ${59 - i} min ago</title></rect>`).join("");
  const pick = (name, label, values) => `<label class="filter">${label} <select data-act="report-filter" data-name="${name}"><option value="">all</option>${values.map((v) => `<option value="${esc(v)}"${f[name] === v ? " selected" : ""}>${esc(v)}</option>`).join("")}</select></label>`;
  const share = (rows, title, isMember) => `<div class="card pad"><h3>${title}</h3>${
    rows.length
      ? `<table><thead><tr><th scope="col">${isMember ? "Member" : "Account"}</th><th scope="col">Requests</th><th scope="col">Tokens in</th><th scope="col">Tokens out</th><th scope="col">Failed</th></tr></thead><tbody>${rows
          .map((x) => `<tr><td>${isMember ? esc(x.name) : `<span class="agentname">${avatar(x.provider, x.provider)}${esc(x.name)}</span>`}</td><td>${n(x.requests)}<span class="sharebar"><i style="width:${Math.round((x.requests / Math.max(1, t.requests)) * 100)}%"></i></span></td><td class="muted">${n(x.input_tokens)}</td><td class="muted">${n(x.output_tokens)}</td><td class="${x.failed ? "warnmark" : "muted"}">${n(x.failed)}</td></tr>`)
          .join("")}</tbody></table>`
      : `<span class="muted">Nothing yet.</span>`
  }</div>`;
  const rows = r.requests
    .map((x) => `<tr><td class="muted">${new Date(x.at * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" })}</td><td>${esc(x.who)}</td><td><span class="agentname">${avatar(x.provider, x.provider)}${esc(x.account ?? "none")}</span></td><td class="muted">${esc(x.model ?? "")}</td>
        <td>${x.status < 400 ? `<span class="ok">${x.status}</span>` : `<span class="warnmark">${x.status}</span>`}${x.tried?.length ? ` <span class="badge" title="${esc(x.tried.join("; "))}">after ${plural(x.tried.length, "other")}</span>` : ""}</td>
        <td class="muted">${n(x.input_tokens)} / ${n(x.output_tokens)}</td><td class="muted">${took(x.ms)}${x.streamed ? " · streamed" : ""}</td></tr>`)
    .join("");
  const decisions = r.decisions
    .map((d) => `<li><time>${new Date(d.at * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</time><span class="dot ${d.moved ? "swap" : ""}" aria-hidden="true">${d.moved ? "⇄" : "·"}</span><span>${
      d.chose
        ? `For <strong>${esc(d.who)}</strong> on ${esc(agentTitle(d.provider))}: ${d.moved ? `move from ${esc(d.was_on ?? "nowhere")} to <strong>${esc(d.chose)}</strong>` : `stay on <strong>${esc(d.chose)}</strong>`} <span class="muted">· chosen among ${d.among}</span>`
        : `For <strong>${esc(d.who)}</strong>: <span class="muted">${esc(d.note ?? "no answer")}</span>`
    }</span></li>`)
    .join("");
  $("sessions").innerHTML = `${pageHead("Requests", `What members' agents sent through this server. ${n(r.in_flight)} in flight now, of ${n(r.limit)} at once.${t.since ? ` The oldest request counted below is from ${new Date(t.since * 1000).toLocaleString([], { dateStyle: "medium", timeStyle: "short" })}.` : ""}`, `<button data-act="report-refresh">Refresh</button>`)}
    <div class="tiles">
      ${tile(n(t.requests), "requests")}
      ${tile(n(t.input_tokens), "tokens in")}
      ${tile(n(t.output_tokens), "tokens out")}
      ${tile(took(t.p50_ms), `median time · ${took(t.p95_ms)} for the slowest 5%`)}
      ${tile(n(t.failed), `failed · ${n(t.moved)} retried on another account`, t.failed ? "warn" : "")}
    </div>
    <div class="card pad chart"><h3>Requests a minute, last hour</h3>
      <svg viewBox="0 0 600 46" preserveAspectRatio="none" role="img" aria-label="Requests per minute over the last hour, ${n(r.per_minute.reduce((a, b) => a + b, 0))} in all, at most ${peak} in a minute">${bars}</svg></div>
    <div class="steps">${share(r.by_account, "By account", false)}${share(r.by_member, "By member", true)}</div>

    <h3 class="section">Decisions <span class="count">${r.decisions.length}</span></h3>
    ${decisions ? `<ul class="events">${decisions}</ul>` : `<p class="muted">${state.config.advisor_enabled ? "The decision model has not been asked yet. It is asked when a member's account passes half of a window and there is another to choose." : "The decision model is off, so the fixed rules place every request: stay while there is room, move at 90%, on refusal, or on failure. Turn it on from the Server page."}</p>`}

    <h3 class="section">Requests <span class="count">${n(t.requests)}</span></h3>
    <div class="toolbar">${pick("who", "Member", r.members)}${pick("provider", "Provider", r.providers)}${pick("account", "Account", r.accounts)}${pick("result", "Result", ["ok", "failed"])}
      <span class="spacer"></span><span class="muted">${r.requests.length < t.requests ? `newest ${r.requests.length} of ${n(t.requests)} · ` : ""}${n(r.kept)} kept here, all of them in <code>gateway-traces.log</code></span></div>
    ${rows ? `<div class="card pad"><table class="traces"><caption class="skip">Requests</caption><thead><tr><th scope="col">When</th><th scope="col">Who</th><th scope="col">Account</th><th scope="col">Model</th><th scope="col">Result</th><th scope="col">Tokens in / out</th><th scope="col">Time</th></tr></thead><tbody>${rows}</tbody></table></div>` : `<div class="empty"><h3>No requests match</h3><p>Nothing has come through with these filters.</p></div>`}`;
}

async function loadReport() {
  if (!SERVER || !state.server.settings?.gateway) return;
  try {
    state.report = await api("POST", "/server/requests", state.reportFilter);
  } catch (e) {
    console.warn("requests", e);
  }
}

function renderSessions() {
  if (SERVER) return renderRequests();
  const everyone = state.accounts.filter((a) => a.enabled && a.health === "ok");
  const rows = state.sessions
    .map((s) => {
      const usable = everyone.filter((a) => a.provider === s.provider);
      const isClaude = s.provider === "claude";
      // An account live in another session's profile cannot be put in this one too.
      const picker = s.store
        ? `<select data-act="swap" data-id="${esc(s.id)}" aria-label="Account for the session in ${esc(short(s.cwd))}">${usable
            .map((a) => {
              // In use by another session, or it is the login in your own
              // profile, which a separately managed session cannot share.
              const elsewhere = a.id !== s.account_id && (a.sessions.length > 0 || (a.is_default && s.store !== "default"));
              return `<option value="${esc(a.id)}"${a.id === s.account_id ? " selected" : ""}${elsewhere ? " disabled" : ""}>${esc(a.label)}${elsewhere ? " (in use)" : ""}</option>`;
            })
            .join("")}</select>`
        : !isClaude
          ? `<strong>${esc(s.account_label ?? "")}</strong> <span class="badge" title="This agent reads its account when it starts. Start it again to run on another one.">set at start</span>`
          : `<span class="muted">your own login, not managed</span>`;
      const a = s.advice;
      const verdict = !a
        ? ""
        : a.error
          ? `<div class="muted">decision model: ${esc(a.error)}</div>`
          : a.switch == null
            ? `<div class="muted">decision model: asking…</div>`
            : `<div class="muted">decision model: ${Math.round(a.switch * 100)}% for switching${a.target ? ` to ${esc(a.target)}` : ""}${a.acted ? " · switched" : ""}</div>`;
      const ask = state.config.advisor_enabled && state.config.has_key && s.store ? ` <button class="quiet" data-act="advise" data-id="${esc(s.id)}">Ask the decision model</button>` : "";
      const kind = s.native ? `<span class="badge" title="Started as plain claude. All such sessions share one login and switch together.">plain claude · shared login</span>` : "";
      const agent = isClaude ? "Claude Code" : agentTitle(s.agent);
      const usage = isClaude
        ? `${windowRow(s.usage, "5h", "5-hour window")}${windowRow(s.usage, "7d", "7-day window")}`
        : `<span class="muted">usage is not reported to llmpool</span>`;
      return `<li class="card sess">
          ${avatar(agent, isClaude ? "claude" : (s.agent ?? "").split("/").pop())}
          <div class="who"><div class="name"><strong title="${esc(s.cwd)}"><bdi>${esc(short(s.cwd))}</bdi></strong> ${kind}</div>
            <div class="sub">${esc(agent)}${s.picked_by ? ` · ${esc(s.picked_by)}` : ""}</div>${verdict}</div>
          <div class="pick">${picker}${ask}</div>
          <div class="usage">${usage}</div>
        </li>`;
    })
    .join("");
  const events = state.events
    .filter((e) => e.kind !== "agent")
    .slice(-40)
    .reverse()
    .map((e) => `<li><time>${new Date(e.at * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</time><span class="dot ${e.kind}" aria-hidden="true">${EVENT_ICONS[e.kind] ?? "·"}</span><span>${esc(tidy(e.message))}</span></li>`)
    .join("");
  $("sessions").innerHTML = `${pageHead("Sessions", "What is running now, on which account, and why.")}
    ${
      rows
        ? `<ul class="cards">${rows}</ul>
           <p class="muted foot">Choosing another account switches a running Claude Code session within about a minute. Other agents read their account when they start.</p>`
        : `<div class="empty"><h3>Nothing is running</h3><p>Start Claude Code as usual, or any other agent with <code>llmpool exec -- codex</code> (or opencode, gemini, grok, …). It lands on the account your rules pick.</p></div>`
    }
    <h3 class="section">Recent activity</h3>
    ${events ? `<ul class="events">${events}</ul>` : `<p class="muted">Nothing yet.</p>`}`;
}

// ---- policies ---------------------------------------------------------------

const TRIGGER_LABELS = {
  session_start: "a session starts",
  usage_above: "usage goes above",
  rate_limited: "the account is rate limited",
  auth_failed: "the login stops working",
  cwd_changed: "the directory changes",
  schedule: "the time is",
  pressure: "usage is climbing fast",
};
const CONDITION_LABELS = { cwd: "directory matches", git_remote: "git remote matches", model: "model matches", account_tag: "current account is tagged" };
const CONDITION_HINTS = { cwd: "~/work/acme/**", git_remote: "*github.com:acme/*", model: "*opus*", account_tag: "work" };
const ACTION_LABELS = { assign: "use", swap: "switch to", ask: "ask me, among", notify: "only tell me", hold: "do nothing" };
const PICK_LABELS = { least_busy: "used by the fewest sessions", most_quota: "with the most quota left", soonest_reset: "whose week resets soonest", round_robin: "next in turn" };
const TRIGGER_EVENT = { usage_above: "usage_updated", pressure: "usage_updated", schedule: "tick" };
const DAYS = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

const TEMPLATES = {
  "Switch before the limit": { name: "switch before the limit", enabled: true, on: { usage_above: { window: "5h", percent: 90 } }, do: { swap: { tag: "same", pick: "most_quota" }, notify: true, hold: false } },
  "Relieve a subscription under pressure": { name: "relieve pressure", enabled: true, on: { pressure: { window: "5h", full_within_minutes: 30, min_sessions: 2 } }, do: { swap: { pick: "least_busy" }, notify: true, hold: false } },
  "Spread new sessions across accounts": { name: "spread sessions", enabled: true, on: "session_start", do: { assign: { pick: "least_busy" }, notify: false, hold: false } },
  "Switch when rate limited": { name: "switch when rate limited", enabled: true, on: "rate_limited", do: { swap: { pick: "most_quota" }, notify: true, hold: false } },
  "Use tagged accounts in a directory": { name: "work repos use work accounts", enabled: true, on: "session_start", when: { cwd: "~/work/**" }, do: { assign: { tag: "work", pick: "most_quota" }, notify: false, hold: false } },
  "Ask me when a session starts": { name: "ask at start", enabled: true, on: "session_start", do: { ask: { pick: "most_quota" }, notify: false, hold: false } },
  "Move off a dead login": { name: "move off a dead login", enabled: true, on: "auth_failed", do: { swap: { pick: "most_quota" }, notify: true, hold: false } },
  "Switch at a time of day": { name: "evening account", enabled: true, on: { schedule: { at: "18:00" } }, do: { swap: { pick: "most_quota" }, notify: true, hold: false } },
};

const triggerKind = (rule) => (typeof rule.on === "string" ? rule.on : Object.keys(rule.on)[0]);
const triggerParams = (rule) => (typeof rule.on === "string" ? {} : rule.on[triggerKind(rule)]);
const actionKind = (rule) => ["assign", "swap", "ask"].find((k) => rule.do[k]) ?? (rule.do.hold ? "hold" : "notify");
const allTags = () => [...new Set(state.accounts.flatMap((a) => a.tags))].sort();

function targetValue(target) {
  if (target?.account) return `account:${target.account}`;
  if (target?.tag) return `tag:${target.tag}`;
  return "any";
}

function triggerControls(rule, i) {
  const kind = triggerKind(rule);
  const params = triggerParams(rule);
  if (kind === "usage_above")
    return `<input class="num" type="number" min="1" max="100" data-act="percent" data-i="${i}" value="${esc(params.percent ?? 90)}" aria-label="Percent used" />
      <span>% of the</span>
      <select data-act="window" data-i="${i}" aria-label="Usage window">${options(["5h", "7d"], params.window ?? "5h", { "5h": "5-hour window", "7d": "7-day window" })}</select>`;
  if (kind === "pressure")
    return `<span>: at this pace the</span>
      <select data-act="p-window" data-i="${i}" aria-label="Usage window">${options(["5h", "7d"], params.window ?? "5h", { "5h": "5-hour window", "7d": "7-day window" })}</select>
      <span>is full within</span>
      <input class="num" type="number" min="1" data-act="p-minutes" data-i="${i}" value="${esc(params.full_within_minutes ?? 30)}" aria-label="Minutes until full" />
      <span>minutes, with at least</span>
      <input class="num" type="number" min="1" data-act="p-sessions" data-i="${i}" value="${esc(params.min_sessions ?? 1)}" aria-label="Sessions sharing the account" />
      <span>session(s) on the account</span>`;
  if (kind === "schedule") {
    const days = params.days ?? [];
    return `<input type="time" data-act="at" data-i="${i}" value="${esc(params.at ?? "09:00")}" aria-label="Time of day" />
      <span class="days" role="group" aria-label="Days; none ticked means every day">${DAYS.map(
        (d) => `<label><input type="checkbox" data-act="day" data-i="${i}" data-key="${d}"${days.includes(d) ? " checked" : ""} /> ${d}</label>`,
      ).join("")}</span>`;
  }
  return "";
}

function renderRule(rule, i) {
  const kind = triggerKind(rule);
  const action = actionKind(rule);
  const target = rule.do[action];
  const when = rule.when ?? {};
  const conditions = Object.keys(CONDITION_LABELS).filter((k) => when[k] !== undefined);
  const n = `rule ${i + 1}, ${rule.name}`;

  const conditionRows = conditions
    .map(
      (key) => `<span class="line">
        <select data-act="cond-key" data-i="${i}" data-key="${key}" aria-label="Condition">${options(Object.keys(CONDITION_LABELS), key, CONDITION_LABELS)}</select>
        <input class="wide" data-act="cond-value" data-i="${i}" data-key="${key}" value="${esc(when[key])}" placeholder="${esc(CONDITION_HINTS[key])}" aria-label="${esc(CONDITION_LABELS[key])}" />
        <button class="quiet" data-act="cond-remove" data-i="${i}" data-key="${key}" aria-label="Remove condition ${esc(CONDITION_LABELS[key])}">×</button></span>`,
    )
    .join("");
  const timeNote = when.time ? `<span class="badge" title="Edit in the TOML view">time window (edit in TOML)</span>` : "";

  const targets = [
    ["any", "any account"],
    ...(action === "swap" ? [["tag:same", "an account with the same tags"]] : []),
    ...allTags().map((t) => [`tag:${t}`, `an account tagged ${t}`]),
    ...state.accounts.map((a) => [`account:${a.label}`, a.label]),
  ];
  const current = targetValue(target);
  if (!targets.some(([v]) => v === current)) targets.push([current, current.replace(/^(tag|account):/, "") + " (no such account or tag)"]);
  const targetControls = target
    ? `<select data-act="target" data-i="${i}" aria-label="Which account">${targets
        .map(([v, label]) => `<option value="${esc(v)}"${v === current ? " selected" : ""}>${esc(label)}</option>`)
        .join("")}</select>
       ${target.account || action === "ask" ? "" : `<select data-act="pick" data-i="${i}" aria-label="How to choose among several">${options(state.schema.picks, target.pick ?? "most_quota", PICK_LABELS)}</select>`}`
    : "";
  const actions = kind === "session_start" ? ["assign", "ask", "notify", "hold"] : ["swap", "ask", "notify", "hold"];
  const notify =
    action === "swap" || action === "ask" || action === "assign"
      ? `<label><input type="checkbox" data-act="notify" data-i="${i}"${rule.do.notify ? " checked" : ""} /> and tell me</label>`
      : "";
  const result = state.results[i];

  return `<article class="rule${rule.enabled === false ? " off" : ""}" aria-label="${esc(n)}">
    <div class="head">
      <span class="order" aria-hidden="true">${i + 1}</span>
      <input type="checkbox" data-act="enabled" data-i="${i}"${rule.enabled === false ? "" : " checked"} aria-label="Enable ${esc(n)}" />
      <input class="name" data-act="name" data-i="${i}" value="${esc(rule.name)}" aria-label="Name of rule ${i + 1}" />
      <button class="quiet" data-act="up" data-i="${i}"${i === 0 ? " disabled" : ""} aria-label="Move ${esc(n)} up">↑</button>
      <button class="quiet" data-act="down" data-i="${i}"${i === state.policy.rule.length - 1 ? " disabled" : ""} aria-label="Move ${esc(n)} down">↓</button>
      <button data-act="test" data-i="${i}" aria-label="Test ${esc(n)}">Test</button>
      <button class="quiet danger" data-act="delete" data-i="${i}" aria-label="Delete ${esc(n)}">Delete</button>
    </div>
    <div class="line"><span class="word">When</span>
      <select data-act="trigger" data-i="${i}" aria-label="When">${options(state.schema.triggers.map((t) => t.kind), kind, TRIGGER_LABELS)}</select>${triggerControls(rule, i)}</div>
    <div class="line"><span class="word">If</span>${conditionRows || `<span class="muted">always</span>`}${timeNote}
      <button class="quiet" data-act="cond-add" data-i="${i}"${conditions.length >= 4 ? " disabled" : ""}>+ condition</button></div>
    <div class="line"><span class="word">Then</span>
      <select data-act="action" data-i="${i}" aria-label="Then">${options(actions, action, ACTION_LABELS)}</select>${targetControls}${notify}</div>
    ${result ? `<div class="result" role="status"><span aria-hidden="true">${result.fired ? "✓" : "–"}</span> ${esc(result.text)}</div>` : ""}
  </article>`;
}

function renderPolicies() {
  const settings = state.policy?.settings ?? {};
  const toolbar = `${pageHead("Rules", "When a session starts, fills up or is refused, the first rule that matches decides.")}<div class="toolbar">
      <div role="group" aria-label="View">
        <button data-act="view" data-view="rules" aria-pressed="${state.policyView === "rules"}">Rules</button>
        <button data-act="view" data-view="toml" aria-pressed="${state.policyView === "toml"}">TOML</button>
      </div>
      <span class="spacer"></span>
      ${state.dirty ? `<span class="badge warn">unsaved changes</span><button data-act="revert">Discard</button>` : ""}
      <button class="primary" data-act="save"${state.dirty ? "" : " disabled"}>Save</button>
    </div>
    ${state.policyError ? `<div class="callout bad" role="alert"><strong>⚠ Not saved.</strong><p>${esc(state.policyError)}</p></div>` : ""}
    ${state.policyView === "rules" && state.dirty && state.hasComments ? `<div class="callout" role="note"><strong>The file has comments.</strong><p>Saving from this view rewrites the file and drops them. Switch to TOML to keep them.</p></div>` : ""}`;
  let body;
  if (state.policyView === "toml") {
    body = `<textarea class="toml" data-act="toml" spellcheck="false" aria-label="Policy as TOML">${esc(state.policyText)}</textarea>`;
  } else if (state.policy === null) {
    body = `<div class="callout bad" role="alert"><strong>⚠ The policy file does not parse.</strong><p>Fix it in the TOML view.</p></div>`;
  } else {
    const teamRules = state.remotePolicy.team?.rule ?? [];
    const inherited = teamRules.length
      ? `<div class="callout" role="note"><strong>From your team · tried first · read-only</strong>
          <ol class="teamrules">${teamRules.map((r) => `<li${r.enabled === false ? ' class="muted"' : ""}><strong>${esc(r.name)}</strong> <span class="muted">on ${esc(typeof r.on === "string" ? r.on : Object.keys(r.on ?? {})[0] ?? "")}</span></li>`).join("")}</ol></div>`
      : "";
    body = `${inherited}${state.policy.rule.map(renderRule).join("") || `<p class="empty">No rules yet. Without rules, every session runs on your own login and nothing switches.</p>`}
      <div class="toolbar">
        <label for="template" class="muted">Add a rule:</label>
        <select id="template" data-act="add-rule"><option value="">choose a starting point…</option>${options(Object.keys(TEMPLATES), "")}</select>
      </div>
      <div class="settings">
        <label>Wait <input class="num" type="number" min="0" data-act="cooldown" value="${esc(Math.round((settings.cooldown_secs ?? 300) / 60))}" /> minutes between switches of one session</label>
        <label>Only switch for usage if the other account is <input class="num" type="number" min="0" max="100" data-act="margin" value="${esc(settings.margin_points ?? 10)}" /> points emptier</label>
      </div>`;
  }
  $("policies").innerHTML = toolbar + body;
}

function describe(result, rule) {
  const { decision, labels } = result;
  const name = (id) => labels[id] ?? id;
  const o = decision.outcome;
  const what =
    typeof o === "string"
      ? { hold: "do nothing", notify: "tell you", exhausted: "look for another account and find none that qualifies", cooldown: "wait, because this session switched moments ago" }[o]
      : o.assign
        ? `start the session as ${name(o.assign)}`
        : o.swap
          ? `switch to ${name(o.swap)}`
          : `ask you to choose among ${o.ask.map(name).join(", ")}`;
  if (!decision.rule) return { fired: false, text: "Right now this rule would not fire, and neither would any other." };
  if (decision.rule !== rule.name) return { fired: false, text: `Right now rule “${decision.rule}” comes first and would ${what}.` };
  return { fired: true, text: `Right now this rule would fire and ${what}.` };
}

async function renderedText() {
  if (state.policyView === "toml") return state.policyText;
  return (await api("POST", "/policies/render", { policy: state.policy })).text;
}

function edit(change) {
  change();
  state.dirty = true;
  state.policyError = null;
  state.results = {};
  renderPolicies();
}

// A change that should not redraw, because the user is still typing in the field.
function typed(change) {
  change();
  if (!state.dirty) {
    state.dirty = true;
    // The redraw (for the "unsaved" badge) replaces the field being typed in;
    // put the caret and the scroll position back exactly where they were.
    const active = document.activeElement;
    const { act, i, key } = active?.dataset ?? {};
    const textual = active && active.type !== "number" && active.type !== "time" && active.selectionStart !== undefined;
    const place = textual ? [active.selectionStart, active.selectionEnd, active.scrollTop] : null;
    renderPolicies();
    const again = [...document.querySelectorAll(`[data-act="${act}"]`)].find((el) => el.dataset.i === i && el.dataset.key === key);
    again?.focus();
    if (again && place) {
      again.setSelectionRange(place[0], place[1]);
      again.scrollTop = place[2];
    }
  }
}

function setTrigger(rule, kind) {
  rule.on =
    kind === "usage_above"
      ? { usage_above: { window: "5h", percent: 90 } }
      : kind === "schedule"
        ? { schedule: { at: "09:00" } }
        : kind === "pressure"
          ? { pressure: { window: "5h", full_within_minutes: 30, min_sessions: 2 } }
          : kind;
  // `assign` only makes sense when a session starts, `swap` only after.
  const action = actionKind(rule);
  if (kind === "session_start" && action === "swap") setAction(rule, "assign");
  if (kind !== "session_start" && action === "assign") setAction(rule, "swap");
}

function setAction(rule, action) {
  const previous = rule.do[actionKind(rule)];
  const target = typeof previous === "object" ? previous : { pick: "most_quota" };
  if (action !== "swap" && target.tag === "same") delete target.tag;
  rule.do = { notify: action === "notify" || (rule.do.notify && action !== "hold"), hold: action === "hold" };
  if (["assign", "swap", "ask"].includes(action)) rule.do[action] = target;
}

async function switchView(view) {
  // Carry unsaved edits across the two views.
  if (state.dirty) {
    try {
      if (view === "toml") state.policyText = await renderedText();
      else state.policy = (await api("POST", "/policies/parse", { text: state.policyText })).policy;
      state.policyError = null;
    } catch (e) {
      state.policyError = String(e);
      return renderPolicies();
    }
  }
  state.policyView = view;
  renderPolicies();
}

async function savePolicy() {
  try {
    await api("PUT", "/policies", { text: await renderedText() });
    await loadPolicy();
    state.policyError = null;
    toast("Policies saved");
  } catch (e) {
    state.policyError = String(e);
  }
  renderPolicies();
}

async function testRule(rule, i) {
  try {
    const kind = triggerKind(rule);
    const session = state.sessions.find((s) => s.store) ?? state.sessions[0];
    const request = { text: await renderedText(), event: TRIGGER_EVENT[kind] ?? kind };
    if (session) request.session_id = session.id;
    const result = describe(await api("POST", "/policies/dry-run", request), rule);
    result.text += session ? ` Tested against the session in ${short(session.cwd)}.` : " Tested against a new session in your home directory.";
    state.results = { [i]: result };
  } catch (e) {
    state.results = { [i]: { fired: false, text: String(e) } };
  }
  renderPolicies();
}

async function policyAction(el) {
  const act = el.dataset.act;
  const i = Number(el.dataset.i);
  const rule = state.policy?.rule?.[i];
  const key = el.dataset.key;
  switch (act) {
    case "view":
      return switchView(el.dataset.view);
    case "save":
      return savePolicy();
    case "revert":
      await loadPolicy();
      state.policyError = null;
      return renderPolicies();
    case "toml":
      return typed(() => (state.policyText = el.value));
    case "add-rule":
      if (!TEMPLATES[el.value]) return;
      return edit(() => state.policy.rule.push(structuredClone(TEMPLATES[el.value])));
    case "delete":
      return edit(() => state.policy.rule.splice(i, 1));
    case "up":
    case "down": {
      const j = act === "up" ? i - 1 : i + 1;
      edit(() => ([state.policy.rule[i], state.policy.rule[j]] = [state.policy.rule[j], state.policy.rule[i]]));
      // Keep the keyboard on the rule that moved.
      return void [...document.querySelectorAll(`[data-act="${act}"]`)].find((b) => Number(b.dataset.i) === j && !b.disabled)?.focus();
    }
    case "enabled":
      return edit(() => (rule.enabled = el.checked));
    case "name":
      return typed(() => (rule.name = el.value));
    case "trigger":
      return edit(() => setTrigger(rule, el.value));
    case "percent":
      return typed(() => (rule.on.usage_above.percent = Number(el.value)));
    case "window":
      return edit(() => (rule.on.usage_above.window = el.value));
    case "p-window":
      return edit(() => (rule.on.pressure.window = el.value));
    case "p-minutes":
      return typed(() => (rule.on.pressure.full_within_minutes = Number(el.value)));
    case "p-sessions":
      return typed(() => (rule.on.pressure.min_sessions = Number(el.value)));
    case "at":
      return typed(() => (rule.on.schedule.at = el.value));
    case "day":
      return edit(() => {
        const days = new Set(rule.on.schedule.days ?? []);
        el.checked ? days.add(key) : days.delete(key);
        if (days.size) rule.on.schedule.days = DAYS.filter((d) => days.has(d));
        else delete rule.on.schedule.days;
      });
    case "cond-add": {
      const free = Object.keys(CONDITION_LABELS).find((k) => rule.when?.[k] === undefined);
      return edit(() => (rule.when = { ...(rule.when ?? {}), [free]: free === "cwd" ? "~/**" : "*" }));
    }
    case "cond-remove":
      return edit(() => delete rule.when[key]);
    case "cond-key":
      return edit(() => {
        if (rule.when[el.value] === undefined) {
          rule.when[el.value] = rule.when[key];
          delete rule.when[key];
        }
      });
    case "cond-value":
      return typed(() => (rule.when[key] = el.value));
    case "action":
      return edit(() => setAction(rule, el.value));
    case "target":
      return edit(() => {
        const target = rule.do[actionKind(rule)];
        delete target.account;
        delete target.tag;
        const [type, ...rest] = el.value.split(":");
        if (type !== "any") target[type] = rest.join(":");
      });
    case "pick":
      return edit(() => (rule.do[actionKind(rule)].pick = el.value));
    case "notify":
      return edit(() => (rule.do.notify = el.checked));
    case "cooldown":
      return typed(() => (state.policy.settings = { ...state.policy.settings, cooldown_secs: Math.max(0, Number(el.value)) * 60 }));
    case "margin":
      return typed(() => (state.policy.settings = { ...state.policy.settings, margin_points: Number(el.value) }));
    case "test":
      return testRule(rule, i);
  }
}

async function loadPolicy() {
  const loaded = await api("GET", "/policies");
  state.policy = loaded.policy;
  state.policyText = loaded.text;
  // Comments live only in the text; the rules view cannot carry them.
  state.hasComments = loaded.text.split("\n").some((line) => line.trim().startsWith("#"));
  state.dirty = false;
  state.results = {};
}

// ---- settings ---------------------------------------------------------------

// Every agent llmpool can start, what it runs on and how to start it.
function agentsPanel(claude, hooked) {
  const row = (title, runsOn, status, how, key) => `<tr><td><span class="agentname">${avatar(title, key)}<strong>${esc(title)}</strong></span></td><td class="muted">${runsOn}</td><td>${status}</td><td class="actions">${how}</td></tr>`;
  const command = (text) => `<a href="#" data-act="copy" data-text="${esc(text)}" title="Copy"><code>${esc(text)}</code></a>`;
  const rows = [
    row(
      "Claude Code",
      "its own logins",
      claude ? (hooked ? `<span class="ok" aria-hidden="true">✓</span> connected: live switching, usage, <code>/llmpool</code>` : `<span class="warnmark" aria-hidden="true">⚠</span> installed, not connected yet`) : `<span class="muted">not found on this Mac</span>`,
      command("claude"),
      "claude",
    ),
    ...state.agents.harnesses.map((h) => {
      const runsOn = [h.has_login ? "its own logins" : "", h.provider ? `${esc(agentTitle(h.provider))} keys` : h.has_login ? "any key" : "any key, by provider"].filter(Boolean).join(", ");
      const status = !h.installed
        ? `<span class="muted">not found on this Mac</span>`
        : h.accounts > 0
          ? `<span class="ok" aria-hidden="true">✓</span> ${plural(h.accounts, "account")} in the pool`
          : `<span class="warnmark" aria-hidden="true">⚠</span> no account in the pool yet <button class="quiet" data-act="goto-add" data-kind="${h.logged_in && !h.login_in_pool ? "agent" : "key"}">Add one</button>`;
      return row(h.title, runsOn, status, h.installed ? command(`llmpool exec -- ${h.command}`) : "", h.name);
    }),
  ].join("");
  return `<table><caption class="skip">Coding agents</caption>
      <thead><tr><th scope="col">Agent</th><th scope="col">Runs on</th><th scope="col">On this Mac</th><th scope="col">Start it with</th></tr></thead><tbody>${rows}</tbody></table>
    ${state.agents.error ? `<p class="warnmark" role="alert">⚠ ${esc(state.agents.error)}</p>` : ""}
    <p class="muted">To type <code>codex</code> or <code>opencode</code> as usual and still go through the pool: ${command("llmpool install agents")}. Any other agent or script: ${command("llmpool exec --provider openai -- your-command")}, or describe it once in <code>harnesses.toml</code>.</p>`;
}

// Joining a team's shared pool: by signing in at its server, or with a key.
function teamPanel() {
  const t = state.team;
  if (t.state === "connected") {
    const ago = t.synced_at ? `synced ${Math.max(0, Math.round((Date.now() / 1000 - t.synced_at) / 60))} min ago` : "not synced yet";
    return `<p><span class="ok" aria-hidden="true">✓</span> Connected to <code>${esc(t.url)}</code> as <strong>${esc(t.email)}</strong> · ${plural(t.accounts, "shared account")} · ${ago}</p>
      ${t.message ? `<p class="warnmark" role="alert">⚠ ${esc(t.message)}</p>` : ""}
      <p class="muted">The team's accounts and rules arrive read-only. Its rules are tried before yours; your own account still comes first if Settings says so.</p>
      <div class="copy"><button data-act="team-sync">Sync now</button>
        ${t.message ? `<button class="primary" data-act="team-again">Connect again</button>` : ""}
        <button class="quiet danger" data-act="team-leave" data-confirm="Really leave?">Leave the team's pool</button></div>`;
  }
  if (t.state === "waiting") {
    return `<p role="status"><strong>◌ Waiting for you to finish signing in in your browser…</strong></p>
      <div class="copy"><a href="#" data-act="copy" data-text="${esc(t.auth_url)}">copy the sign-in link</a>
        <button class="quiet" data-act="team-leave">Cancel</button></div>`;
  }
  return `<p class="muted">A team can run an llmpool server that lends its accounts and rules to its members. Enter its address and sign in with your organisation's account, or use an API key if your administrator gave you one.</p>
    ${t.state === "failed" && t.message ? `<p class="warnmark" role="alert">⚠ ${esc(t.message)}</p>` : ""}
    <div class="copy">
      <input class="wide" id="team-url" type="url" placeholder="https://pool.example.com" aria-label="Team server address" value="${esc(state.teamUrl ?? "")}" />
      <button class="primary" data-act="team-signin">Sign in</button>
    </div>
    <div class="copy">
      <input class="wide" id="team-key" type="password" autocomplete="off" placeholder="API key (instead of signing in)" aria-label="Team API key, instead of signing in" />
      <button data-act="team-key">Connect with key</button>
    </div>`;
}

function renderAgents() {
  const hooked = !state.issues.some((i) => i.kind === "hooks_missing");
  const claude = !state.issues.some((i) => i.kind === "claude_missing");
  $("agents").innerHTML = `${pageHead("Agents", "The coding agents llmpool can start, and what each of them runs on.")}
    <div class="card pad">${agentsPanel(claude, hooked)}</div>`;
}

// A link to send: it opens a page that says who invited you to what, with
// the one line that installs llmpool, connects it and makes `claude` use the pool.
function invitePanel(v) {
  const from = state.inviteFrom ?? v.you.split("@")[0].replace(/[._-]+/g, " ").replace(/\b\w/g, (c) => c.toUpperCase());
  const link = `${v.invite}${from.trim() ? `?from=${encodeURIComponent(from.trim())}` : ""}`;
  const subject = from.trim() ? `${from.trim()} invited you to the ${v.name} LLM pool` : `You are invited to the ${v.name} LLM pool`;
  const body = `${subject}.\n\nOne line sets it up, and your coding agent then runs on the shared pool:\n\n${link}\n`;
  return `<p class="muted">Send people this link. It opens a page that says who invited them, with one line to copy: it installs llmpool, connects it to this pool, and makes <code>claude</code> run on it. ${v.sign_in ? `Only people who may sign in (${esc([...v.allowed_domains.map((d) => "*@" + d), ...v.allowed_emails].join(", "))}) get in.` : "They also need an API key from you."}</p>
    <div class="copy">
      <label class="filter">The pool is called <input class="wide" id="invite-name" value="${esc(v.settings.name ?? "")}" placeholder="${esc(v.name)}" aria-label="Name of the pool on the invitation" /></label>
      <button data-act="invite-name">Save</button>
      <label class="filter">from <input class="tags" id="invite-from" value="${esc(from)}" aria-label="Your name on the invitation" /></label>
    </div>
    <div class="copy">
      <code class="invite">${esc(link)}</code>
      <button class="primary" data-act="copy" data-text="${esc(link)}">Copy link</button>
      <a class="buttonlike" href="mailto:?subject=${encodeURIComponent(subject)}&body=${encodeURIComponent(body)}">Email it</a>
      <a class="buttonlike" href="${esc(link)}" target="_blank" rel="noopener">See the page</a>
    </div>`;
}

// What only a server has: who may connect, what each may use, who is on what.
function renderServer() {
  const v = state.server;
  if (!v.settings) return ($("team").innerHTML = pageHead("Server", "Loading…"));
  const codes = (items) => (items?.length ? items.map((i) => `<code>${esc(i)}</code>`).join(" ") : `<span class="muted">none</span>`);
  const when = (at) => new Date(at * 1000).toLocaleString([], { dateStyle: "medium", timeStyle: "short" });
  const members = v.members
    .map((m) => `<tr><td><strong>${esc(m.email)}</strong>${m.email === v.you ? ` <span class="badge">you</span>` : ""}</td><td class="muted">${m.by_key ? "an API key" : "their account"}</td><td class="muted">${esc(when(m.expires_at))}</td>
        <td class="actions"><button class="quiet danger" data-act="srv-revoke" data-email="${esc(m.email)}" data-confirm="Really sign them out?">Sign out everywhere</button></td></tr>`)
    .join("");
  const seats = v.seats.map((x) => `<tr><td><strong>${esc(x.account)}</strong></td><td>${esc(x.who)}</td><td class="muted">${plural(x.sessions, "session")}</td></tr>`).join("");
  const audit = v.audit
    .map((e) => `<li><time>${new Date(e.at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</time><span class="dot ${e.event.includes("denied") ? "login" : ""}" aria-hidden="true">${e.event.includes("denied") ? "⚠" : "·"}</span><span><strong>${esc(e.event.replaceAll("_", " "))}</strong> ${esc(e.who)}${e.detail ? ` <span class="muted">${esc(e.detail)}</span>` : ""}</span></li>`)
    .join("");
  $("team").innerHTML = `${pageHead("Server", `Serving this pool at <code>${esc(v.public_url)}</code>. Members connect with <code>llmpool team connect ${esc(v.public_url)}</code>.`)}
    <h3 class="section">Invite your team</h3>
    <div class="card pad">${invitePanel(v)}</div>

    <h3 class="section">Gateway</h3>
    <div class="card pad">
      <label class="switch"><input type="checkbox" data-act="srv-gateway" data-confirm="This changes how every member's agents connect. Press again to go ahead."${v.settings.gateway ? " checked" : ""} />
        <span><strong>Send members' requests through this server</strong><br /><span class="muted">On, no login or key leaves the server: members' agents talk to <code>${esc(v.gateway.base)}/&lt;provider&gt;</code> with their own session, and each request runs on an account chosen here. When one fills up or is refused, the next request runs on another, with nothing to restart. Off, accounts are lent to members' machines instead.</span></span></label>
      ${v.settings.gateway ? `<div class="copy indent"><button data-act="goto-requests">Open requests</button><span class="muted">${v.gateway.recent.length ? `${v.gateway.recent.length >= 200 ? "200+" : v.gateway.recent.length} recorded` : "none yet; members get the gateway the next time their llmpool syncs"} · every request is one line in <code>gateway-traces.log</code>: who, account, model, tokens, time. Never what was said.</span></div>` : ""}
    </div>

    <h3 class="section">Decision model <span class="badge">optional</span></h3>
    <div class="card pad">
      <label class="switch"><input type="checkbox" data-act="advisor"${state.config.advisor_enabled ? " checked" : ""} />
        <span><strong>Let a decision model place heavy use</strong><br /><span class="muted">When a member's account passes half of a window and there is another to choose, the server shows the model the whole pool, with how many members are on each account, and asks where that member's requests should go next. Accounts are named by letter; nothing that was said is sent. It is asked after a request has been answered, never on its time, and its choice holds for ten minutes. Off, the fixed rules place every request.</span></span></label>
      <p class="indent">${state.config.has_key ? `<span class="ok" aria-hidden="true">✓</span> A key is stored on the server. <button class="quiet danger" data-act="key-clear" data-confirm="Really forget it?">Forget it</button>` : `<span class="warnmark" aria-hidden="true">⚠</span> No key yet.`}
        <span class="muted"> · ${esc(state.config.calls_today ?? 0)} of ${esc(state.config.max_calls_per_day ?? 200)} questions used today</span></p>
      <div class="copy indent">
        <input class="wide" id="advisor-key" type="password" autocomplete="off" placeholder="${state.config.has_key ? "replace the key" : "API key for Jev"}" aria-label="API key for Jev, the decision model" />
        <button data-act="key-save">Save key</button>
        <button class="quiet" data-act="goto-requests">See its decisions</button>
      </div>
    </div>

    <h3 class="section">Lending</h3>
    <div class="card pad">
      <label class="switch"><input type="checkbox" data-act="srv-share"${v.settings.share_logins ? " checked" : ""} />
        <span><strong>Let several members run on one subscription login at once</strong><br /><span class="muted">Off, a login in use by one member is not lent to another until they stop. Providers license such logins per person; turn this on only where yours allows it. Keys are always shared.</span></span></label>
      <p><strong>Who may use what</strong><br /><span class="muted">One line per member: an address, a key's name, <code>*@domain</code> or <code>*</code>, then the tags it opens (<code>*</code> for all). Empty: every member may use every account.</span></p>
      <textarea id="srv-access" rows="4" spellcheck="false" aria-label="Access rules, one per line" placeholder="*@example.com=work&#10;lead@example.com=*">${esc(v.settings.access)}</textarea>
      <div class="copy"><button data-act="srv-access">Save access</button></div>
      <p><strong>Turned away</strong><br /><span class="muted">Members who may not connect even though sign-in or a key would admit them.</span></p>
      <div class="copy"><input class="wide" id="srv-blocked" value="${esc(v.settings.blocked.join(", "))}" placeholder="addresses or key names, comma separated" aria-label="Blocked members, comma separated" />
        <button data-act="srv-blocked">Save</button></div>
    </div>

    <h3 class="section">Members <span class="count">${v.members.length}</span></h3>
    <div class="card pad">${members ? `<table><caption class="skip">Members with a session</caption><thead><tr><th>Member</th><th>Signed in with</th><th>Session ends</th><th></th></tr></thead><tbody>${members}</tbody></table>` : `<span class="muted">Nobody is signed in. Members who connect with an API key do not appear here; their use shows below.</span>`}</div>

    <h3 class="section">In use now</h3>
    <div class="card pad">${seats ? `<table><caption class="skip">Accounts members are running on</caption><tbody>${seats}</tbody></table>` : `<span class="muted">No member is running on a lent account right now.</span>`}</div>

    <h3 class="section">Who can connect <span class="badge">set in the deployment</span></h3>
    <div class="card pad"><table><tbody>
      <tr><td class="muted">Sign-in</td><td>${v.sign_in ? `<code>${esc(v.sign_in)}</code>` : `<span class="muted">off; API keys only</span>`}</td></tr>
      ${v.sign_in ? `<tr><td class="muted">May sign in</td><td>${codes([...v.allowed_domains.map((d) => "*@" + d), ...v.allowed_emails])}</td></tr>` : ""}
      <tr><td class="muted">API keys</td><td>${codes(v.key_members)}</td></tr>
      <tr><td class="muted">Administrators</td><td>${codes(v.admins)}</td></tr>
      <tr><td class="muted">Stored logins</td><td>${v.sealed ? `<span class="ok" aria-hidden="true">✓</span> encrypted with a key held outside the volume` : `<span class="warnmark" aria-hidden="true">⚠</span> not encrypted: set <code>LLMPOOL_VAULT_KEY</code>`}</td></tr>
    </tbody></table></div>

    <h3 class="section">Audit log</h3>
    ${audit ? `<ul class="events">${audit}</ul>` : `<p class="muted">Nothing yet.</p>`}`;
}

function renderTeam() {
  if (SERVER) return renderServer();
  const fact = (title, text) => `<div><strong>${title}</strong><p class="muted">${text}</p></div>`;
  $("team").innerHTML = `${pageHead("Team", "Use a pool your team shares, through a server it runs.")}
    <div class="card pad">${teamPanel()}</div>
    <div class="facts">
      ${fact("Lent, not copied", "You receive short-lived access to the team's accounts. What can renew them never leaves the server.")}
      ${fact("Yours stay yours", "Your own accounts and rules keep working. A login of your own always wins over a lent one.")}
      ${fact("Aware of each other", "Rules see how many teammates are on each account, so sessions spread out instead of piling up.")}
    </div>`;
}

function renderSettings() {
  const c = state.config;
  const hooked = !state.issues.some((i) => i.kind === "hooks_missing");
  const claude = !state.issues.some((i) => i.kind === "claude_missing");
  const check = (ok, yes, no) => `<li>${ok ? `<span class="ok" aria-hidden="true">✓</span> ${yes}` : `<span class="warnmark" aria-hidden="true">⚠</span> ${no}`}</li>`;
  $("settings").innerHTML = `${pageHead("Settings", "How accounts are chosen, and how llmpool sits on this Mac.")}
    <h3 class="section">Choosing accounts</h3>
    <div class="card pad">
      <label class="switch"><input type="checkbox" data-act="prefer-own"${c.prefer_own ? " checked" : ""} />
        <span><strong>Prefer my own account</strong><br /><span class="muted">An account tagged <code>own</code> is used whenever it has room; the rest of the pool is for when it does not. The login your agent had when you set llmpool up is tagged <code>own</code> for you.</span></span></label>
      <p class="indent"><label>Counts as having room below <input class="num" type="number" min="1" max="100" data-act="own-until" value="${esc(c.own_until ?? 85)}" /> % used</label></p>
    </div>

    <h3 class="section">Decision model <span class="badge">optional</span></h3>
    <div class="card pad">
      <label class="switch"><input type="checkbox" data-act="advisor"${c.advisor_enabled ? " checked" : ""} />
        <span><strong>Let a decision model advise on switching</strong><br /><span class="muted">When an account passes half of a window, llmpool sends the decision model (Jev) a small picture of the pool, with accounts named by letter only, and asks whether to switch and where to. It acts only when the model is sure enough, and never outside your rules' limits. Off, your rules decide alone.</span></span></label>
      <p class="indent"><label>Act when it is at least <input class="num" type="number" min="50" max="100" data-act="switch-above" value="${esc(Math.round((c.switch_above ?? 0.7) * 100))}" /> % sure</label>
        <span class="muted"> · ${esc(c.calls_today ?? 0)} of ${esc(c.max_calls_per_day ?? 200)} questions used today</span></p>
      <p class="indent">${c.has_key ? `<span class="ok">✓</span> A key is stored in your keychain. <button class="quiet danger" data-act="key-clear" data-confirm="Really forget it?">Forget it</button>` : `<span class="warnmark">⚠</span> No key yet.`}</p>
      <div class="copy indent">
        <input class="wide" id="advisor-key" type="password" autocomplete="off" placeholder="${c.has_key ? "replace the key" : "API key for Jev"}" aria-label="API key for Jev, the decision model" />
        <button data-act="key-save">Save key</button>
      </div>
    </div>

    <h3 class="section">This Mac</h3>
    <div class="card pad">
      <ul class="checks">
        ${check(claude, "Claude Code is installed", "Claude Code was not found")}
        ${check(hooked, "Claude Code reports to llmpool (hooks, status line, <code>/llmpool</code>)", "Claude Code is not connected to llmpool yet")}
        ${check(state.accounts.length > 0, `${plural(state.accounts.length, "account")} in the pool`, "No accounts in the pool yet")}
      </ul>
      <label class="switch"><input type="checkbox" data-act="login-item"${state.loginItem ? " checked" : ""}${state.loginItem === null ? " disabled" : ""} />
        <span><strong>Start llmpool when I log in</strong><br /><span class="muted">It sits in the menu bar; closing this window does not quit it.</span></span></label>
      <div class="copy"><button class="${hooked ? "" : "primary"}" data-act="setup">${hooked ? "Run setup again" : "Set up llmpool on this Mac"}</button>
        <span class="muted">Installs the command line tool, connects Claude Code, adds your current login.</span></div>
      ${state.setupLog ? `<pre class="log" role="status">${esc(state.setupLog)}</pre>` : ""}
    </div>
    <p class="muted foot"><kbd>⌘1</kbd>–<kbd>⌘6</kbd> sections · <kbd>⌘S</kbd> save rules · <kbd>⌘R</kbd> refresh</p>`;
}

async function settingsAction(el) {
  const act = el.dataset.act;
  const put = (change, ok) => attempt(() => api("PUT", "/config", change), ok);
  if (act === "prefer-own") await put({ prefer_own: el.checked }, el.checked ? "Your own account comes first" : "All accounts are treated alike");
  if (act === "own-until") await put({ own_until: Number(el.value) }, "Saved");
  if (act === "advisor") await put({ advisor_enabled: el.checked }, el.checked ? "The decision model is on" : "The decision model is off; your rules decide alone");
  if (act === "switch-above") await put({ switch_above: Number(el.value) / 100 }, "Saved");
  if (act === "key-save") {
    const key = $("advisor-key").value.trim();
    if (!key) return toast("Paste the key first", "error");
    await attempt(() => api("PUT", "/advisor/key", { key }), "Key stored in your keychain");
  }
  if (act === "key-clear") await attempt(() => api("DELETE", "/advisor/key"), "Key forgotten");
  if (act === "copy") return copy(el.dataset.text);
  if (act === "goto-add") {
    state.addKind = el.dataset.kind;
    state.adding = true;
    go("pool");
    return renderPool();
  }
  if (act === "team-signin" || act === "team-key" || act === "team-again") {
    const url = act === "team-again" ? state.team.url : $("team-url").value.trim();
    const key = act === "team-key" ? $("team-key").value.trim() : null;
    if (!url) return toast("Enter the server's address first", "error");
    if (act === "team-key" && !key) return toast("Paste the key first", "error");
    state.teamUrl = url;
    await attempt(() => api("POST", "/team", { url, key }), key ? "Connected to the team's pool" : "Finish signing in in your browser");
  }
  if (act === "goto-requests" || act === "goto-server") return go(act === "goto-requests" ? "sessions" : "team");
  if (act === "report-refresh") {
    await loadReport();
    return renderRequests();
  }
  if (act === "invite-name") await attempt(() => api("PUT", "/server", { name: $("invite-name").value }), "Saved");
  if (act === "srv-gateway") await attempt(() => api("PUT", "/server", { gateway: el.checked }), el.checked ? "The server is a gateway; members switch over at their next sync" : "Accounts are lent to members again");
  if (act === "srv-share") await attempt(() => api("PUT", "/server", { share_logins: el.checked }), el.checked ? "Logins are shared between members" : "One member per login at a time");
  if (act === "srv-access") await attempt(() => api("PUT", "/server", { access: $("srv-access").value }), "Access saved");
  if (act === "srv-blocked") await attempt(() => api("PUT", "/server", { blocked: $("srv-blocked").value.split(",") }), "Saved");
  if (act === "srv-revoke") await attempt(() => api("DELETE", `/server/members/${encodeURIComponent(el.dataset.email)}`), `${el.dataset.email} is signed out`);
  if (act === "team-sync") await attempt(() => api("POST", "/team/sync"), "Synced");
  if (act === "team-leave") await attempt(() => api("DELETE", "/team"), state.team.state === "waiting" ? "Sign-in cancelled" : "Left the team's pool");
  if (act === "login-item") {
    await attempt(() => invoke("login_item", { state: el.checked ? "on" : "off" }), el.checked ? "llmpool starts at login" : "llmpool no longer starts at login");
    state.loginItem = null;
  }
  if (act === "setup") {
    el.disabled = true;
    el.textContent = "Setting up…";
    try {
      state.setupLog = await invoke("setup");
      toast("Setup finished");
    } catch (e) {
      state.setupLog = String(e);
      toast("Setup could not finish", "error");
    }
  }
  await refresh(true);
}

// ---- wiring -----------------------------------------------------------------

async function poolAction(el) {
  const id = el.dataset.id;
  const account = state.accounts.find((a) => a.id === id);
  const act = el.dataset.act;
  if (act === "copy") return copy(el.dataset.text);
  if (act === "goto-setup") return go("settings");
  if (act === "add-open" || act === "add-close") {
    state.adding = act === "add-open";
    state.addBaseline = state.accounts.map((a) => a.id);
    // Closed by hand: a drawing that has not changed is not made again.
    if (!state.adding) {
      // What was typed, a key above all, does not wait in a closed dialog.
      for (const field of $("add")?.querySelectorAll("input, textarea") ?? []) field.value = "";
      $("add")?.close();
    }
    return renderPool();
  }
  if (act === "add-kind") {
    state.addKind = el.dataset.kind;
    return renderPool();
  }
  if (act === "key-provider") {
    state.keyProvider = el.value;
    return renderPool();
  }
  if (act === "import-file") {
    const file = $("import-file").files[0];
    if (!file) return toast("Choose the exported file first", "error");
    const bundle = await file.text();
    const ok = await attempt(async () => {
      const r = await api("POST", "/import", { bundle, passphrase: $("import-pass").value, replace: false });
      toast(`Added ${r.added.length}${r.skipped.length ? `, left ${r.skipped.length} as they were` : ""}`);
    });
    // Refused, the file and passphrase stay for another try.
    if (!ok) return;
    state.adding = false;
    return refresh(true);
  }
  if (act === "add-agent") {
    if (await attempt(() => api("POST", "/accounts/files", { harness: el.dataset.name }), "Login added to the pool")) state.adding = false;
    return refresh(true);
  }
  if (act === "add-key") {
    const known = state.providers.find((p) => p.name === state.keyProvider);
    const env = {};
    if (known) document.querySelectorAll("#pool .keyvar").forEach((input) => input.value.trim() && (env[input.dataset.var] = input.value.trim()));
    else if ($("key-var").value.trim()) env[$("key-var").value.trim()] = $("key-value").value.trim();
    const provider = known ? known.name : $("key-provider").value.trim().toLowerCase();
    if (!provider) return toast("Name the provider first", "error");
    if (!Object.keys(env).length || (known && Object.keys(env).length < known.env.length)) return toast("Fill in the key first", "error");
    const tags = $("key-tags").value.split(",").map((t) => t.trim()).filter(Boolean);
    const added = await attempt(() => api("POST", "/accounts/key", { provider, label: $("key-label").value.trim() || null, tags, env }), "Key added to the pool");
    if (!added) return;
    state.adding = false;
    return refresh(true);
  }
  if (["login", "relogin", "fix", "login-cancel", "login-code"].includes(act)) return loginAction(el, account);
  const done = await attempt(
    async () => {
      if (act === "capture") await api("POST", "/accounts/capture", {});
      if (act === "share") await api("POST", "/team/share", { account: id, keep: true });
      if (act === "toggle") await api("PATCH", `/accounts/${id}`, { enabled: !account.enabled });
      if (act === "tags") await api("PATCH", `/accounts/${id}`, { tags: el.value.split(",").map((t) => t.trim()).filter(Boolean) });
      if (act === "remove") await api("DELETE", `/accounts/${id}`);
    },
    { share: `${account?.label} is in the team's pool`, capture: "Login added to the pool", toggle: account?.enabled ? `${account?.label} disabled` : `${account?.label} enabled`, tags: "Tags saved", remove: `${account?.label} removed` }[act],
  );
  if (act === "capture" && done) state.adding = false;
  await refresh(true);
  return done;
}

// Signing in happens in the browser, driven by Claude Code itself; the
// window only starts it, shows that it is waiting, and can call it off.
async function loginAction(el, account) {
  const act = el.dataset.act;
  if (act === "login-cancel") {
    await attempt(() => api("DELETE", "/logins"), "Sign-in cancelled");
    return refresh(true);
  }
  if (act === "login-code") {
    const code = $("login-code").value.trim();
    if (!code) return toast("Paste the code from the sign-in page first", "error");
    await attempt(() => api("POST", "/logins/code", { code }), "Code sent. Finishing the sign-in…");
    return refresh(true);
  }
  const issue = act === "fix" ? state.issues[Number(el.dataset.i)] : null;
  if (issue?.action === "add_current") {
    await attempt(() => api("POST", "/accounts/capture", {}), "Login added to the pool");
    return refresh(true);
  }
  const request =
    act === "login"
      ? { email: $("login-email").value.trim() || null, tags: $("login-tags").value.split(",").map((t) => t.trim()).filter(Boolean) }
      : { email: issue?.email ?? account?.email ?? null };
  await attempt(() => api("POST", "/logins", request), "Finish signing in in your browser");
  return refresh(true);
}

async function sessionAction(el) {
  const account = state.accounts.find((a) => a.id === el.value);
  await attempt(() => api("POST", `/sessions/${el.dataset.id}/swap`, { account: el.value }), `Switching to ${account?.label}. It takes effect within about a minute.`);
  await refresh(true);
}

const busy = () => {
  const active = document.activeElement;
  if (state.adding && state.addKind === "machine") return false;
  return (
    document.querySelector("dialog[open]") ||
    (active && ["INPUT", "SELECT", "TEXTAREA"].includes(active.tagName) && active.type !== "checkbox") ||
    document.querySelector('main [data-armed="yes"]') ||
    // Something typed and not yet saved. The rules page keeps its own state
    // and is never redrawn from here, so what is unsaved there holds nothing up.
    [...document.querySelectorAll("main > section:not(#policies) :is(input:not([type=checkbox]):not([type=file]), textarea)")].some((el) => el.value !== el.defaultValue && !el.closest("dialog:not([open])"))
  );
};
let redrawing = false;
let owed = false;
// A page whose content has not changed is left as it is: focus, scrolling and
// a half-made choice all survive a refresh that found nothing new.
for (const id of ["pool", "sessions", "agents", "team", "settings"]) {
  const page = $(id);
  const html = Object.getOwnPropertyDescriptor(Element.prototype, "innerHTML");
  let last;
  Object.defineProperty(page, "innerHTML", {
    get: () => html.get.call(page),
    set: (next) => {
      // Asked for outright, it is drawn afresh: an action may have left a
      // control in a state only a new drawing clears.
      if (next === last && !redrawing) return;
      // The control that had focus has it again in the new drawing.
      const had = page.contains(document.activeElement) ? document.activeElement : null;
      const key = had && ["act", "id", "email", "i", "name"].map((k) => had.dataset[k] ?? "").join("|");
      // Only while the dialog is in use, and never a secret or a chosen file:
      // those are typed again rather than carried about.
      const fields = 'dialog[open] :is(input:not([type="password"]):not([type="file"]), textarea)';
      const typed = new Map(state.adding ? [...page.querySelectorAll(fields)].filter((el) => el.value && (el.id || el.dataset.var)).map((el) => [el.id || el.dataset.var, el.value]) : []);
      html.set.call(page, (last = next));
      for (const el of page.querySelectorAll(fields.replace("[open]", ""))) if (!el.value && typed.has(el.id || el.dataset.var)) el.value = typed.get(el.id || el.dataset.var);
      // On a phone a row is a card: each cell says which column it was.
      for (const table of page.querySelectorAll("table")) {
        if (!table.tHead) continue;
        const heads = [...table.querySelectorAll("thead th")].map((th) => th.textContent.trim());
        for (const row of table.tBodies[0]?.rows ?? []) [...row.cells].forEach((cell, i) => heads[i] && (cell.dataset.label = heads[i]));
      }
      // A button or a switch gets its focus back. A select does not: its
      // choice is made, and focus left there would hold the next refresh up.
      if (had?.dataset.act && (had.tagName === "BUTTON" || had.type === "checkbox")) [...page.querySelectorAll(`[data-act="${had.dataset.act}"]`)].find((el) => ["act", "id", "email", "i", "name"].map((k) => el.dataset[k] ?? "").join("|") === key)?.focus();
    },
  });
}
// However the add dialog closes (its button, Esc), it is no longer in use,
// and what was typed in it, a key above all, does not wait there.
document.addEventListener(
  "close",
  (e) => {
    if (e.target.id !== "add") return;
    state.adding = false;
    for (const field of e.target.querySelectorAll("input, textarea")) field.value = "";
  },
  true,
);
// A second press for what cannot be taken back with one.
document.addEventListener(
  "click",
  (e) => {
    const el = e.target.closest?.("button[data-confirm], input[data-confirm]");
    if (!el || el.dataset.armed === "yes") return;
    e.stopImmediatePropagation();
    e.preventDefault();
    if (el.tagName === "INPUT") {
      // A switch has no words of its own to change: say it beside it.
      el.dataset.armed = "yes";
      toast(el.dataset.confirm);
      return setTimeout(() => el.isConnected && (el.dataset.armed = ""), 5000);
    }
    const was = el.textContent;
    // A fixed name would hide the question from a screen reader.
    const name = el.getAttribute("aria-label");
    el.removeAttribute("aria-label");
    el.dataset.armed = "yes";
    el.textContent = el.dataset.confirm;
    setTimeout(() => el.isConnected && ((el.dataset.armed = ""), (el.textContent = was), name && el.setAttribute("aria-label", name)), 5000);
  },
  true,
);

function renderSummary() {
  const free = state.stats.headroom_5h == null ? "" : `${Math.round(state.stats.headroom_5h)}% of the 5-hour allowance free`;
  $("summary").textContent = state.offline ?? (free || "llmpool is running");
  // Old numbers must not pass for live ones, on a phone least of all.
  $("offline").hidden = !state.offline;
  const stale = state.offline ? `${state.offline} · ${state.loadedAt ? `showing what was loaded at ${state.loadedAt}` : "nothing has loaded yet"}; trying again every few seconds.` : "";
  // Said once, not every three seconds, to whoever is listening.
  if ($("offline").textContent !== stale) $("offline").textContent = stale;
  $("summary").className = state.offline ? "offline" : "muted online";
  // Not being set up yet is not a problem with an account.
  const problems = (SERVER ? 0 : state.issues.filter((i) => i.action !== "install").length) + state.accounts.filter((a) => !a.shared && a.health !== "ok").length;
  const counts = {
    pool: problems ? "!" : state.accounts.length || "",
    sessions: SERVER ? (state.report?.in_flight ? state.report.in_flight : "") : state.sessions.length || "",
    policies: (state.remotePolicy.policy?.rule?.length ?? 0) + (state.remotePolicy.team?.rule?.length ?? 0) || "",
    agents: state.agents.harnesses.filter((h) => h.installed).length + 1,
    team: state.team.state === "connected" ? (state.team.message ? "!" : "on") : state.team.state === "waiting" ? "…" : "",
    settings: "",
  };
  for (const [name, value] of Object.entries(counts)) {
    $(`n-${name}`).textContent = value;
    $(`n-${name}`).className = `count${value === "!" ? " alert" : ""}`;
  }
}

async function refresh(force = false) {
  try {
    const before = state.login.state;
    // The two lists are the heart of the window; anything else that fails
    // keeps its last value and the rest of the window still draws.
    [state.accounts, state.sessions] = await Promise.all([api("GET", "/accounts"), api("GET", "/sessions")]);
    const optional = async (key, path) => {
      try {
        state[key] = await api("GET", path);
      } catch (e) {
        console.warn(path, e);
      }
    };
    await Promise.all([
      optional("events", "/events"),
      optional("issues", "/issues"),
      optional("login", "/logins"),
      optional("stats", "/stats"),
      optional("config", "/config"),
      optional("team", "/team"),
      SERVER ? optional("server", "/server") : null,
      loadReport(),
      optional("agents", "/harnesses"),
      state.providers.length ? null : optional("providers", "/providers"),
      optional("remotePolicy", "/policies"),
    ]);
    if (state.loginItem === null && !SERVER) {
      state.loginItem = await invoke("login_item", { state: "status" }).then((text) => /starts at login/.test(text) && !/does not/.test(text), () => false);
    }
    // A sign-in that just ended is news, whichever way it went.
    if (before === "waiting" && state.login.state !== "waiting") {
      toast(state.login.message, state.login.state === "done" ? "ok" : "error");
      force = true;
    }
    // An account sent from someone's machine has arrived: say so and get
    // out of the way.
    if (state.adding && state.addKind === "machine") {
      const arrived = state.accounts.filter((a) => !(state.addBaseline ?? []).includes(a.id));
      if (arrived.length) {
        toast(`${arrived.map((a) => a.label).join(", ")} arrived`);
        state.adding = false;
        force = true;
      }
    }
    state.offline = null;
    state.loadedAt = new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  } catch (e) {
    state.offline = unreachable(e);
    // A drawing that was asked for and could not be made is still owed.
    owed ||= Boolean(force);
    return renderSummary();
  }
  renderSummary();
  // Redrawing would take the field out from under someone using it.
  if (force || owed || !busy()) {
    redrawing = Boolean(force) || owed;
    try {
      renderPool();
      renderSessions();
      renderAgents();
      renderTeam();
      renderSettings();
      owed = false;
    } finally {
      redrawing = false;
    }
  }
}

// Tabs: click, or arrow keys as the tab pattern expects.
const tabs = [...document.querySelectorAll('[role="tab"]')];
function selectTab(tab) {
  for (const t of tabs) {
    const on = t === tab;
    t.setAttribute("aria-selected", String(on));
    t.tabIndex = on ? 0 : -1;
    $(t.getAttribute("aria-controls")).hidden = !on;
  }
  tab.focus();
  tab.scrollIntoView({ inline: "nearest", block: "nearest" });
  // A new page starts at its top, whichever of the two is scrolling.
  $("main").scrollTop = 0;
  window.scrollTo(0, 0);
}
$("tabs").addEventListener("click", (e) => e.target.closest('[role="tab"]') && selectTab(e.target.closest('[role="tab"]')));
const go = (name) => selectTab($(`tab-${name}`));
// A server shows fewer pages; the hidden ones are not there for keys either.
const shown = () => tabs.filter((t) => !t.hidden);
$("tabs").addEventListener("keydown", (e) => {
  const live = shown();
  const at = live.indexOf(document.activeElement);
  const next = { ArrowDown: at + 1, ArrowUp: at - 1, ArrowRight: at + 1, ArrowLeft: at - 1, Home: 0, End: live.length - 1 }[e.key];
  if (at < 0 || next === undefined) return;
  e.preventDefault();
  selectTab(live[(next + live.length) % live.length]);
});

$("pool").addEventListener("click", (e) => {
  if (e.target.matches("a[data-act]")) e.preventDefault();
  if (e.target.matches("button[data-act], a[data-act]")) poolAction(e.target);
});
$("pool").addEventListener("change", (e) => e.target.matches("input[data-act], select[data-act]") && poolAction(e.target));
// Enter in the email field starts the sign-in.
$("pool").addEventListener("keydown", (e) => {
  if (e.key !== "Enter") return;
  if (e.target.id === "login-email") document.querySelector('[data-act="login"]')?.click();
  if (e.target.id === "login-code") document.querySelector('[data-act="login-code"]')?.click();
});
$("sessions").addEventListener("click", async (e) => {
  if (SERVER || !e.target.matches('button[data-act="advise"]')) return;
  e.target.disabled = true;
  await attempt(() => api("POST", `/sessions/${e.target.dataset.id}/advise`), "The decision model answered");
  await refresh(true);
});
$("sessions").addEventListener("change", (e) => !SERVER && e.target.matches("select[data-act]") && sessionAction(e.target));
for (const page of ["settings", "team", "agents"]) {
  $(page).addEventListener("click", (e) => {
    if (!e.target.matches("button[data-act], a[data-act]")) return;
    e.preventDefault();
    settingsAction(e.target);
  });
  $(page).addEventListener("change", (e) => e.target.matches("input[data-act]") && settingsAction(e.target));
}
// What is typed for the team's server survives a redraw.
$("team").addEventListener("input", (e) => e.target.id === "team-url" && (state.teamUrl = e.target.value));
// The name on an invitation follows what is typed, link and all.
$("team").addEventListener("change", (e) => {
  if (e.target.id !== "invite-from") return;
  state.inviteFrom = e.target.value;
  renderTeam();
});
$("team").addEventListener("keydown", (e) => {
  if (e.key !== "Enter") return;
  if (e.target.id === "team-url") document.querySelector('[data-act="team-signin"]')?.click();
  if (e.target.id === "team-key") document.querySelector('[data-act="team-key"]')?.click();
});
$("settings").addEventListener("keydown", (e) => e.key === "Enter" && e.target.id === "advisor-key" && document.querySelector('[data-act="key-save"]')?.click());
$("policies").addEventListener("click", (e) => e.target.matches("button[data-act]") && policyAction(e.target));
$("policies").addEventListener("change", (e) => e.target.matches("select[data-act], input[type=checkbox][data-act]") && policyAction(e.target));
$("policies").addEventListener("input", (e) => e.target.matches("textarea[data-act], input[data-act]:not([type=checkbox])") && policyAction(e.target));

// Keys inside the window: ⌘1–3 sections, ⌘S save rules, ⌘R refresh.
window.addEventListener("keydown", (e) => {
  if (!(e.metaKey || e.ctrlKey) || e.altKey) return;
  // In a browser, reload and the tab numbers are the browser's.
  if (SERVER && e.key !== "s") return;
  const tab = shown()[Number(e.key) - 1];
  if (tab) {
    e.preventDefault();
    selectTab(tab);
  } else if (e.key === "s" && state.dirty) {
    e.preventDefault();
    savePolicy();
  } else if (e.key === "r") {
    e.preventDefault();
    refresh(true).then(() => toast("Refreshed"));
  }
});

// Messages from the tray and the global hotkeys.
window.__TAURI__.event.listen("llmpool-message", (event) => {
  go("sessions");
  toast(event.payload, "error");
});

// On a server: only what a server has, and a way out.
if (SERVER) {
  for (const name of ["agents", "settings"]) $(`tab-${name}`).hidden = true;
  $("tab-team").querySelector("span").textContent = "Server";
  $("tab-sessions").querySelector("span").textContent = "Requests";
  // The Requests page shares the handlers of the pages beside it.
  $("sessions").addEventListener("click", (e) => e.target.matches("button[data-act]") && settingsAction(e.target));
  $("sessions").addEventListener("change", async (e) => {
    if (!e.target.matches('select[data-act="report-filter"]')) return;
    state.reportFilter[e.target.dataset.name] = e.target.value;
    await loadReport();
    renderRequests();
  });
  document.querySelector(".keys").innerHTML = `<a href="#" id="sign-out">Sign out</a>`;
  $("sign-out").addEventListener("click", async (e) => {
    e.preventDefault();
    await invoke("sign_out").catch(() => {});
    location.replace("/login");
  });
  document.title = "llmpool server";
}

// A section can be opened by address: `#sessions`, `#team`, ...
const fromHash = () => shown().includes($(`tab-${location.hash.slice(1)}`)) && go(location.hash.slice(1));
window.addEventListener("hashchange", fromHash);
fromHash();

// Leaving with unsaved rules would silently drop them.
window.addEventListener("beforeunload", (e) => state.dirty && e.preventDefault());

(async () => {
  try {
    state.schema = await api("GET", "/policies/schema");
    await loadPolicy();
  } catch (e) {
    state.offline = unreachable(e);
  }
  await refresh(true);
  renderPolicies();
  setInterval(refresh, 3000);
})();
