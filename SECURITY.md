# Security

lampbus runs on a machine in your home and talks to Bluetooth lamps. Its exposed surfaces are:

- the HTTP/WebSocket API (`lampbus serve`), bound to `127.0.0.1` by default and protected by a bearer token kept in
  `~/.config/lampbus/token` (mode 0600). It is plain HTTP: reach it from other devices over a private network such as
  Tailscale, never the public internet;
- the Matter adapter, which uses a **test vendor identity** and a fixed setup code (documented in the install guide). Anyone on
  your LAN who knows the code can pair with it while it is in commissioning mode (only before the first pairing);
- MCP over stdio, which has the privileges of the process that starts it.

Lamp protocols are unauthenticated by design (the iStrip key is shared by every lamp); anyone within Bluetooth range
can control a lamp that is not connected to something else.

Report vulnerabilities privately through GitHub's "Report a vulnerability" on this repository's Security tab. Please
do not open a public issue for them. Expect a first reply within a week.
