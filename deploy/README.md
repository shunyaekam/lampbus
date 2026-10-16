# Running lampbus on an always-on machine

The daemon (`lampbus serve`) is what you want on a machine that stays on: it keeps the Bluetooth link, runs
animations and schedules, accepts live colour input, and serves the web UI. The machine must be within Bluetooth range
of the lamp (about 5-10 m).

## 1. Get the binary

Either build it on the machine (`cargo build --release -p lampbus`, needs Rust 1.80+; on Linux also `libdbus-1-dev`,
`libasound2-dev`, `pkg-config`), or download the artefact for your platform from a tagged GitHub release
(built by `.github/workflows/release.yml`). On Linux the binary needs `libdbus-1` at runtime, which any BlueZ system has.

## 2. Install as a service

```console
$ ./deploy/install.sh path/to/lampbus     # systemd (Linux) or launchd (macOS), as your user
```

## 3. Configure (optional)

`~/.config/lampbus/config.json`, for example:

```json
{
  "devices": [{"alias": "desk", "name": "SSL-E089ED"}],
  "bind": "127.0.0.1:8787",
  "udp_stream": "127.0.0.1:8789",
  "schedules": [
    {"name": "wake",  "at": "07:00", "days": ["mon","tue","wed","thu","fri"], "control": {"animation": "sunrise"}},
    {"name": "sleep", "at": "23:00", "control": {"animation": "sunset"}}
  ]
}
```

## 4. Reach it from your other devices

Bind to your Tailscale address instead of localhost, for example `"bind": "100.x.y.z:8787"`, then open
`http://100.x.y.z:8787/?token=...` on your phone (the token is in `~/.config/lampbus/token`) and add the page to the
home screen. The API is plain HTTP with a bearer token and relies on the private network for encryption: **do not bind it
to a public address.** The UDP inputs are unauthenticated; keep them on localhost or the tailnet.

Point apps such as [living-lamp](https://github.com/shunyaekam/living-lamp) at the same machine: `lampbus-screen --to 100.x.y.z:8789`.

## 5. Apple Home, Google Home, Alexa, Home Assistant (Matter)

Run the adapter next to the daemon: `lampbus-matter` (add it to the service the same way). In the Home app choose
Add Accessory > More options, and enter the setup code it prints (`3497-011-2332` for the built-in test identity).
Apple Home warns that the accessory is uncertified; that is expected. Pairings are kept in `~/.config/lampbus/matter`.
You need a home hub (Apple TV or HomePod) for remote access and automations.

## Checking it works

```console
$ lampbus status                  # talks to the running daemon automatically
$ systemctl --user status lampbus # or: tail -f ~/Library/Logs/lampbus.log
```
