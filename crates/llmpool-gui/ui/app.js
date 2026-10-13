// llmpool window. Talks to the daemon through one Tauri command, `api`,
// which forwards to the daemon's HTTP API.

const invoke = window.__TAURI__.core.invoke;
const api = (method, path, body) => invoke("api", { method, path, body: body ?? null });

const state = {
  accounts: [],
  sessions: [],
  events: [],
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

// ---- feedback ---------------------------------------------------------------

function toast(text, kind = "ok") {
  const el = document.createElement("div");
  el.className = `toast ${kind}`;
  el.textContent = String(text).replace(/^Error: /, "");
  $("toasts").append(el);
  setTimeout(() => el.remove(), kind === "error" ? 9000 : 4000);
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

// ---- pool -------------------------------------------------------------------

// A login runs out on a known day; say so while there is still time to act.
function expiryBadge(a) {
  if (!a.login_expires_at || a.health !== "ok") return "";
  const days = Math.floor((a.login_expires_at * 1000 - Date.now()) / 86400000);
  if (days > 7) return "";
  return `<span class="badge warn">⚠ login expires ${days <= 0 ? "today" : `in ${plural(days, "day")}`}</span>`;
}

function accountBadge(a) {
  if (!a.enabled) return `<span class="badge">⏸ disabled</span>`;
  if (a.health !== "ok") return `<span class="badge bad">⚠ needs a new login</span>`;
  if (a.is_default) return `<span class="badge ok">default login</span>`;
  return "";
}

function addPanel() {
  const hasDefault = state.accounts.some((a) => a.is_default);
  return `<div class="callout">
    <div class="steps">
      <div>
        <h3>The login Claude Code uses now</h3>
        <p class="muted">${hasDefault ? "Already in the pool. Press again after logging Claude Code in to a different account." : "One press. Nothing about your Claude Code setup changes."}</p>
        <div class="copy"><button class="primary" data-act="capture">Add current login</button></div>
      </div>
      <div>
        <h3>Another account</h3>
        <p class="muted">Run this in a terminal. It opens Claude Code in a scratch profile; log in, then quit with <code>/exit</code>.</p>
        <div class="copy"><code>llmpool add claude --tag work</code><button class="quiet" data-act="copy" data-text="llmpool add claude --tag work" aria-label="Copy command">Copy</button></div>
      </div>
    </div>
  </div>`;
}

function renderPool() {
  const dead = state.accounts.filter((a) => a.health !== "ok");
  const rows = state.accounts
    .map((a) => {
      const used = a.sessions.length ? `<div class="muted">in use by ${plural(a.sessions.length, "session")}</div>` : "";
      return `<tr>
        <td><strong>${esc(a.label)}</strong> ${accountBadge(a)} ${expiryBadge(a)}${used}</td>
        <td><input class="tags" data-act="tags" data-id="${esc(a.id)}" value="${esc(a.tags.join(", "))}" placeholder="e.g. work, acme" aria-label="Tags for ${esc(a.label)}, comma separated" /></td>
        <td>${meter(a.usage, "5h", "5-hour window")}</td>
        <td>${meter(a.usage, "7d", "7-day window")}</td>
        <td class="actions">
          <button data-act="toggle" data-id="${esc(a.id)}" aria-label="${a.enabled ? "Disable" : "Enable"} ${esc(a.label)}">${a.enabled ? "Disable" : "Enable"}</button>
          <button class="quiet danger" data-act="remove" data-id="${esc(a.id)}" aria-label="Remove ${esc(a.label)}">Remove</button>
        </td></tr>`;
    })
    .join("");
  const warning = dead.length
    ? `<div class="callout bad" role="alert"><strong>⚠ ${esc(dead.map((a) => a.label).join(", "))} ${dead.length > 1 ? "need" : "needs"} a new login.</strong>
        <p>Policies skip ${dead.length > 1 ? "them" : "it"} until then. Run <code>llmpool add claude</code> in a terminal and sign in with that account.</p></div>`
    : "";
  $("pool").innerHTML = `${warning}
    ${
      rows
        ? `<table><caption class="skip">Accounts in the pool</caption>
            <thead><tr><th scope="col">Account</th><th scope="col">Tags</th><th scope="col">5-hour window</th><th scope="col">7-day window</th><th scope="col"><span class="skip">Actions</span></th></tr></thead>
            <tbody>${rows}</tbody></table>
           <p class="muted">Tags group accounts for policies, e.g. “use an account tagged work in ~/work”.</p>`
        : `<p class="empty">No accounts yet. Add the login Claude Code is using now to start the pool.</p>`
    }
    <h2>Add an account</h2>${addPanel()}`;
}

// ---- sessions ---------------------------------------------------------------

const EVENT_ICONS = { swap: "⇄", assign: "▶", adopt: "▶", end: "■", notice: "ℹ", login: "⚠", policy_error: "⚠", agent: "·" };

function renderSessions() {
  const usable = state.accounts.filter((a) => a.enabled && a.health === "ok");
  const rows = state.sessions
    .map((s) => {
      // An account live in another session's profile cannot be put in this one too.
      const picker = s.store
        ? `<select data-act="swap" data-id="${esc(s.id)}" aria-label="Account for the session in ${esc(short(s.cwd))}">${usable
            .map((a) => {
              const elsewhere = a.id !== s.account_id && a.sessions.length > 0;
              return `<option value="${esc(a.id)}"${a.id === s.account_id ? " selected" : ""}${elsewhere ? " disabled" : ""}>${esc(a.label)}${elsewhere ? " (in use)" : ""}</option>`;
            })
            .join("")}</select>`
        : `<span class="muted">your own login, not managed</span>`;
      const kind = s.native ? `<span class="badge" title="Started as plain claude. All such sessions share one login and switch together.">plain claude · shared login</span>` : "";
      return `<tr><td class="path" title="${esc(s.cwd)}"><bdi>${esc(short(s.cwd))}</bdi></td><td>${picker} ${kind}</td>
        <td>${meter(s.usage, "5h", "5-hour window")}</td><td>${meter(s.usage, "7d", "7-day window")}</td></tr>`;
    })
    .join("");
  const events = state.events
    .filter((e) => e.kind !== "agent")
    .slice(-40)
    .reverse()
    .map((e) => `<li><time>${new Date(e.at * 1000).toLocaleTimeString()}</time><span aria-hidden="true">${EVENT_ICONS[e.kind] ?? "·"}</span><span>${esc(tidy(e.message))}</span></li>`)
    .join("");
  $("sessions").innerHTML = `
    ${
      rows
        ? `<table><caption class="skip">Running sessions</caption>
            <thead><tr><th scope="col">Directory</th><th scope="col">Account</th><th scope="col">5-hour window</th><th scope="col">7-day window</th></tr></thead><tbody>${rows}</tbody></table>
           <p class="muted">Choosing another account switches the running session. It takes effect within about a minute.</p>`
        : `<p class="empty">No sessions running. Start Claude Code, or run <code>llmpool run</code> to start one on the account your policies pick.</p>`
    }
    <h2>Recent activity</h2>
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
};
const CONDITION_LABELS = { cwd: "directory matches", git_remote: "git remote matches", model: "model matches", account_tag: "current account is tagged" };
const CONDITION_HINTS = { cwd: "~/work/acme/**", git_remote: "*github.com:acme/*", model: "*opus*", account_tag: "work" };
const ACTION_LABELS = { assign: "use", swap: "switch to", ask: "ask me, among", notify: "only tell me", hold: "do nothing" };
const PICK_LABELS = { most_quota: "with the most quota left", soonest_reset: "whose week resets soonest", round_robin: "next in turn" };
const TRIGGER_EVENT = { usage_above: "usage_updated", schedule: "tick" };
const DAYS = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

const TEMPLATES = {
  "Switch before the limit": { name: "switch before the limit", enabled: true, on: { usage_above: { window: "5h", percent: 90 } }, do: { swap: { tag: "same", pick: "most_quota" }, notify: true, hold: false } },
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
  const toolbar = `<div class="toolbar">
      <div role="group" aria-label="View">
        <button data-act="view" data-view="rules" aria-pressed="${state.policyView === "rules"}">Rules</button>
        <button data-act="view" data-view="toml" aria-pressed="${state.policyView === "toml"}">TOML</button>
      </div>
      <span class="muted">Rules are tried top to bottom; the first that matches wins.</span>
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
    body = `${state.policy.rule.map(renderRule).join("") || `<p class="empty">No rules yet. Without rules, every session runs on your own login and nothing switches.</p>`}
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
  rule.on = kind === "usage_above" ? { usage_above: { window: "5h", percent: 90 } } : kind === "schedule" ? { schedule: { at: "09:00" } } : kind;
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

// ---- wiring -----------------------------------------------------------------

async function poolAction(el) {
  const id = el.dataset.id;
  const account = state.accounts.find((a) => a.id === id);
  const act = el.dataset.act;
  if (act === "copy") return copy(el.dataset.text);
  if (act === "remove" && el.dataset.armed !== "yes") {
    // Two presses, so a slip does not drop a login.
    el.dataset.armed = "yes";
    el.textContent = "Really remove?";
    return setTimeout(() => el.isConnected && ((el.dataset.armed = ""), (el.textContent = "Remove")), 4000);
  }
  const done = await attempt(
    async () => {
      if (act === "capture") await api("POST", "/accounts/capture", {});
      if (act === "toggle") await api("PATCH", `/accounts/${id}`, { enabled: !account.enabled });
      if (act === "tags") await api("PATCH", `/accounts/${id}`, { tags: el.value.split(",").map((t) => t.trim()).filter(Boolean) });
      if (act === "remove") await api("DELETE", `/accounts/${id}`);
    },
    { capture: "Login added to the pool", toggle: account?.enabled ? `${account?.label} disabled` : `${account?.label} enabled`, tags: "Tags saved", remove: `${account?.label} removed` }[act],
  );
  await refresh(true);
  return done;
}

async function sessionAction(el) {
  const account = state.accounts.find((a) => a.id === el.value);
  await attempt(() => api("POST", `/sessions/${el.dataset.id}/swap`, { account: el.value }), `Switching to ${account?.label}. It takes effect within about a minute.`);
  await refresh(true);
}

const busy = () => {
  const active = document.activeElement;
  return active && ["INPUT", "SELECT", "TEXTAREA"].includes(active.tagName);
};

function renderSummary() {
  const managed = state.sessions.length;
  $("summary").textContent = state.offline ?? `${plural(state.accounts.length, "account")} · ${plural(managed, "session")}`;
  $("summary").className = state.offline ? "badge bad" : "muted";
}

async function refresh(force = false) {
  try {
    [state.accounts, state.sessions, state.events] = await Promise.all([api("GET", "/accounts"), api("GET", "/sessions"), api("GET", "/events")]);
    state.offline = null;
  } catch (e) {
    state.offline = `⚠ ${String(e).replace(/^Error: /, "")}`;
    return renderSummary();
  }
  renderSummary();
  // Redrawing would take the field out from under someone using it.
  if (force || !busy()) {
    renderPool();
    renderSessions();
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
}
$("tabs").addEventListener("click", (e) => e.target.matches('[role="tab"]') && selectTab(e.target));
$("tabs").addEventListener("keydown", (e) => {
  const at = tabs.indexOf(document.activeElement);
  const next = { ArrowRight: at + 1, ArrowLeft: at - 1, Home: 0, End: tabs.length - 1 }[e.key];
  if (at < 0 || next === undefined) return;
  e.preventDefault();
  selectTab(tabs[(next + tabs.length) % tabs.length]);
});

$("pool").addEventListener("click", (e) => e.target.matches("button[data-act]") && poolAction(e.target));
$("pool").addEventListener("change", (e) => e.target.matches("input[data-act]") && poolAction(e.target));
$("sessions").addEventListener("change", (e) => e.target.matches("select[data-act]") && sessionAction(e.target));
$("policies").addEventListener("click", (e) => e.target.matches("button[data-act]") && policyAction(e.target));
$("policies").addEventListener("change", (e) => e.target.matches("select[data-act], input[type=checkbox][data-act]") && policyAction(e.target));
$("policies").addEventListener("input", (e) => e.target.matches("textarea[data-act], input[data-act]:not([type=checkbox])") && policyAction(e.target));

// Keys inside the window: ⌘1–3 sections, ⌘S save rules, ⌘R refresh.
window.addEventListener("keydown", (e) => {
  if (!(e.metaKey || e.ctrlKey) || e.altKey) return;
  const tab = { 1: tabs[0], 2: tabs[1], 3: tabs[2] }[e.key];
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
  selectTab(tabs[1]);
  toast(event.payload, "error");
});

// Leaving with unsaved rules would silently drop them.
window.addEventListener("beforeunload", (e) => state.dirty && e.preventDefault());

(async () => {
  try {
    state.schema = await api("GET", "/policies/schema");
    await loadPolicy();
  } catch (e) {
    state.offline = `⚠ ${String(e).replace(/^Error: /, "")}`;
  }
  await refresh(true);
  renderPolicies();
  setInterval(refresh, 3000);
})();
