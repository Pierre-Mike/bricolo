"""Pure funnel logic for the tailscale-funnel sidecar: no processes, no
sockets, no clock. A port of lib/tailscale_funnel.lua from the Hammerspoon
setup; server.py adds the CLI calls, the watchdog and the HTTP intake.

The vocabulary, once:
    lease       a named reservation. One owner, one TTL, renewed on every
                poll. It names the local port the funnel should carry, and
                the probe that proves the far end is the right one.
    generation  bumped every time the funnel is seen coming up, so a caller
                spots "my URL changed" by comparing an integer.
    hostname    <machine>.<tailnet>.ts.net, from `tailscale status --json`.
                Known as soon as the Mac is logged in, armed or not.
    url         the public base URL, and only while the funnel actually
                carries the leased port. Otherwise null.
    fatal       broken until a human acts: no CLI, logged out, Funnel not
                allowed, the funnel pointed at the wrong port.
"""

import json
import math
import re
from urllib.parse import parse_qsl, urlsplit

# ── Constants ────────────────────────────────────────────────────────────────
PORT = 8795  # the intake, first free port after the phone server's 8794
LEASE_TTL_S = 120  # a lease nobody renews is gone after this
WATCH_S = 30  # the watchdog's period, which is also the renew period
IDLE_EVERY = 10  # with no lease, read tailscale every tenth tick (5 minutes)
PROBE_MISSES = 3  # consecutive probe failures that count as a dead funnel
FUNNEL_PORT = 443  # Funnel serves 443, 8443 and 10000; 443 is the one Meta reaches
ORIGIN_HOST = "127.0.0.1"

# Where the CLI lives. The Mac App Store build has no Funnel and no CLI, so
# only the standalone app and the Homebrew package are looked for.
BINS = [
    "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    "/opt/homebrew/bin/tailscale",
    "/usr/local/bin/tailscale",
]
# The standalone app's binary is the GUI unless this is set.
CLI_ENV = {"TAILSCALE_BE_CLI": "1"}

MODES = ("funnel", "off")

DOWNLOAD_URL = "https://tailscale.com/download/macos"
ADMIN_DNS_URL = "https://login.tailscale.com/admin/dns"
ADMIN_ACL_URL = "https://login.tailscale.com/admin/acls/file"
ADMIN_MACHINES_URL = "https://login.tailscale.com/admin/machines"

# The path WhatsApp posts to, appended to the public address.
WEBHOOK_PATH = "/whatsapp/webhook"

# The node attribute a tailnet policy file must grant before Funnel runs.
FUNNEL_CAP = "tailscale.com/cap/funnel"

# ── What is wrong, in words a human can act on ───────────────────────────────
NOT_INSTALLED = "no tailscale CLI: install the standalone app from tailscale.com/download"
NOT_RUNNING = "tailscaled is not answering: open the Tailscale app"
NOT_LOGGED_IN = "this Mac is not logged in to a tailnet: sign in from the Tailscale app"
STOPPED = "Tailscale is stopped on this Mac: connect it from the Tailscale app"
NEEDS_AUTH = "this Mac is waiting to be approved in the tailnet"
NO_HOSTNAME = "no .ts.net name yet: turn on MagicDNS and HTTPS certificates for the tailnet"
NOT_ALLOWED = "Funnel is not allowed in this tailnet: add the funnel node attribute to the policy file"
NO_HTTPS = "HTTPS certificates are off for this tailnet: turn them on in the admin console"
BAD_PORT = "Funnel only serves ports 443, 8443 and 10000"
NO_STATUS = "tailscale status did not answer"
FUNNEL_OFF = "the funnel is off: turn it on from the Tailscale Funnel tile"
PROBE_DEAD = "the funnel stopped answering its own probe"
NO_LEASE_PORT = "nothing has leased a port for the funnel to carry"


# ── Requests ─────────────────────────────────────────────────────────────────
def params(path):
    """Query parameters out of a path, so every lease endpoint also works as
    a plain GET."""
    query = urlsplit(path or "").query
    return dict(parse_qsl(query, keep_blank_values=True))


def form_decode(body):
    """A form body, `owner=x&mode=funnel`, or None."""
    if not isinstance(body, str) or "=" not in body:
        return None
    out = dict(parse_qsl(body.strip(), keep_blank_values=True))
    return out or None


def _number(value):
    if isinstance(value, bool) or value is None:
        return None
    try:
        n = float(value)
    except (TypeError, ValueError):
        return None
    if math.isnan(n) or math.isinf(n):
        return None
    return int(n) if n == int(n) else n


def request(body, query=None):
    """One lease request, however it arrived: a JSON body, a form body, or a
    query string. The body wins, because a caller that sent one meant it."""
    req = None
    if isinstance(body, str) and body.strip():
        try:
            req = json.loads(body)
        except ValueError:
            req = None
        if not isinstance(req, dict):
            req = form_decode(body)
    if not isinstance(req, dict):
        req = {}
    for k, v in (query or {}).items():
        if req.get(k) is None:
            req[k] = v
    return normalize(req)


def normalize(req):
    """Defaults, types, and a public URL with no trailing slash, because
    callers append paths to it."""
    req = req or {}
    ttl = _number(req.get("ttl_s"))
    out = {
        "owner": req.get("owner"),
        "mode": req.get("mode") or "funnel",
        "port": _number(req.get("port")),
        "public_url": req.get("public_url") or None,
        "ttl_s": ttl if ttl is not None and ttl > 0 else LEASE_TTL_S,
        "probe_path": req.get("probe_path") or None,
        "probe_expect": req.get("probe_expect"),
    }
    if isinstance(out["public_url"], str):
        out["public_url"] = out["public_url"].rstrip("/") or None
    return out


def bad_request(req):
    """Why a request can't be answered, as opposed to why a funnel is broken.
    A 400's worth of explanation, or None."""
    if not isinstance(req.get("owner"), str) or req["owner"] == "":
        return "owner is required"
    if req.get("mode") not in MODES:
        return "mode must be funnel or off (got %s)" % req.get("mode")
    if req["mode"] != "off" and not req.get("port"):
        return "port is required: the local port the funnel should carry traffic to"
    return None


def req_key(req):
    """What makes two lease requests the same job, so a renewal that changed
    nothing doesn't read tailscale again."""
    req = req or {}
    return "|".join(str(req.get(k)) for k in ("mode", "port", "public_url", "probe_path"))


# ── The commands ─────────────────────────────────────────────────────────────
def origin(port):
    return "http://%s:%d" % (ORIGIN_HOST, int(_number(port) or 0))


# Read-only, both of them, and the only two the watchdog runs.
def status_command():
    return ["status", "--json"]


def serve_command():
    return ["serve", "status", "--json"]


# The one command that publishes a port to the public internet. `--bg` writes
# the route into tailscaled, so it survives a reboot.
def on_command(port):
    return ["funnel", "--bg", "--https=%d" % FUNNEL_PORT, origin(port)]


def off_command():
    return ["funnel", "--https=%d" % FUNNEL_PORT, "off"]


def find_bin(exists, candidates=None):
    """The first candidate path that is there, or None."""
    for path in candidates or BINS:
        if exists(path):
            return path
    return None


# ── Reading tailscale ────────────────────────────────────────────────────────
def funnel_allowed(node):
    """Is the funnel node attribute granted to this machine? None means the
    client doesn't say (an older one prints neither field)."""
    if not isinstance(node, dict):
        return None
    caps = node.get("CapMap")
    listed = node.get("Capabilities")
    if not isinstance(caps, dict) and not isinstance(listed, list):
        return None
    names = list(caps.keys()) if isinstance(caps, dict) else []
    names += [c for c in listed if isinstance(c, str)] if isinstance(listed, list) else []
    return any(FUNNEL_CAP in name for name in names)


def parse_status(text):
    """`tailscale status --json`, reduced to the facts this sidecar needs, or
    None when it isn't JSON."""
    try:
        doc = json.loads(text) if isinstance(text, str) else None
    except ValueError:
        return None
    if not isinstance(doc, dict):
        return None
    node = doc.get("Self") if isinstance(doc.get("Self"), dict) else {}
    # DNSName comes back fully qualified, with the trailing dot.
    dns = node.get("DNSName")
    dns = dns.rstrip(".") if isinstance(dns, str) else None
    auth = doc.get("AuthURL")
    backend = doc.get("BackendState")
    return {
        "backend": backend if isinstance(backend, str) else None,
        "hostname": dns or None,
        "funnel": funnel_allowed(node),
        "online": node.get("Online") is True,
        # Set while a login waits to be finished in a browser.
        "auth_url": auth if isinstance(auth, str) and auth else None,
    }


def target_port(target):
    """The port behind a proxy target, http://127.0.0.1:8794 -> 8794."""
    if not isinstance(target, str):
        return None
    m = re.search(r":(\d+)/?$", target)
    return int(m.group(1)) if m else None


def serve_from_json(doc):
    """`tailscale serve status --json`. `on` means both halves are there: the
    funnel is open on a host:port AND something is proxied behind it."""
    allow = doc.get("AllowFunnel") if isinstance(doc.get("AllowFunnel"), dict) else {}
    opened = sorted(k for k, on in allow.items() if on)
    if not opened:
        return {"on": False}
    hostport = opened[0]
    m = re.match(r"^(.*?):(\d+)$", hostport)
    host, port = (m.group(1), int(m.group(2))) if m else (hostport, None)
    web = doc.get("Web") if isinstance(doc.get("Web"), dict) else {}
    entry = web.get(hostport) if isinstance(web.get(hostport), dict) else {}
    handlers = entry.get("Handlers") if isinstance(entry.get("Handlers"), dict) else {}
    proxy = None
    for path in ["/"] + sorted(handlers):
        handler = handlers.get(path)
        if isinstance(handler, dict) and handler.get("Proxy"):
            proxy = handler["Proxy"]
            break
    return {
        "on": proxy is not None,
        "host": host,
        "funnel_port": port,
        "target": proxy,
        "port": target_port(proxy),
    }


def serve_from_text(text):
    """The human-readable form, as a fallback:
        https://mac.tail1234.ts.net (Funnel on)
        |-- / proxy http://127.0.0.1:8794
    """
    target = re.search(r"proxy\s+(https?://[\w.\-]+:\d+)", text)
    host = re.search(r"https://([\w\-.]+\.ts\.net)", text)
    target = target.group(1) if target else None
    return {
        "on": "Funnel on" in text and target is not None,
        "host": host.group(1) if host else None,
        "funnel_port": FUNNEL_PORT,
        "target": target,
        "port": target_port(target),
    }


def parse_serve(text):
    if not isinstance(text, str) or not text.strip():
        return {"on": False}
    try:
        doc = json.loads(text)
    except ValueError:
        doc = None
    if isinstance(doc, dict):
        if doc.get("AllowFunnel") or doc.get("Web") or doc.get("TCP"):
            return serve_from_json(doc)
        return {"on": False}
    return serve_from_text(text)


# ── What the CLI's complaints mean ───────────────────────────────────────────
FATAL_LINES = [
    ("funnel is not enabled", NOT_ALLOWED),
    ("funnel node attribute", NOT_ALLOWED),
    ("nodeattrs", NOT_ALLOWED),
    ("tailscale.com/s/no-funnel", NOT_ALLOWED),
    ("failed to connect to local tailscaled", NOT_RUNNING),
    ("is tailscaled running", NOT_RUNNING),
    ("cannot connect to tailscaled", NOT_RUNNING),
    ("tailscale is stopped", STOPPED),
    ("logged out", NOT_LOGGED_IN),
    ("needslogin", NOT_LOGGED_IN),
    ("not logged in", NOT_LOGGED_IN),
    ("https is not enabled", NO_HTTPS),
    ("https must be enabled", NO_HTTPS),
    ("magicdns is not enabled", NO_HOSTNAME),
    ("only ports 443", BAD_PORT),
    ("invalid port", BAD_PORT),
]


def fatal_reason(text):
    if not isinstance(text, str):
        return None
    low = text.lower()
    for needle, reason in FATAL_LINES:
        if needle in low:
            return reason
    return None


# Why this Mac can't serve a funnel, from one `tailscale status --json`.
BACKEND_PROBLEMS = {
    "NoState": NOT_RUNNING,
    "NeedsLogin": NOT_LOGGED_IN,
    "NeedsMachineAuth": NEEDS_AUTH,
    "Stopped": STOPPED,
    # "Starting" gets the benefit of the doubt; the next tick asks again.
}


def status_problem(status):
    if not status:
        return NO_STATUS
    down = BACKEND_PROBLEMS.get(status.get("backend") or "")
    if down:
        return down
    if not status.get("hostname"):
        return NO_HOSTNAME
    if status.get("funnel") is False:
        return NOT_ALLOWED
    return None


def port_problem(serve, port):
    """A funnel that is up but carries somebody else's port is worse than no
    funnel. Only a problem while a funnel lease names a port."""
    serve = serve or {}
    if not serve.get("on") or not port or not serve.get("port"):
        return None
    if serve["port"] == port:
        return None
    return "the funnel carries %s, not port %d: turn it off and on again" % (
        serve.get("target") or "port %s" % serve["port"],
        port,
    )


def host_url(host):
    return "https://" + host if isinstance(host, str) and host else None


# ── Probing ──────────────────────────────────────────────────────────────────
def probe_failed(misses):
    return (misses or 0) >= PROBE_MISSES


def probe_ok(body, expect):
    """Did the origin answer with what its owner said proves it is the
    origin? The voice agent's /phone/ping returns a token chosen at start."""
    if expect is None or expect == "":
        return True
    if not isinstance(body, str):
        return False
    return body.strip() == expect


# ── Leases ───────────────────────────────────────────────────────────────────
# A lease is {"expires": t, "taken": seq, "req": normalized request}.
def lease_expiry(req, now):
    return now + (req.get("ttl_s") or LEASE_TTL_S)


def expire(leases, now):
    """Drop the leases nobody renewed and say whose they were. Nothing is
    torn down for them."""
    gone = sorted(o for o, lease in leases.items() if (lease.get("expires") or 0) <= now)
    for owner in gone:
        del leases[owner]
    return gone


def _live(lease, now):
    return now is None or (lease.get("expires") or 0) > now


def active_count(leases, now=None):
    return sum(1 for lease in (leases or {}).values() if _live(lease, now))


def active_list(leases, now=None):
    """The live leases, by owner name, with seconds left."""
    out = [
        {"owner": owner, "expires_in": max(0, math.floor((lease.get("expires") or 0) - (now or 0)))}
        for owner, lease in (leases or {}).items()
        if _live(lease, now)
    ]
    return sorted(out, key=lambda item: item["owner"])


def primary(leases, now=None):
    """(req, owner) of the lease that decides the port: the first one taken,
    for as long as it is renewed. (None, None) with no live lease."""
    live = [(lease.get("taken") or 0, owner, lease) for owner, lease in (leases or {}).items() if _live(lease, now)]
    if not live:
        return None, None
    _, owner, lease = min(live, key=lambda item: (item[0], item[1]))
    return lease.get("req"), owner


def conflict(live, req):
    """One funnel serves one origin. A second caller asking for another port
    would be handed a URL that reaches somebody else's service."""
    if not live or not live.get("port") or not req.get("port"):
        return None
    if live["port"] == req["port"] and (live.get("mode") or "funnel") == req.get("mode"):
        return None
    return "the funnel already serves %s mode on port %s; this request asked for %s mode on port %s" % (
        live.get("mode") or "funnel",
        live["port"],
        req.get("mode"),
        req["port"],
    )


# ── Status ───────────────────────────────────────────────────────────────────
# Four states:
#   off       nothing published, or the only lease asked for `off`
#   starting  a CLI call is in flight and the answer isn't in yet
#   ready     the funnel carries the leased port and there is a URL
#   error     a human has to do something; the next tick says so again
def compute_status(s):
    if s.get("fatal"):
        return "error"
    if s.get("mode") == "off":
        return "off"
    if s.get("funnel") and s.get("url"):
        return "ready"
    if s.get("pending"):
        return "starting"
    return "off"


def uptime(s, now):
    started = s.get("started_at")
    if started is None:
        return 0
    return max(0, math.floor(now - started))


# Every key, always, including the null ones: a caller reading
# state["error"] must not have to know the key can disappear.
STATE_KEYS = ["status", "url", "hostname", "generation", "error", "uptime_s", "mode", "port", "funnel", "leases"]


def snapshot(s, now):
    """The answer every lease endpoint gives, and the only shape a caller sees."""
    return {
        "status": compute_status(s),
        "url": s.get("url") or None,
        "hostname": s.get("hostname") or None,
        "generation": s.get("generation") or 0,
        "error": s.get("err"),
        "uptime_s": uptime(s, now),
        "mode": s.get("mode"),
        "port": s.get("port"),
        "funnel": s.get("funnel") is True,
        "leases": active_list(s.get("leases"), now),
    }


def encode_state(snap):
    return json.dumps({k: snap.get(k) for k in STATE_KEYS}, separators=(",", ":")) + "\n"


# ── Setting it up ────────────────────────────────────────────────────────────
# The checklist, in the order the prerequisites happen. Each step says who
# finishes it:
#   done    checked, and the sidecar can see it is
#   todo    the tile does this one; the step's action is a button
#   manual  the tile gets you to the page, you finish it
#   wait    blocked on a step above it
MARK = {"done": "✅", "todo": "⬜", "manual": "\U0001f464", "wait": "⏳"}

ACTION_LABEL = {
    "install": "Open the Tailscale download page",
    "openapp": "Open the Tailscale app",
    "signin": "Sign in from the Tailscale app",
    "check": "Check now",
    "admindns": "Open DNS settings",
    "adminmachines": "Open the machines page",
    "adminacl": "Copy the policy snippet and open the policy file",
    "copyenv": "Copy the voice agent's settings",
    "arm": "Turn the funnel on",
    "copyhook": "Copy the webhook URL",
}

# Actions the tile already has a button of its own for.
TILE_ACTIONS = ("arm", "check")

POLICY_SNIPPET = "\n".join(
    [
        '"nodeAttrs": [',
        "  {",
        '    "target": ["autogroup:member"],',
        '    "attr":   ["funnel"]',
        "  }",
        "],",
    ]
)


def webhook_url(hostname):
    base = host_url(hostname)
    return base + WEBHOOK_PATH if base else None


def env_lines(hostname):
    """The voice agent's settings, with this Mac's address already in them."""
    return (
        "\n".join(
            [
                "PHONE_TUNNEL=plugin",
                "PHONE_TUNNEL_PLUGIN=http://127.0.0.1:%d" % PORT,
                "PHONE_PUBLIC_URL=" + (host_url(hostname) or "https://<machine>.<tailnet>.ts.net"),
                "WHATSAPP_ENABLED=1",
            ]
        )
        + "\n"
    )


def setup_steps(env):
    """`env` is what the sidecar knows now: bin, status (parse_status),
    hostname, port, funnel, url."""
    env = env or {}
    st = env.get("status")
    has_bin = env.get("bin") is not None
    back = st.get("backend") if st else None
    signed_in = back == "Running"
    host = env.get("hostname") or (st.get("hostname") if st else None)
    steps = []

    install = {"key": "install", "label": "Tailscale installed"}
    if has_bin:
        install["state"] = "done"
    else:
        install.update(state="manual", action="install", detail="the tile opens tailscale.com/download; running the installer is yours")
    steps.append(install)

    signin = {"key": "signin", "label": "Signed in to a tailnet"}
    if not has_bin:
        signin.update(state="wait", detail="waiting for the client")
    elif signed_in:
        signin["state"] = "done"
    elif back == "NeedsMachineAuth":
        signin.update(state="manual", action="adminmachines", detail="a tailnet admin approves this Mac in the admin console")
    elif back == "Starting":
        signin.update(state="wait", detail="tailscaled is starting")
    elif back == "NoState":
        signin.update(state="todo", action="openapp", detail="tailscaled is not answering: the tile opens the Tailscale app")
    elif st is None:
        signin.update(state="wait", action="check", detail="nothing has been read from the CLI yet")
    elif back == "Stopped":
        signin.update(state="manual", action="openapp", detail="Tailscale is stopped: connect it from the Tailscale app")
    else:
        signin.update(
            state="manual",
            action="signin",
            detail=("a sign-in is half-finished: the tile reopens the page" if st.get("auth_url") else "sign in from the Tailscale app")
            + "; choosing an account and approving this Mac is yours",
        )
    steps.append(signin)

    dns = {"key": "dns", "label": "MagicDNS and HTTPS certificates on"}
    if not signed_in:
        dns.update(state="wait", detail="waiting for this Mac to be signed in")
    elif host:
        dns["state"] = "done"
    else:
        dns.update(state="manual", action="admindns", detail="turn both on in the admin console")
    steps.append(dns)

    cap = {"key": "funnel", "label": "Funnel allowed in this tailnet"}
    if not signed_in:
        cap.update(state="wait", detail="waiting for this Mac to be signed in")
    elif st.get("funnel") is True:
        cap["state"] = "done"
    elif st.get("funnel") is False:
        cap.update(state="manual", action="adminacl", detail="the tile copies the policy snippet and opens the policy file; pasting and saving it is yours")
    else:
        cap.update(state="wait", action="adminacl", detail="this client does not report it; turning the funnel on will say")
    steps.append(cap)

    lease = {"key": "lease", "label": "A local port to carry"}
    if env.get("port"):
        lease["state"] = "done"
    else:
        lease.update(state="manual", action="copyenv", detail="nothing has leased the funnel; start the voice agent")
    steps.append(lease)

    arm = {"key": "arm", "label": "The funnel is on"}
    if env.get("funnel") and env.get("url"):
        arm["state"] = "done"
    elif not (has_bin and signed_in and host and env.get("port")):
        arm.update(state="wait", detail="waiting for the steps above")
    else:
        arm.update(state="todo", action="arm", detail="one click, once: the route is written into tailscaled and survives a reboot")
    steps.append(arm)

    # Meta's dashboard is behind a login the sidecar doesn't have, so this
    # step is never ticked and never counted.
    meta = {"key": "meta", "label": "Meta's callback URL set", "checkable": False}
    if not host:
        meta.update(state="wait", detail="waiting for this Mac's .ts.net name")
    else:
        meta.update(state="manual", action="copyhook", detail="the tile copies %s; setting it in Meta's App Dashboard is yours" % webhook_url(host))
    steps.append(meta)
    return steps


def step_line(step):
    return MARK.get(step.get("state"), MARK["wait"]) + "  " + str(step.get("label") or "")


def setup_line(steps):
    """"Setup 3 of 6 done", counting only the steps the sidecar can check."""
    checkable = [s for s in steps or [] if s.get("checkable", True)]
    done = sum(1 for s in checkable if s.get("state") == "done")
    return "Setup %d of %d done" % (done, len(checkable))


def next_step(steps):
    """The first step the sidecar can check that isn't done."""
    for step in steps or []:
        if step.get("checkable", True) and step.get("state") != "done":
            return step
    return None


# ── What the tile shows ──────────────────────────────────────────────────────
LABEL = {"off": "Off", "starting": "Checking", "ready": "On", "error": "Error"}
NO_URL = "No public address yet"


def human_duration(seconds):
    seconds = max(0, math.floor(seconds or 0))
    if seconds < 60:
        return "%ds" % seconds
    if seconds < 3600:
        return "%dm %ds" % (seconds // 60, seconds % 60)
    return "%dh %dm" % (seconds // 3600, (seconds % 3600) // 60)


def status_line(s, now):
    """The state in words, with the one detail that matters in that state."""
    status = compute_status(s)
    head = LABEL.get(status, "Off")
    if status == "ready":
        return "%s · up %s · generation %d" % (head, human_duration(uptime(s, now)), s.get("generation") or 0)
    if status == "error":
        return head + ": " + (s.get("err") or "something went wrong")
    if status == "starting":
        return head + ": " + (s.get("err") or "asking tailscale")
    if s.get("mode") == "off" and s.get("url"):
        return head + ": the lease supplies " + s["url"]
    if active_count(s.get("leases"), now) > 0:
        return head + ": " + (s.get("err") or FUNNEL_OFF)
    return head + ": nothing has leased the funnel"


def url_line(s):
    if s.get("url"):
        return s["url"]
    host = host_url(s.get("hostname"))
    if host:
        return host + " (funnel off)"
    return NO_URL


def copyable(s):
    """What Copy URL puts on the clipboard: the live address, or the one this
    Mac will answer on once the funnel is armed."""
    return s.get("url") or host_url(s.get("hostname"))


def lease_line(leases):
    if not leases:
        return "No leases"
    return "Leased by " + ", ".join("%s (%s left)" % (l["owner"], human_duration(l["expires_in"])) for l in leases)
