# Principles, derived

Every rule in [AGENTS.md](../AGENTS.md) is derived here. If a rule seems wrong, argue with the derivation, not the rule:
each principle ends with what would falsify it.

Evidence tags used throughout: **[DERIVED]** follows from the math given the assumptions stated; **[MEASURED]** observed
on real hardware or in CI, with the method; **[ASSUMPTION]** believed but not yet measured (never report it as fact);
**[OWNER]** a decision by the project's owner, not derivable.

Notation: a *lamp* receives a stream of commands over a narrow radio link. The *core* decides what to send; a *shell*
does the I/O; *apps* produce signals (screen colour, music, schedules). Time is in milliseconds.

---

## P1. Bound the problem by its narrowest link

**Statement.** Reduce information at its source, before the narrowest link. Never ship more data than the lamp can use.

**Derivation.** Let X be a source signal (a screen), Y the command stream, Z the light emitted. They form a Markov chain
X → Y → Z, so by the data-processing inequality I(X;Z) ≤ I(X;Y) ≤ C·T, where C is the link capacity in bits per second.
Nothing downstream of a bottleneck can recover what the bottleneck discarded.

For the iStrip lamp one colour command carries 3 × 8 bits of colour and one of 101 brightness levels:
24 + log₂ 101 = **30.7 bits [DERIVED]**. The lamp followed updates at 20 and 40 per second and looked smooth, and looked
choppy at 2, 5 and 10 per second (owner's eyes, 2026-10-14) **[MEASURED]**. At 40 updates/s, C ≈ **1,226 bit/s**.

A 4K screen at 60 Hz is 3840 × 2160 × 24 × 60 = 1.19 × 10¹⁰ bit/s. The reduction needed before the link is
≈ **9.7 × 10⁶ ×** **[DERIVED]**.

**Consequences.**
- Screen and music apps reduce to a few numbers *where the data lives* (the machine that sees the screen) and send only
  those. The stream API takes `r g b [brightness]`, never pixels (`crates/lampbus/src/http.rs`, `stream_socket`).
- The core paces writes to at most one per 25 ms (`Config::min_write_interval_ms`), the rate the lamp is known to follow.
- Identical consecutive frames are never re-sent (P3).
- Protocols at the narrow waist stay tiny; a field that does not change a decision does not exist.

**Falsified by.** A lamp family whose link is not the bottleneck (e.g. a mains-powered Wi-Fi bulb), in which case P1
still applies to whatever *is* the narrowest link.

---

## P2. Send functions, not samples

**Statement.** Describe a change over time as a short program, and expand it next to the radio.

**Derivation.** An animation with K parameters sampled at N instants has description length ≈ K versus N·b for the
samples. For smooth signals N ≫ K. The 30-minute `sunrise` is **5 keyframes** that expand to 1800 s × 2 fps = **3,600
frames, a 720× reduction [DERIVED]**. Expanding at the receiver also makes the output depend on the core's clock alone,
so jitter and latency on the way in cannot distort it: sample(t) uses the t the core owns.

**Consequences.**
- Animations are declarative JSON ([spec/animation-format.md](../spec/animation-format.md)), evaluated by
  `lampbus-render`, driven by the core's ticks (`crates/lampbus-core/src/lamp.rs`, `tick`).
- A lamp's own built-in effects are the extreme case (zero traffic); the control language exposes them (`effect`).
- No animation contains code. Bounded description ⇒ bounded work (P8).

**Falsified by.** Signals with no compact description (live audio, a camera): those are streamed, which is why a stream
source exists (P5), and why it is smoothed and dithered rather than described.

---

## P3. Commands are absolute, so the system converges

**Statement.** Every command sets a value; none changes one relatively. Retries, repeats and re-sends are always safe.

**Derivation.** Model the lamp state as components (power, colour, level, speed, effect). Each command overwrites some
components with given values: a *last-writer-wins register* per component. Hence:
(a) c ∘ c = c (idempotent);
(b) the state after any delivered sequence depends only on the last write to each component, so if the final target is
re-sent after any loss, the lamp converges, without acknowledgements and without reading state back (the lamp's
write-without-response channel offers neither);
(c) relative commands ("brighter by 10") are *not* idempotent and need exactly-once delivery, which this channel cannot give.

The core keeps a belief B about the lamp (what it last wrote, or ⊥ = unknown) and sends only
frames(target, B) = { write_k : target_k ≠ B_k }, which is minimal given B and at most (number of components) frames.
On reconnect B := ⊥, so the diff is the full state. Forgetting is always safe precisely because of (a).

**Consequences.**
- `crates/lampbus-core/src/lower.rs`: `Sent` is B; `lower` computes the diff; `Sent::default()` is ⊥, set on every link
  change (`lamp.rs`, `link_up`, `link_down`).
- The pending queue is replaced wholesale by each re-render (newest wins); stale frames never go out
  (`only_the_newest_command_is_sent_after_a_slow_connect`).
- Raw protocol frames are the one exception and sit in a separate queue that a re-render cannot discard.

**Falsified by.** A lamp whose protocol has only relative or toggle commands. Then the definition must model an
explicit state-query, or the lamp cannot be driven reliably, and the definition's `status` must say so.

---

## P4. The core is a pure function of its inputs

**Statement.** The core does no I/O and reads no clock, randomness or thread state. A shell supplies everything.

**Derivation.** Model the core as a Mealy machine M = (S, I, A, δ, λ) with δ: S × I → S and λ: S × I → A\*, where the
input alphabet I contains commands, link events *and the current time* as a plain number. With no hidden input, the output
sequence is a function of the input sequence alone. Consequences: any failure is reproducible by replaying its inputs;
time can be virtual, so a 30-minute sunrise is tested in microseconds; and all concurrency, retries of the radio, and
platform differences live in thin shells.

**Consequences.**
- `lampbus-defs`, `lampbus-render` and `lampbus-core` do not use `std::time`, randomness, threads, files or sockets
  **[MEASURED: grep, 2026-10-14]**, and none depends on tokio or btleplug **[MEASURED: `cargo tree`]**. Keep it so.
- The simulation harness (`crates/lampbus-core/tests/sim.rs`) drives the core with a virtual clock and checks predicates
  over every action trace: **(I1)** never write while disconnected; **(I2)** never connect while connected or
  connecting; **(I3)** writes to one lamp are at least 25 ms apart. A bug that breaks an invariant fails the suite
  wherever it occurs.
- The same core can be hosted by a desktop shell today and a phone, browser or microcontroller shell later.

**Falsified by.** A requirement the core cannot meet without reading the world (it should then ask the shell via an
action and receive the answer as an event, never reach out itself).

---

## P5. Priority is a total order with exact fallback

**Statement.** When several sources want the lamp, the highest-priority *active* one wins, and when it stops the lamp
returns to exactly what the next one would have shown alone.

**Derivation.** Sources are ordered stream (3) > animation (2) > built-in effect (1) > static colour (0), with power-off
dominating all. A stream is *active* at time t iff t − t_last < T, T = 2 s. Output(t) = the value of the active source of
maximal priority. Two properties follow and are required: **liveness** (a crashed live source cannot leave the lamp stuck:
after T seconds it is inactive), and **no residue** (leaving a source resets its filter state, so the next output depends
only on the lower source and the clock).

**Consequences.** `lamp.rs`, `rerender` is the selection chain; `Smoother` and `last_smooth` reset when a stream expires;
`a_live_stream_overrides_the_look_and_falls_back_when_it_stops` and `power_off_pauses_an_animation_and_power_on_restarts_it`
pin it. A finished animation that *holds* keeps its final intensity, not full brightness
(`holding_the_last_frame_of_an_animation_keeps_its_brightness`: a bug the simulator found).

**Falsified by.** A use case needing blending of sources (cross-fades). That is a new operator on the order, not a
violation: state it explicitly before adding it.

---

## P6. Respect perception and quantisation

**Statement.** Mix in linear light, resolve fine levels in time, and filter live signals asymmetrically. Do not guess a
lamp's response curve.

**Derivation.**
*Perception.* Just-noticeable differences are roughly proportional to intensity (Weber), so equal *ratios* look equal,
not equal differences. A lamp with n linear steps changes by 1/n between step n−1 and n: at level 1 of 100 the next
step is a 100 % jump, which is visible banding. CIE lightness maps perceived to physical: Y = ((L\*+16)/116)³ for L\* > 8,
else L\*/903.3, so L\* = 50 is Y = 0.184 (tested: `lightness_to_luminance`).

*Dithering.* Let q_t = round(v_t + e_{t−1}) and e_t = v_t + e_{t−1} − q_t. Unclipped, |e_t| ≤ ½ by induction, and
Σ q_t = Σ v_t + e_0 − e_N, so **|mean(q) − mean(v)| ≤ 1/N [DERIVED]**: over a window of N frames the effective step
shrinks from Δ to Δ/N (about 20× finer over one second at 20 fps). The error is clamped to [−1, 1] so a saturated channel
cannot wind up. This only helps if frames arrive faster than the eye resolves flicker, so it is applied to animations and
streams and *never* to static colours. **[MEASURED 2026-10-15, owner's lamp: the assumption fails at the dim end.]** Alternating levels 0 and 1 at 20 updates/s is a 100 % modulation and flickers visibly, so dithering is **off by default** (`dither` in the config) and should only be enabled where one step is a small fraction of the level.

*Smoothing.* y ← y + α(x − y), α = 1 − e^(−Δt/τ), exact for irregular Δt, with τ = 30 ms when x rises and 150 ms when it
falls (defaults): quick to respond, slow to flicker.

**Consequences.** `crates/lampbus-render/src/pipeline.rs` (`Dither`, `Smoother`, `Calibration`, `lightness_to_luminance`),
`color.rs` (`mix` in linear light by default), core config `color_gamma` and `level_curve`. Both default to *identity*,
because the lamp's response is unmeasured **[ASSUMPTION]**: calibrate by eye, record the result in the scorecard, then change the default.

**Falsified by.** Measuring the lamp's real response; or a user study showing dithering visible as flicker (then lower
the animation rate or disable `dither`, which is a config switch).

---

## P7. Lamps are data, and every datum is checkable without the lamp

**Statement.** A lamp is described by a definition plus test vectors from an *independent* source. Hardware-verified is a
status earned by running on hardware.

**Derivation.** Differential testing finds a bug when two implementations disagree, which requires their errors to be
independent. An oracle that shares code with the interpreter is correlated with it and tests nothing. So vectors come
from separate re-implementations of each upstream project's frame construction (`tools/oracles`, which does not depend on
`lampbus-defs`), and the suite must demonstrate *power*: it fails when a fault is seeded. Done 2026-10-14: breaking a
checksum kind, one byte, and the AES step each failed the suite (3 of 3) **[MEASURED]**.

**Consequences.** `definitions/*.json` + `*.vectors.json`; CI runs `cargo run -p lampbus-oracles -- --check`; status is
`hardware-verified` (run on a real lamp by a maintainer), `source-verified` (matches independent open implementations),
or `unverified`. Only the iStrip definition is hardware-verified today. Roles fix parameter names so the core can use any
lamp (`Definition::caps()`).

**Falsified by.** A protocol the template language cannot express. Then extend the language (spec change, new vectors)
or provide a compiled driver; do not special-case it in the core.

---

## P8. Fail closed at every boundary

**Statement.** Reject what is unknown or ambiguous with a message that makes the next attempt correct. Execute nothing a
stranger or an agent supplies.

**Derivation.** Let L be the set of valid control objects. A mistyped key lies outside L, so rejecting unknown keys maps
"typo" to *error* instead of to a different valid command. "At most one of colour/effect/animation/preset" removes
ambiguous composites. Hard limits (animation ≤ 86,400 s, ≤ 500 keys, ≤ 40 fps, frame ≤ 64 bytes, name ≤ 40 characters)
bound worst-case work per input. Numeric ranges are *clamped* (a deliberate, documented leniency); types are not coerced.
Error messages list the valid alternatives, so an agent converges in one retry.

**Consequences.** `deny_unknown_fields` on definitions and config; `KEYS` in `crates/lampbus/src/control.rs`;
`lampbus_render::anim::load`; `unsafe` is forbidden and `unwrap`/`expect`/`panic`/unchecked indexing are denied in
non-test code (workspace lints); no animation, definition or control object is ever evaluated as code.

**Falsified by.** A real workflow where strictness blocks legitimate use. Relax by adding an explicit field, not by
guessing.

---

## P9. Claims are measurements

**Statement.** A claim is (statement, method, value, date, machine). Without a method it is an assumption, and
assumptions are labelled.

**Derivation.** A claim that cannot be falsified carries no information. "Small" and "fast" are only meaningful against a
number and a procedure that anyone can repeat; "met" means CI reproduces it. So [SCORECARD.md](SCORECARD.md) lists each
criterion with its target, method and today's value, including the ones we miss: the binary is **1.10 MB** against a
500 KB target, and says so.

**Consequences.** Measure before claiming; record the command that produced a number; tag evidence as above; update the
scorecard in the same commit that changes a measured quantity; never write a number from memory.

**Falsified by.** Nothing: this is the rule for deciding what counts as evidence.

---

## P10. Reuse before build; own what you ship

**Statement.** Build only what the best existing option fails to do on a stated criterion. Prefer owning or forking to
waiting on others. Ship protocol facts, never vendor artefacts.

**Derivation.** For each option o, total cost ≈ integration(o) + footprint(o) + p_unresponsive(o) · delay(o). Depending on
a maintainer who may not reply adds the last term, which can dominate; forking or owning removes it. Build wins only when
the best existing option fails a scorecard criterion by more than the cost of building. This is why Bluetooth uses
`btleplug`, Matter is planned on `rs-matter`, screen/music apps are planned as adapters to existing tools via DDP, and a
native HomeKit crate was dropped (D5). The owner has no ego in this: if something already does it better, use it.

**Consequences.** [docs/research](research/landscape-2026-10-14.md) is dated and states its method and limits. Decompiled
apps and firmware never enter the repository (D9). Upstream pull requests are not planned around unless the project is
actively maintained **[OWNER]**.

**Falsified by.** An existing project that beats the scorecard: adopt it and delete our code.

---

## Open assumptions (do not report these as facts)

| # | Assumption | How to settle it |
|---|---|---|
| A1 | iStrip brightness is 0-100. Another project documents 0-64. | **Settled 2026-10-15:** 100% is visibly brighter than 64%, so the range is 0-100. Also measured at 50%: a 1-level step is invisible and 2 levels or more are visible. |
| A2 | The lamp's response is close to linear duty, so `color_gamma` = 1. | Open. A linear 20 s fade looked "discrete" with visible steps throughout; a perceptual curve (B) looked slightly less stepped at the top but flickered at the bottom. Not resolved; fast fades are a stress case, not real use. |
| A3 | Flicker fusion is fast enough that dithering is invisible at 20 updates/s. | **Falsified 2026-10-15** at the dim end (see P6). Dithering is off by default. |
| A4 | Some iStrip firmware needs a one-time "join group" command; the owner's lamp did not. | Test on another unit, or ask a user whose lamp ignores commands. |
| A5 | The 2-second stream timeout suits screen sync. | Measure with a real screen-sync app. |
