# The living lamp

`lampbus-life` (in `apps/life`) is an ambient creature that drives a lamp. It is an **app**: it uses only the lampbus
UDP input and the control language, and the core knows nothing about it.

## Three loops, three speeds

```
 EGO     an agent (Claude, Hermes), you         seconds to hours    sets goals and mood, never frames
   |  "calm for an hour", "campfire", "something good happened"
 LIFE    lampbus-life                           20 times a second   breathing, drift, stirring, habituation
   |  colour + level frames over UDP
 REFLEX  lampbus core + the lamp                milliseconds        pacing, belief, reconnection, safety
```

An agent answers in seconds and costs money per thought, so it cannot drive a smooth 20 Hz trajectory and must not be
the thing that keeps the lamp going. It sets **attractors** (a mood, a palette, an energy level, limits); the creature
produces the **trajectories** toward them. If the agent stops, the lamp keeps living.

## What it does

- A **baseline** that follows the time of day (deep ember at night, warm gold in the morning, amber in the evening), or
  a fixed colour for a mood.
- **Breathing** with a period that wanders, **drift** in the colour, and **stirring**: an unprompted brightening every
  minute or two that settles again.
- A fast small **flicker** for fire-like moods, band-limited to 2.5 Hz.
- **Pokes**: `good`, `info`, `warn`, `bad` pulse a colour. Repeated pokes within a minute count for `0.6^n`
  (habituation), so a lamp that is poked constantly does not nag.
- **Moods** are sets of parameters. The creature never jumps to a new one: every parameter eases toward its target
  with a 6 s time constant, so a change takes about fifteen seconds and always looks continuous.

Safety rules the tests enforce: nothing modulates faster than 3 Hz in any mood, and the brightness ceiling (`--max`,
default 40%) is never exceeded.

## Using it

```console
$ lampbus-life --mood ember            # run it (or install it as a service: deploy/install.sh --life)
$ lampbus-life mood campfire           # ease into the campfire
$ lampbus-life set flicker 0.4         # change one number
$ lampbus-life poke good               # react to something
$ lampbus-life sleep | wake            # stop or resume driving the lamp
$ lampbus-life status
```

Moods: `ember` (follows the day), `campfire`, `calm`, `focus`, `ocean`, `night`. The commands are plain text
datagrams to `127.0.0.1:8790`, so anything can steer it: `echo 'mood calm' | nc -u -w0 127.0.0.1 8790`.

It needs `"udp_stream": "127.0.0.1:8789"` in the lampbus config. Also set `"stream_yield_seconds": 3600` so a person
always wins: when someone sends a colour, an effect or an animation (from the CLI, Home or Siri), live streams are
ignored for that long, and **turning the lamp on** hands it back to the creature. As a service (`--no-setup`) it never
turns the lamp on by itself and honours your master brightness: it takes over whenever the lamp is on.

`--fine` moves the brightness into the colour channels (finer steps, smoother dim light); `--fps 30` updates faster.

## Letting an agent show what it is doing

`tools/agent-presence.sh working|done|needs-you|error` maps an agent's state onto the creature. With Claude Code, hooks
can call it (check your version's hook documentation for the exact settings format):

```json
{
  "hooks": {
    "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "tools/agent-presence.sh working"}]}],
    "Stop":             [{"hooks": [{"type": "command", "command": "tools/agent-presence.sh done"}]}],
    "Notification":     [{"hooks": [{"type": "command", "command": "tools/agent-presence.sh needs-you"}]}]
  }
}
```

## What this is and is not

A small closed-loop dynamical system with internal state, a baseline it maintains, memory (habituation), spontaneous
activity and responses to the world. That is what makes something read as alive. It is not a claim of anything more,
and it has no goals of its own beyond staying pleasant.
