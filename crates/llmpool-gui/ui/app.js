// llmpool window. Talks to the daemon through one Tauri command, `api`,
// which forwards to the daemon's HTTP API.

const invoke = window.__TAURI__.core.invoke;
const api = (method, path, body) => invoke("api", { method, path, body: body ?? null });

const state = {
  accounts: [],
  sessions: [],
  events: [],
  schema: { triggers: [], conditions: [], picks: [] },
  policy: { settings: {}, rule: [] },
  policyText: "",
  policyView: "rules", // or "toml"
  policyMessage: null, // { kind: "ok" | "error", text }
  results: {}, // rule index -> dry-run text
  dirty: false,
};

const $ = (id) => document.getElementById(id);
const esc = (value) =>
  String(value ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
const options = (values, selected, labels = {}) =>
  values.map((v) => `<option value="${esc(v)}"${v === selected ? " selected" : ""}>${esc(labels[v] ?? v)}</option>`).join("");

// ---- usage ------------------------------------------------------------------

function resetsIn(at) {
  if (!at) return "";
  const minutes = Math.floor((at * 1000 - Date.now()) / 60000);
  if (minutes <= 0) return "";
  const days = Math.floor(minutes / 1440);
  const hours = Math.floor((minutes % 1440) / 60);
  return "resets in " + (days > 0 ? `${days}d ${hours}h` : hours > 0 ? `${hours}h ${minutes % 60}m` : `${minutes}m`);
}

function meter(usage, name) {
  const w = usage?.windows?.[name];
  if (!w) return `<span class="muted">no reading</span>`;
  const p = Math.round(w.used_percent);
  const level = p >= 95 ? "bad" : p >= 80 ? "warn" : "";
  return `<div class="meter ${level}" title="${esc(name)} window">
    <div class="track"><div class="fill" style="width:${Math.min(100, p)}%"></div></div>
    <span class="value">${p}%</span><small>${esc(resetsIn(w.resets_at))}</small></div>`;
}

// ---- pool -------------------------------------------------------------------

function renderPool() {
  const rows = state.accounts
    .map((a) => {
      const status = !a.enabled
        ? `<span class="badge">disabled</span>`
        : a.health !== "ok"
          ? `<span class="badge bad">${esc(a.health.replace(/_/g, " "))}</span>`
          : a.is_default
            ? `<span class="badge">default login</span>`
            : "";
      return `<tr>
        <td><strong>${esc(a.label)}</strong> ${status}<div class="muted">${a.sessions.length ? `${a.sessions.length} session${a.sessions.length > 1 ? "s" : ""}` : ""}</div></td>
        <td><input class="tags" data-act="tags" data-id="${esc(a.id)}" value="${esc(a.tags.join(", "))}" placeholder="add tags" aria-label="Tags for ${esc(a.label)}" /></td>
        <td>${meter(a.usage, "5h")}</td>
        <td>${meter(a.usage, "7d")}</td>
        <td class="actions">
          <button data-act="toggle" data-id="${esc(a.id)}">${a.enabled ? "Disable" : "Enable"}</button>
          <button class="quiet danger" data-act="remove" data-id="${esc(a.id)}">Remove</button>
        </td></tr>`;
    })
    .join("");
  $("pool").innerHTML = `
    <div class="toolbar">
      <button class="primary" data-act="capture">Add the login Claude Code has now</button>
      <span class="muted">For another account, run <code>llmpool add claude</code> in a terminal.</span>
    </div>
    <div id="pool-message"></div>
    ${
      rows
        ? `<table><thead><tr><th>Account</th><th>Tags</th><th>5-hour window</th><th>7-day window</th><th></th></tr></thead><tbody>${rows}</tbody></table>`
        : `<p class="empty">No accounts yet. Add the login Claude Code is using now to start the pool.</p>`
    }`;
}

// ---- sessions ---------------------------------------------------------------

function renderSessions() {
  const usable = state.accounts.filter((a) => a.enabled && a.health === "ok");
  const rows = state.sessions
    .map((s) => {
      const picker = s.store
        ? `<select data-act="swap" data-id="${esc(s.id)}" aria-label="Account for session ${esc(s.id)}">${usable
            .map((a) => `<option value="${esc(a.id)}"${a.id === s.account_id ? " selected" : ""}>${esc(a.label)}</option>`)
            .join("")}</select>`
        : `<span class="muted">default login (not managed)</span>`;
      return `<tr><td>${esc(s.id)}</td><td>${picker}</td><td>${meter(s.usage, "5h")}</td><td>${meter(s.usage, "7d")}</td>
        <td class="path" title="${esc(s.cwd)}">${esc(s.cwd)}</td></tr>`;
    })
    .join("");
  const events = state.events
    .slice(-40)
    .reverse()
    .map((e) => `<li><time>${new Date(e.at * 1000).toLocaleTimeString()}</time><span>${esc(e.message)}</span></li>`)
    .join("");
  $("sessions").innerHTML = `
    <div id="sessions-message"></div>
    ${
      rows
        ? `<table><thead><tr><th>Session</th><th>Account</th><th>5-hour window</th><th>7-day window</th><th>Directory</th></tr></thead><tbody>${rows}</tbody></table>`
        : `<p class="empty">No sessions. Start one with <code>llmpool run</code>.</p>`
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
};
const CONDITION_LABELS = { cwd: "directory matches", git_remote: "git remote matches", model: "model matches", account_tag: "current account is tagged" };
const ACTION_LABELS = { assign: "use", swap: "switch to", ask: "ask me, among", notify: "only tell me", hold: "do nothing" };
const PICK_LABELS = { most_quota: "with the most quota left", soonest_reset: "whose week resets soonest", round_robin: "next in turn" };
const TRIGGER_EVENT = { usage_above: "usage_updated" };

const triggerKind = (rule) => (typeof rule.on === "string" ? rule.on : Object.keys(rule.on)[0]);
const triggerParams = (rule) => (typeof rule.on === "string" ? {} : rule.on[triggerKind(rule)]);
const actionKind = (rule) => ["assign", "swap", "ask"].find((k) => rule.do[k]) ?? (rule.do.hold ? "hold" : "notify");
const allTags = () => [...new Set(state.accounts.flatMap((a) => a.tags))].sort();

function targetValue(target) {
  if (target?.account) return `account:${target.account}`;
  if (target?.tag) return `tag:${target.tag}`;
  return "any";
}

function renderRule(rule, i) {
  const kind = triggerKind(rule);
  const params = triggerParams(rule);
  const action = actionKind(rule);
  const target = rule.do[action];
  const when = rule.when ?? {};
  const conditions = Object.keys(CONDITION_LABELS).filter((k) => when[k] !== undefined);

  const triggerExtra =
    kind === "usage_above"
      ? `<input class="num" type="number" min="1" max="100" data-act="percent" data-i="${i}" value="${esc(params.percent ?? 90)}" aria-label="Percent" />
         <span>% of the</span>
         <select data-act="window" data-i="${i}" aria-label="Window">${options(["5h", "7d"], params.window ?? "5h", { "5h": "5-hour window", "7d": "7-day window" })}</select>`
      : "";

  const conditionRows = conditions
    .map(
      (key) => `<span class="line">
        <select data-act="cond-key" data-i="${i}" data-key="${key}" aria-label="Condition">${options(Object.keys(CONDITION_LABELS), key, CONDITION_LABELS)}</select>
        <input class="wide" data-act="cond-value" data-i="${i}" data-key="${key}" value="${esc(when[key])}" placeholder="${key === "cwd" ? "~/work/acme/**" : "*"}" aria-label="Value" />
        <button class="quiet" data-act="cond-remove" data-i="${i}" data-key="${key}" aria-label="Remove condition">×</button></span>`,
    )
    .join("");
  const timeNote = when.time ? `<span class="badge" title="Edit in the TOML view">time window</span>` : "";

  const targets = [
    ["any", "any account"],
    ...(action === "swap" ? [["tag:same", "an account with the same tags"]] : []),
    ...allTags().map((t) => [`tag:${t}`, `an account tagged ${t}`]),
    ...state.accounts.map((a) => [`account:${a.label}`, a.label]),
  ];
  const current = targetValue(target);
  if (!targets.some(([v]) => v === current)) targets.push([current, current.replace(/^(tag|account):/, "")]);
  const targetControls = target
    ? `<select data-act="target" data-i="${i}" aria-label="Which account">${targets
        .map(([v, label]) => `<option value="${esc(v)}"${v === current ? " selected" : ""}>${esc(label)}</option>`)
        .join("")}</select>
       ${target.account || action === "ask" ? "" : `<select data-act="pick" data-i="${i}" aria-label="How to choose">${options(state.schema.picks, target.pick ?? "most_quota", PICK_LABELS)}</select>`}`
    : "";
  const actions = kind === "session_start" ? ["assign", "ask", "notify", "hold"] : ["swap", "ask", "notify", "hold"];
  const notify =
    action === "swap" || action === "ask"
      ? `<label><input type="checkbox" data-act="notify" data-i="${i}"${rule.do.notify ? " checked" : ""} /> and tell me</label>`
      : "";

  return `<div class="rule${rule.enabled === false ? " off" : ""}">
    <div class="head">
      <input type="checkbox" data-act="enabled" data-i="${i}"${rule.enabled === false ? "" : " checked"} aria-label="Rule enabled" />
      <input class="name" data-act="name" data-i="${i}" value="${esc(rule.name)}" aria-label="Rule name" />
      <button class="quiet" data-act="up" data-i="${i}"${i === 0 ? " disabled" : ""} aria-label="Move up">↑</button>
      <button class="quiet" data-act="down" data-i="${i}"${i === state.policy.rule.length - 1 ? " disabled" : ""} aria-label="Move down">↓</button>
      <button data-act="test" data-i="${i}">Test</button>
      <button class="quiet danger" data-act="delete" data-i="${i}">Delete</button>
    </div>
    <div class="line"><span class="word">When</span>
      <select data-act="trigger" data-i="${i}" aria-label="Trigger">${options(state.schema.triggers.map((t) => t.kind), kind, TRIGGER_LABELS)}</select>${triggerExtra}</div>
    <div class="line"><span class="word">If</span>${conditionRows || `<span class="muted">always</span>`}${timeNote}
      <button class="quiet" data-act="cond-add" data-i="${i}"${conditions.length >= 4 ? " disabled" : ""}>+ condition</button></div>
    <div class="line"><span class="word">Then</span>
      <select data-act="action" data-i="${i}" aria-label="Action">${options(actions, action, ACTION_LABELS)}</select>${targetControls}${notify}</div>
    ${state.results[i] ? `<div class="result muted">${esc(state.results[i])}</div>` : ""}
  </div>`;
}

function renderPolicies() {
  const message = state.policyMessage ? `<span class="${state.policyMessage.kind}">${esc(state.policyMessage.text)}</span>` : "";
  const toolbar = `<div class="toolbar">
      <button data-act="view" data-view="rules"${state.policyView === "rules" ? " disabled" : ""}>Rules</button>
      <button data-act="view" data-view="toml"${state.policyView === "toml" ? " disabled" : ""}>TOML</button>
      <span class="muted">The first rule that matches wins.</span>
      <span class="spacer"></span>${message}
      <button class="primary" data-act="save"${state.dirty ? "" : " disabled"}>Save</button>
    </div>`;
  const body =
    state.policyView === "toml"
      ? `<textarea class="toml" data-act="toml" spellcheck="false" aria-label="Policy as TOML">${esc(state.policyText)}</textarea>`
      : state.policy === null
        ? `<p class="error">The policy file does not parse. Fix it in the TOML view.</p>`
        : `${state.policy.rule.map(renderRule).join("") || `<p class="empty">No rules. Without rules, sessions run on your default login.</p>`}
           <button data-act="add-rule">+ Add rule</button>`;
  $("policies").innerHTML = toolbar + body;
}

function describe(result) {
  const { decision, labels } = result;
  const name = (id) => labels[id] ?? id;
  const o = decision.outcome;
  const what =
    typeof o === "string"
      ? { no_match: "no rule would fire", hold: "do nothing", notify: "tell you", exhausted: "want another account, but none qualifies", cooldown: "wait: this session switched moments ago" }[o]
      : o.assign
        ? `start as ${name(o.assign)}`
        : o.swap
          ? `switch to ${name(o.swap)}`
          : `ask you to choose among ${o.ask.map(name).join(", ")}`;
  return decision.rule ? `Right now, rule "${decision.rule}" would fire and ${what}.` : "Right now, no rule would fire.";
}

async function renderedText() {
  if (state.policyView === "toml") return state.policyText;
  return (await api("POST", "/policies/render", { policy: state.policy })).text;
}

function edit(change) {
  change();
  state.dirty = true;
  state.policyMessage = null;
  state.results = {};
  renderPolicies();
}

function setTrigger(rule, kind) {
  rule.on = kind === "usage_above" ? { usage_above: { window: "5h", percent: 90 } } : kind;
  // `assign` only makes sense when a session starts, `swap` only after.
  const action = actionKind(rule);
  if (kind === "session_start" && action === "swap") setAction(rule, "assign");
  if (kind !== "session_start" && action === "assign") setAction(rule, "swap");
}

function setAction(rule, action) {
  const previous = rule.do[actionKind(rule)];
  const target = typeof previous === "object" ? previous : { pick: "most_quota" };
  rule.do = { notify: action === "notify" || (rule.do.notify && action !== "hold"), hold: action === "hold" };
  if (["assign", "swap", "ask"].includes(action)) rule.do[action] = target;
}

async function policyAction(el) {
  const act = el.dataset.act;
  const i = Number(el.dataset.i);
  const rule = state.policy?.rule?.[i];
  const key = el.dataset.key;
  switch (act) {
    case "view": {
      // Carry unsaved edits across the two views.
      if (state.dirty) {
        try {
          if (el.dataset.view === "toml") state.policyText = await renderedText();
          else state.policy = await parseText();
        } catch (e) {
          state.policyMessage = { kind: "error", text: String(e) };
          return renderPolicies();
        }
      }
      state.policyView = el.dataset.view;
      return renderPolicies();
    }
    case "toml":
      state.policyText = el.value;
      state.dirty = true;
      return void document.querySelector('[data-act="save"]')?.removeAttribute("disabled");
    case "save":
      try {
        const text = await renderedText();
        await api("PUT", "/policies", { text });
        await loadPolicy();
        state.policyMessage = { kind: "ok", text: "Saved" };
      } catch (e) {
        state.policyMessage = { kind: "error", text: String(e) };
      }
      return renderPolicies();
    case "add-rule":
      return edit(() =>
        state.policy.rule.push({ name: "new rule", enabled: true, on: "rate_limited", do: { swap: { pick: "most_quota" }, notify: true, hold: false } }),
      );
    case "delete":
      return edit(() => state.policy.rule.splice(i, 1));
    case "up":
    case "down": {
      const j = act === "up" ? i - 1 : i + 1;
      return edit(() => ([state.policy.rule[i], state.policy.rule[j]] = [state.policy.rule[j], state.policy.rule[i]]));
    }
    case "enabled":
      return edit(() => (rule.enabled = el.checked));
    case "name":
      rule.name = el.value;
      state.dirty = true;
      return void document.querySelector('[data-act="save"]')?.removeAttribute("disabled");
    case "trigger":
      return edit(() => setTrigger(rule, el.value));
    case "percent":
      return edit(() => (rule.on.usage_above.percent = Number(el.value)));
    case "window":
      return edit(() => (rule.on.usage_above.window = el.value));
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
      rule.when[key] = el.value;
      state.dirty = true;
      return void document.querySelector('[data-act="save"]')?.removeAttribute("disabled");
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
    case "test":
      try {
        const kind = triggerKind(rule);
        const session = state.sessions[0];
        const request = { text: await renderedText(), event: TRIGGER_EVENT[kind] ?? kind };
        if (session) request.session_id = session.id;
        state.results = { [i]: describe(await api("POST", "/policies/dry-run", request)) + (session ? ` (tested against session ${session.id})` : " (tested against a new session in your home directory)") };
      } catch (e) {
        state.results = { [i]: String(e) };
      }
      return renderPolicies();
  }
}

// The daemon parses and validates the text; nothing is saved.
async function parseText() {
  return (await api("POST", "/policies/parse", { text: state.policyText })).policy;
}

async function loadPolicy() {
  const loaded = await api("GET", "/policies");
  state.policy = loaded.policy;
  state.policyText = loaded.text;
  state.dirty = false;
  state.results = {};
}

// ---- wiring -----------------------------------------------------------------

function flash(id, error) {
  const el = $(id);
  if (el) el.innerHTML = `<p class="error">${esc(error)}</p>`;
}

async function poolAction(el) {
  const id = el.dataset.id;
  const account = state.accounts.find((a) => a.id === id);
  try {
    if (el.dataset.act === "capture") await api("POST", "/accounts/capture", {});
    if (el.dataset.act === "toggle") await api("PATCH", `/accounts/${id}`, { enabled: !account.enabled });
    if (el.dataset.act === "tags")
      await api("PATCH", `/accounts/${id}`, { tags: el.value.split(",").map((t) => t.trim()).filter(Boolean) });
    if (el.dataset.act === "remove") {
      // Two presses, so a slip does not drop a login.
      if (el.dataset.armed !== "yes") {
        el.dataset.armed = "yes";
        el.textContent = "Really remove?";
        return;
      }
      await api("DELETE", `/accounts/${id}`);
    }
    await refresh(true);
  } catch (e) {
    flash("pool-message", e);
  }
}

async function sessionAction(el) {
  try {
    await api("POST", `/sessions/${el.dataset.id}/swap`, { account: el.value });
    await refresh(true);
  } catch (e) {
    await refresh(true);
    flash("sessions-message", e);
  }
}

const busy = () => {
  const active = document.activeElement;
  return active && ["INPUT", "SELECT", "TEXTAREA"].includes(active.tagName);
};

async function refresh(force = false) {
  try {
    [state.accounts, state.sessions, state.events] = await Promise.all([
      api("GET", "/accounts"),
      api("GET", "/sessions"),
      api("GET", "/events"),
    ]);
    $("health").textContent = "";
  } catch (e) {
    $("health").textContent = String(e);
    return;
  }
  // Redrawing would take the field out from under someone typing in it.
  if (force || !busy()) {
    renderPool();
    renderSessions();
  }
}

document.querySelector("nav").addEventListener("click", (event) => {
  const tab = event.target.closest("[data-tab]")?.dataset.tab;
  if (!tab) return;
  for (const button of document.querySelectorAll("[data-tab]")) button.setAttribute("aria-selected", String(button.dataset.tab === tab));
  for (const panel of document.querySelectorAll("[role=tabpanel]")) panel.hidden = panel.id !== tab;
});

$("pool").addEventListener("click", (e) => e.target.matches("button[data-act]") && poolAction(e.target));
$("pool").addEventListener("change", (e) => e.target.matches("input[data-act]") && poolAction(e.target));
$("sessions").addEventListener("change", (e) => e.target.matches("select[data-act]") && sessionAction(e.target));
$("policies").addEventListener("click", (e) => e.target.matches("button[data-act]") && policyAction(e.target));
$("policies").addEventListener("change", (e) => e.target.matches("select[data-act], input[type=checkbox][data-act], input[type=number][data-act]") && policyAction(e.target));
$("policies").addEventListener("input", (e) => e.target.matches("textarea[data-act], input.name, input.wide") && policyAction(e.target));

(async () => {
  try {
    state.schema = await api("GET", "/policies/schema");
    await loadPolicy();
  } catch (e) {
    $("health").textContent = String(e);
  }
  await refresh(true);
  renderPolicies();
  setInterval(refresh, 3000);
})();
