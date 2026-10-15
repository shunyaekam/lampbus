# Add your lamp

A lamp is one JSON file and a file of test vectors: no code, no build. This takes about an hour for a lamp whose
protocol is already known, and an afternoon for one that is not. Humans and agents can both do every step.

## 1. See what the lamp exposes

Close the vendor app (a lamp takes **one** Bluetooth connection) and stop `lampbus serve`. Then:

```console
$ lampbus gatt                       # list the named devices nearby
$ lampbus gatt MyLamp                # connect and print every service, characteristic, property and readable value
```

`gatt` only reads. Write down the advertised name (becomes `match.name_prefix`), the service that carries commands and
the characteristic that is **write** or **write-without-response** (becomes `gatt`). A characteristic with **notify**
is where the lamp answers, if it ever does. If a lamp does not show up in the list, it may still be connected to
something else (the lamp in this project stops advertising while connected).

## 2. Find the protocol

In order of effort:

1. **Someone already did it.** Search GitHub for the lamp's name or its vendor app. Many lamps share a controller
   (ELK-BLEDOM, Triones, Govee H61xx are already here). See `docs/research/`.
2. **Read the vendor app.** Download the APK, open it in `jadx`, and look for the code that builds the byte array
   written to Bluetooth (search for `writeCharacteristic`, `0x7e`, `AES`, `checksum`). This is how the iStrip lamp
   was done; the result is in [protocols/istrip.md](protocols/istrip.md).
3. **Sniff it.** Android: Developer options, "Enable Bluetooth HCI snoop log", use the app, open the log in Wireshark
   and look at ATT writes to your characteristic. Change one thing in the app at a time (one colour, one level).

Typical shapes: a fixed header, a command byte, arguments, a checksum or padding, sometimes AES-128-ECB or an XOR
over the frame. All of these are expressible; see the [format](../spec/definition-format.md).

## 3. Write the definition

Copy the nearest existing file in [`definitions/`](../definitions) (`elk-bledom.json` is the smallest) and edit:

```json
{
  "schema": "lampbus.device/0",
  "id": "my-lamp",
  "name": "My lamp",
  "status": "unverified",
  "match": { "name_prefix": ["MyLamp"] },
  "gatt": { "service": "0000fff0-0000-1000-8000-00805f9b34fb",
            "write": "0000fff3-0000-1000-8000-00805f9b34fb", "mode": "without-response" },
  "roles": { "on": "power_on", "off": "power_off", "color_rgb": "color", "brightness": "brightness" },
  "commands": {
    "power_on":  { "frame": ["0x7e", 0, 4, "0xf0", 0, 1, "0xff", 0, "0xef"] },
    "power_off": { "frame": ["0x7e", 0, 4, 0, 0, 0, "0xff", 0, "0xef"] },
    "color":      { "params": {"r": {}, "g": {}, "b": {}}, "frame": ["0x7e", 0, 5, 3, "$r", "$g", "$b", 0, "0xef"] },
    "brightness": { "params": {"level": {"min": 0, "max": 100}}, "frame": ["0x7e", 0, 1, "$level", 0, 0, 0, 0, "0xef"] }
  },
  "sources": ["where you learned the protocol: a repository, a file in the app, a capture"]
}
```

`roles` tell lampbus which commands mean "colour", "brightness", "on", "off" and "effect", so the rest of the system
(Siri, Home Assistant, animations, screen sync) works with your lamp automatically. Commands without a role are still
reachable as `lampbus raw <command> name=value`. Put the file in your `definitions_dir`
(`~/.config/lampbus/definitions` by default) and it is picked up on the next start.

## 4. Check it

```console
$ lampbus check my-lamp.json my-lamp.vectors.json
my-lamp.json: valid. My lamp (my-lamp), 4 commands, 0 effects, status Unverified
12 vectors reproduce exactly.
```

The **vectors** file lists the exact bytes each command must produce. They must come from somewhere that is **not
your own definition**: the upstream project's frame builder, a captured frame from the vendor app, or the app's source
code. A vector you generated from the definition you are testing proves nothing. The easiest honest source is a
capture: "I set the app to red at 50% and the bytes were these". [`tools/oracles`](../tools/oracles) shows how the
existing vectors are produced from independent re-implementations.

## 5. Try it on the lamp

```console
$ lampbus scan                                  # your lamp should appear, matched to my-lamp
$ lampbus color red 50
$ lampbus raw power_off
```

Test each command and each role, and write down what you saw. Change `status` to `hardware-verified` only when you
ran it on the real lamp; `source-verified` when the bytes match independent sources but you have no lamp.
Things worth testing that the app may hide: whether a command is ignored in some state, what the lamp does when it
turns itself on, whether it replies on a notify characteristic (`lampbus gatt` shows announcements).

## 6. Contribute it

Open a pull request with `definitions/<id>.json`, `definitions/<id>.vectors.json`, and a short
`docs/protocols/<id>.md` saying what is verified and what is not. CI checks the vectors on Linux and macOS. If you
cannot do all of it, open an issue with the `gatt` output and any captures: someone else can finish it.
