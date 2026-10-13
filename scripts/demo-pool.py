#!/usr/bin/env python3
"""Fill a throwaway llmpool with made-up accounts, sessions and rules.

    eval "$(scripts/demo-pool.py /tmp/llmpool-demo)"     # sets HOME and LLMPOOL_HOME for this shell
    scripts/dev-ui.py                                    # the interface, with the made-up pool in it

For screenshots, design work and browser tests: nothing here is a real login
and nothing touches your own pool. The daemon it starts keeps running; stop it
with `llmpool daemon --stop` in the same shell, then delete the directory.
"""
import json
import os
import pathlib
import subprocess
import sys
import time
import urllib.request

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "/tmp/llmpool-demo").resolve()
home, data = root / "home", root / "data"
binary = os.environ.get("LLMPOOL_BIN") or str(pathlib.Path(__file__).resolve().parent.parent / "target/debug/llmpool")
env = {
    **os.environ,
    "HOME": str(home),
    "LLMPOOL_HOME": str(data),
    "LLMPOOL_FILE_VAULT": "1",
    "LLMPOOL_NO_CLAUDE_KEYCHAIN": "1",
    "LLMPOOL_NO_NOTIFY": "1",
    "LLMPOOL_NO_RENEW": "1",
}
quiet = {"stdin": subprocess.DEVNULL, "stdout": subprocess.DEVNULL, "stderr": subprocess.DEVNULL}


def login(directory, email, token, config=None):
    directory.mkdir(parents=True, exist_ok=True)
    soon = int(time.time() * 1000)
    oauth = {"accessToken": token, "refreshToken": "r-" + token, "expiresAt": soon + 8 * 3600_000, "refreshTokenExpiresAt": soon + 25 * 86400_000}
    (directory / ".credentials.json").write_text(json.dumps({"claudeAiOauth": oauth}))
    (config or directory / ".claude.json").write_text(json.dumps({"oauthAccount": {"emailAddress": email, "organizationUuid": "org-demo"}}))


login(home / ".claude", "dana@acme.dev", "demo-0", home / ".claude.json")
(home / ".claude/settings.json").write_text("{}")
(home / ".codex").mkdir(parents=True, exist_ok=True)
# An ID token whose only content is {"email": "dana@acme.dev"}.
(home / ".codex/auth.json").write_text(json.dumps({"tokens": {"id_token": "h.eyJlbWFpbCI6ImRhbmFAYWNtZS5kZXYifQ.s", "refresh_token": "demo"}}))
subprocess.run([binary, "list"], env=env, **quiet)  # starts the daemon
where = json.loads((data / "daemon.json").read_text())


def api(method, path, body=None):
    request = urllib.request.Request(
        f"http://127.0.0.1:{where['port']}{path}",
        method=method,
        data=None if body is None else json.dumps(body).encode(),
        headers={"Authorization": "Bearer " + where["token"]},
    )
    return json.loads(urllib.request.urlopen(request).read() or b"null")


api("POST", "/accounts/capture", {})
for i, (email, tags) in enumerate([("team-1@acme.dev", ["work"]), ("team-2@acme.dev", ["work"]), ("research@acme.dev", ["research"])]):
    scratch = data / f"login-demo{i}"
    login(scratch, email, f"demo-{i + 1}")
    api("POST", "/accounts/capture", {"profile_dir": str(scratch), "tags": tags})
api("POST", "/accounts/key", {"provider": "openai", "label": "openai-work", "tags": ["work"], "env": {"OPENAI_API_KEY": "sk-demo-0000000000000000"}})
api("POST", "/accounts/key", {"provider": "xai", "tags": ["research"], "env": {"XAI_API_KEY": "xai-demo-0000000000000000"}})
api("POST", "/accounts/key", {"provider": "gemini", "env": {"GEMINI_API_KEY": "demo-0000000000000000"}})
api("POST", "/accounts/files", {"harness": "codex"})

# Sessions belong to a process; this one stands in for the agents.
stand_in = subprocess.Popen(["sleep", "86400"], **quiet, start_new_session=True)
now = int(time.time())


def session(cwd, account, five=None, seven=None, **extra):
    started = api("POST", "/sessions", {"cwd": str(home / cwd), "pid": stand_in.pid, "account": account, **extra})
    if five is not None:
        limits = {"five_hour": {"used_percentage": five, "resets_at": now + 2 * 3600 + 900}, "seven_day": {"used_percentage": seven, "resets_at": now + 3 * 86400}}
        api("POST", f"/statusline/claude/{started['session_id']}", {"rate_limits": limits})


session("work/api-gateway", "team-1@acme.dev", 78, 41)
session("work/web-app", "team-2@acme.dev", 34, 62)
session("research/evals", "research@acme.dev", 93, 20)
session("work/cli", "openai-work", agent="codex", provider="openai")
api("PUT", "/policies", {"text": """[[rule]]
name = "work repos use work accounts"
on = "session_start"
when = { cwd = "~/work/**" }
do = { assign = { tag = "work", pick = "least_busy" } }

[[rule]]
name = "move before the limit"
on = { usage_above = { window = "5h", percent = 90 } }
do = { swap = { tag = "same" }, notify = true }
"""})
for name in ["HOME", "LLMPOOL_HOME", "LLMPOOL_FILE_VAULT", "LLMPOOL_NO_CLAUDE_KEYCHAIN", "LLMPOOL_NO_NOTIFY", "LLMPOOL_NO_RENEW"]:
    print(f"export {name}='{env[name]}'")
