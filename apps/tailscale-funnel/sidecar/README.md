# tailscale-funnel

One public address for this Mac, `https://<machine>.<tailnet>.ts.net`, leased on `127.0.0.1:8795` to whoever needs it. The voice agent (`voice_agent/phone/lease.py`) takes a lease for its phone server on 8794 and renews it every 30 s. A port of `apps/tailscale_funnel` from the Hammerspoon setup; the tailscale-funnel plugin (`../plugin`) draws the hub widget.

The sidecar watches Tailscale with two read-only commands, `tailscale status --json` and `tailscale serve status --json` (with `TAILSCALE_BE_CLI=1`): every 30 s while a lease is live, every 5 minutes otherwise. It never arms the funnel by itself. Only `POST /arm` does, and only the tile's button calls it. It never turns the funnel off when the last lease goes, because the address outliving the voice agent is the point.

| Endpoint | Body or query | Answer |
|---|---|---|
| `GET /health` | | `{"ready": true}` |
| `POST /lease` | `{"owner","mode":"funnel"\|"off","port","ttl_s","probe_path","probe_expect","public_url"}` | the state; 400 bad request, 409 another owner holds another port, 503 a human has to act |
| `GET /funnel`, `GET /tunnel` | | the state, without touching a lease |
| `POST /release` | `{"owner"}` | the state |
| `POST /check` | | reads Tailscale now; the state, or 409 while a read is in flight |
| `POST /arm` | | runs `tailscale funnel --bg --https=443 http://127.0.0.1:<leased port>`; 409 with no lease, 502 when Tailscale refuses |
| `POST /disarm` | | runs `tailscale funnel --https=443 off` |
| `GET /view` | | the state plus what the tile shows: `status_line`, `url_line`, `lease_line`, `setup_line`, `next_step`, `steps`, `webhook_url`, `env_lines`, `policy_snippet` |

`/lease`, `/funnel`, `/tunnel`, `/release` and `/check` also take a GET with query parameters (`/lease?owner=x&port=8794`), and a JSON or form body. `/arm` and `/disarm` are POST only.

The state always has every key:

```json
{"status":"ready","url":"https://mac.tail1234.ts.net","hostname":"mac.tail1234.ts.net","generation":1,
 "error":null,"uptime_s":182,"mode":"funnel","port":8794,"funnel":true,"leases":[{"owner":"voice-agent","expires_in":95}]}
```

`status` is `off`, `starting`, `ready` or `error`. `url` stays null until the funnel carries the leased port while Tailscale runs; `hostname` is there as soon as the Mac is logged in. A lease expires 120 s after the last `/lease`. With a `probe_path`, the sidecar fetches `url + probe_path` each round, and three misses in a row turn the status to `error`. An answer other than `probe_expect` counts as a miss.

`funnel.py` holds the pure logic (parsing, leases, status, setup steps); `server.py` holds the CLI calls, the watchdog and the HTTP intake. `SIDECAR_PORT` moves the port.

```bash
python3 -m unittest      # no tailscale runs: the CLI, clock and probe are fakes
bricolo sidecar add tailscale-funnel --command /opt/homebrew/bin/python3 --port 8795 -- /path/to/bricolo/apps/tailscale-funnel/sidecar/server.py
curl -s 127.0.0.1:8795/funnel
curl -s -X POST 127.0.0.1:8795/lease -d '{"owner":"test","mode":"funnel","port":8794}'
curl -s -X POST 127.0.0.1:8795/release -d '{"owner":"test"}'
```
