#!/usr/bin/env python3
"""Live browser view of the Claude <-> Codex coordination channel.

Reads AGENT_HANDOFF.md (the shared state file both agents write) and
.collab/messages.jsonl (verbatim `codex queue` payloads) and serves a live
timeline at http://127.0.0.1:8787.

Standard library only. Deliberately does not read Codex rollout logs: those
hold the user's whole private session, not just inter-agent traffic.
"""

import http.server
import json
import os
import re
import socketserver
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HANDOFF = os.path.join(ROOT, "AGENT_HANDOFF.md")
MESSAGES = os.path.join(ROOT, ".collab", "messages.jsonl")
PORT = int(os.environ.get("AGENT_MONITOR_PORT", "8787"))

# "- 2026-09-15 / Claude / SCOPE: body", with the date optional and an optional
# trailing message id, as in "Codex / MONITOR UPDATE codex-monitor-002: body".
ENTRY = re.compile(
    r"^-\s+(?:(\d{4}-\d{2}-\d{2})\s*/\s*)?"
    r"([\w-]+)\s*/\s*"
    r"([A-Z][A-Z0-9]*(?:\s+[A-Z0-9]+)*)"
    r"(?:\s+([A-Za-z][\w-]*))?"
    r"\s*:\s*(.*)$"
)


def _sections(text):
    """Split markdown into {heading: [lines]} on '## ' boundaries."""
    out, current = {}, None
    for line in text.splitlines():
        if line.startswith("## "):
            current = line[3:].strip()
            out[current] = []
        elif current is not None:
            out[current].append(line)
    return out


def _bullets(lines):
    """Join wrapped bullets into one string each, preserving order."""
    items, buf = [], None
    for line in lines:
        if line.startswith("- "):
            if buf is not None:
                items.append(buf)
            buf = line
        elif buf is not None and line.startswith("  ") and line.strip():
            buf += " " + line.strip()
        elif not line.strip():
            if buf is not None:
                items.append(buf)
                buf = None
    if buf is not None:
        items.append(buf)
    return items


def _entries(lines):
    parsed = []
    for raw in _bullets(lines):
        match = ENTRY.match(raw)
        if match:
            date, agent, tag, msg_id, body = match.groups()
            parsed.append(
                {
                    "date": date,
                    "agent": agent,
                    "tag": tag.strip(),
                    "id": msg_id,
                    "body": body.strip(),
                }
            )
        else:
            parsed.append(
                {"date": None, "agent": None, "tag": None, "id": None, "body": raw[2:].strip()}
            )
    return parsed


def read_state():
    state = {
        "goal": "",
        "ownership": [],
        "messages": [],
        "findings": [],
        "checks": [],
        "raw": [],
        "error": None,
    }
    try:
        with open(HANDOFF, encoding="utf-8") as handle:
            sections = _sections(handle.read())
    except FileNotFoundError:
        state["error"] = "AGENT_HANDOFF.md not found"
        sections = {}
    except OSError as exc:
        state["error"] = str(exc)
        sections = {}

    for name, lines in sections.items():
        low = name.lower()
        if "goal" in low:
            state["goal"] = "\n".join(line for line in lines if line.strip())
        elif "ownership" in low:
            state["ownership"] = _entries(lines)
        elif "message" in low or "handoff" in low:
            state["messages"] = _entries(lines)
        elif "finding" in low or "audit" in low:
            state["findings"] = _entries(lines)
        elif "check" in low:
            state["checks"] = _entries(lines)

    try:
        with open(MESSAGES, encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if not line:
                    continue
                try:
                    state["raw"].append(json.loads(line))
                except ValueError:
                    pass
    except (FileNotFoundError, OSError):
        pass
    return state


def stamp():
    """Cheap change token over both watched files."""
    parts = []
    for path in (HANDOFF, MESSAGES):
        try:
            info = os.stat(path)
            parts.append("{0}:{1}".format(info.st_mtime_ns, info.st_size))
        except OSError:
            parts.append("-")
    return "|".join(parts)


PAGE = r"""<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Agent Monitor</title>
<style>
:root{--bg:#0d1117;--panel:#161b22;--line:#21262d;--fg:#e6edf3;--dim:#8b949e;
--claude:#d98f4e;--codex:#4e9fd9;--user:#b98ae0;--accent:#6ee7a8;--warn:#f0883e}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);
font:14px/1.55 ui-monospace,SFMono-Regular,"Cascadia Mono",Menlo,monospace}
header{position:sticky;top:0;z-index:5;background:rgba(13,17,23,.94);
backdrop-filter:blur(8px);border-bottom:1px solid var(--line);padding:14px 20px;
display:flex;align-items:center;gap:14px;flex-wrap:wrap}
h1{font-size:15px;margin:0;font-weight:600;letter-spacing:.02em}
.live{display:flex;align-items:center;gap:6px;color:var(--dim);font-size:12px}
.dot{width:8px;height:8px;border-radius:50%;background:var(--accent);
box-shadow:0 0 0 0 rgba(110,231,168,.6);animation:p 2.2s infinite}
.dot.off{background:#f85149;animation:none}
@keyframes p{70%{box-shadow:0 0 0 7px rgba(110,231,168,0)}
100%{box-shadow:0 0 0 0 rgba(110,231,168,0)}}
main{max-width:1080px;margin:0 auto;padding:20px 16px 80px}
.panels{display:grid;grid-template-columns:repeat(auto-fit,minmax(280px,1fr));
gap:12px;margin-bottom:26px}
.panel{background:var(--panel);border:1px solid var(--line);border-radius:8px;padding:14px 16px}
.panel h2{font-size:11px;text-transform:uppercase;letter-spacing:.09em;color:var(--dim);
margin:0 0 9px;font-weight:600}
.panel li{margin:0 0 6px;font-size:12.5px}
.panel ul{margin:0;padding-left:16px}
.goal{white-space:pre-wrap;font-size:12.5px}
.row{display:flex;gap:12px;margin:0 0 10px}
.row.codex{flex-direction:row-reverse}
.row.user{justify-content:center}
.msg{max-width:74%;background:var(--panel);border:1px solid var(--line);
border-radius:8px;padding:11px 13px}
.row.claude .msg{border-left:2px solid var(--claude)}
.row.codex .msg{border-right:2px solid var(--codex)}
.row.user .msg{max-width:86%;background:#1b1830;border-color:#3b3163;
border-left:2px solid var(--user);border-right:2px solid var(--user)}
.row.user .who{justify-content:center}
.name.user{color:var(--user)}
.who{font-size:11px;color:var(--dim);margin-bottom:5px;display:flex;gap:8px;align-items:center}
.row.codex .who{justify-content:flex-end}
.name{font-weight:600}
.name.claude{color:var(--claude)}
.name.codex{color:var(--codex)}
.tag{font-size:10px;letter-spacing:.06em;padding:1px 6px;border-radius:3px;
background:var(--line);color:var(--dim)}
.tag.BUG{background:#4a1d1d;color:#ff9c9c}
.tag.GAP{background:#4a3a14;color:var(--warn)}
.tag.POLISH{background:#16324a;color:#79c0ff}
.tag.ACK,.tag.HANDSHAKE{background:#16351f;color:var(--accent)}
.body{font-size:13px;white-space:pre-wrap;word-break:break-word}
.body code{background:#0d1117;border:1px solid var(--line);border-radius:3px;padding:0 4px}
details{margin-top:8px}
summary{cursor:pointer;font-size:11px;color:var(--dim)}
summary:hover{color:var(--fg)}
pre.raw{margin:7px 0 0;padding:9px;background:#0a0d12;border:1px solid var(--line);
border-radius:5px;font-size:11.5px;white-space:pre-wrap;color:var(--dim);
max-height:240px;overflow:auto}
.sep{text-align:center;color:var(--dim);font-size:11px;margin:16px 0 14px;
text-transform:uppercase;letter-spacing:.09em}
.empty{color:var(--dim);font-style:italic;font-size:12.5px}
.chip{font:inherit;font-size:11px;color:var(--dim);background:var(--panel);
border:1px solid var(--line);border-radius:11px;padding:2px 10px;cursor:pointer}
.chip:hover{color:var(--fg);border-color:#3d444d}
.chip[data-who=claude]{color:var(--claude)}
.chip[data-who=codex]{color:var(--codex)}
.chip[data-who=user]{color:var(--user)}
.chip.off{color:var(--dim);opacity:.45;text-decoration:line-through}
.chip.on{color:var(--warn);border-color:var(--warn)}
.banner{background:#4a1d1d;border:1px solid #8b3232;color:#ffb3b3;border-radius:8px;
padding:11px 14px;margin-bottom:18px;font-size:12.5px}
.f-list li{margin-bottom:7px}
@media(max-width:620px){.msg{max-width:100%}.row,.row.codex{flex-direction:column}}
</style></head><body>
<header>
  <h1>nectra &middot; agent monitor</h1>
  <span class="live"><span class="dot" id="dot"></span><span id="status">connecting</span></span>
  <span class="live" id="counts"></span>
  <span class="live" id="filters">
    <button class="chip" data-who="claude" onclick="toggleAuthor('claude')">claude</button>
    <button class="chip" data-who="codex" onclick="toggleAuthor('codex')">codex</button>
    <button class="chip" data-who="user" onclick="toggleAuthor('user')">user</button>
    <button class="chip" id="pause" onclick="togglePause()">pause</button>
  </span>
</header>
<main>
  <div id="banner" class="banner" style="display:none"></div>
  <div class="panels">
    <div class="panel"><h2>Current goal</h2><div class="goal" id="goal"></div></div>
    <div class="panel"><h2>Task ownership</h2><ul id="ownership"></ul></div>
    <div class="panel"><h2>Shared checks</h2><ul id="checks"></ul></div>
  </div>
  <div class="panel" id="findingsPanel" style="margin-bottom:26px;display:none">
    <h2>Audit findings</h2><ul class="f-list" id="findings"></ul>
  </div>
  <div class="sep">coordination timeline &middot; newest first</div>
  <div id="timeline"></div>
</main>
<script>
const esc = s => (s || "").replace(/[&<>]/g, c => ({"&":"&amp;","<":"&lt;",">":"&gt;"}[c]));
const fmt = s => esc(s).replace(/`([^`]+)`/g, "<code>$1</code>");
const el = id => document.getElementById(id);

function agentOf(entry){
  const name = (entry.agent || "").toLowerCase();
  if (name.indexOf("user") === 0) return "user";
  if (name.indexOf("codex") === 0) return "codex";
  return "claude";
}

function label(entry){
  const name = entry.agent || "note";
  return name.replace(/-to-/g, " → ");
}

// Match on message id only. Tag plus date collides whenever a tag repeats,
// which would attach the wrong verbatim payload to an entry.
function rawFor(state, entry){
  if (!entry.id) return null;
  return (state.raw || []).find(r => r.id && r.id === entry.id) || null;
}

let paused = false;
let latest = null;
const hidden = new Set();

function togglePause(){
  paused = !paused;
  const button = el("pause");
  button.textContent = paused ? "resume" : "pause";
  button.classList.toggle("on", paused);
  if (!paused && latest) render(latest);
}

function toggleAuthor(who){
  if (hidden.has(who)) hidden.delete(who); else hidden.add(who);
  document.querySelectorAll("#filters .chip").forEach(chip => {
    chip.classList.toggle("off", hidden.has(chip.dataset.who));
  });
  if (latest) render(latest);
}

function render(state){
  latest = state;
  if (paused) return;

  // A read failure must not render as a healthy empty board.
  const banner = el("banner");
  if (state.error){
    banner.style.display = "";
    banner.textContent = "cannot read handoff file: " + state.error;
    el("dot").classList.add("off");
    el("status").textContent = "stale";
  } else {
    banner.style.display = "none";
  }

  el("goal").textContent = state.goal || "not set";

  const fill = (node, items) => {
    node.innerHTML = items.length
      ? items.map(i => "<li>" + fmt(i.body) + "</li>").join("")
      : '<li class="empty">none yet</li>';
  };
  fill(el("ownership"), state.ownership);
  fill(el("checks"), state.checks);

  el("findingsPanel").style.display = state.findings.length ? "" : "none";
  el("findings").innerHTML = state.findings.map(f =>
    '<li><span class="tag ' + esc(f.tag || "") + '">' + esc(f.tag || "note") + "</span> " +
    '<span class="name ' + agentOf(f) + '">' + esc(f.agent || "") + "</span> " +
    fmt(f.body) + "</li>").join("");

  // Newest first: the file stays append-only chronological, the view inverts it.
  const shown = state.messages.filter(m => !hidden.has(agentOf(m)));
  el("timeline").innerHTML = shown.slice().reverse().map(m => {
    const who = agentOf(m);
    const raw = rawFor(state, m);
    return '<div class="row ' + who + '"><div class="msg">' +
      '<div class="who"><span class="name ' + who + '">' + esc(label(m)) + "</span>" +
      (m.tag ? '<span class="tag ' + esc(m.tag) + '">' + esc(m.tag) + "</span>" : "") +
      (m.id ? "<span>" + esc(m.id) + "</span>" : "") +
      (m.date ? "<span>" + esc(m.date) + "</span>" : "") + "</div>" +
      '<div class="body">' + fmt(m.body) + "</div>" +
      (raw ? '<details><summary>verbatim queue payload</summary><pre class="raw">' +
        esc(raw.text) + "</pre></details>" : "") +
      "</div></div>";
  }).join("") || '<div class="empty">' +
    (hidden.size ? "all authors hidden" : "no messages yet") + "</div>";

  const count = who => state.messages.filter(m => agentOf(m) === who).length;
  el("counts").textContent = count("claude") + " claude · " +
    count("codex") + " codex · " + count("user") + " directives · " +
    state.findings.length + " findings";
}

function connect(){
  const stream = new EventSource("/events");
  stream.onopen = () => {
    el("dot").classList.remove("off");
    el("status").textContent = "live";
  };
  stream.onmessage = event => render(JSON.parse(event.data));
  stream.onerror = () => {
    el("dot").classList.add("off");
    el("status").textContent = "reconnecting";
    stream.close();
    setTimeout(connect, 2000);
  };
}

fetch("/api/state").then(r => r.json()).then(render).catch(() => {});
connect();
</script></body></html>
"""


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    def _harden(self):
        """Loopback-only tool, but a browser page still gets the usual guards."""
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header(
            "Content-Security-Policy",
            "default-src 'self'; script-src 'self' 'unsafe-inline'; "
            "style-src 'self' 'unsafe-inline'; connect-src 'self'; "
            "frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
        )

    def _send(self, code, body, ctype):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self._harden()
        self.end_headers()
        self.wfile.write(body)

    def _host_ok(self):
        """Reject off-origin Host headers so a remote page cannot rebind to us."""
        host = self.headers.get("Host", "")
        return host in ("127.0.0.1:%d" % PORT, "localhost:%d" % PORT, "[::1]:%d" % PORT)

    def do_GET(self):
        if not self._host_ok():
            return self._send(403, b"invalid host", "text/plain; charset=utf-8")
        if self.path.startswith("/events"):
            return self._events()
        if self.path.startswith("/api/state"):
            return self._send(
                200, json.dumps(read_state()).encode("utf-8"), "application/json"
            )
        if self.path in ("/", "/index.html"):
            return self._send(200, PAGE.encode("utf-8"), "text/html; charset=utf-8")
        return self._send(404, b"not found", "text/plain; charset=utf-8")

    def do_POST(self):
        self.send_response(405)
        self.send_header("Allow", "GET")
        self.send_header("Content-Length", "0")
        self._harden()
        self.end_headers()

    do_PUT = do_DELETE = do_PATCH = do_POST

    def _events(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Connection", "keep-alive")
        self._harden()
        self.end_headers()
        last = None
        try:
            while True:
                current = stamp()
                if current != last:
                    last = current
                    payload = json.dumps(read_state())
                    self.wfile.write("data: {0}\n\n".format(payload).encode("utf-8"))
                else:
                    self.wfile.write(b": ping\n\n")
                self.wfile.flush()
                time.sleep(1.0)
        except (BrokenPipeError, ConnectionResetError, OSError):
            return


class Server(socketserver.ThreadingTCPServer):
    daemon_threads = True
    allow_reuse_address = True


def main():
    with Server(("127.0.0.1", PORT), Handler) as httpd:
        print("agent monitor: http://127.0.0.1:{0}".format(PORT), flush=True)
        print("watching: {0}".format(HANDOFF), flush=True)
        print("watching: {0}".format(MESSAGES), flush=True)
        try:
            httpd.serve_forever()
        except KeyboardInterrupt:
            print("stopped", flush=True)


if __name__ == "__main__":
    main()
