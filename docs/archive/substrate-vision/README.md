# Archive: the substrate vision

**Superseded. Not maintained. Do not build on it.**

This was the second plan for CrowbarOS, from November 14 to November 17, 2026:
a lean base that each person shaped into their own OS with Tools their agent
built, under a deterministic security model. Jim Martin and Jason Alexander
replaced it with the [vision](../../VISION.md)
([ADR 0030](../../adr/0030-crowbaros-starts-again-with-an-agent-at-the-persons-side.md)).
Where the two disagree, the vision is right. What it taught us is in
[LESSONS.md](../../LESSONS.md).

Its code, including the Rust workspace in `crates/` and the adversarial lab, is
at the [`substrate-final`](https://github.com/crowbaros/crowbaros/tree/substrate-final)
tag. Links here to removed code point there.

| Path | What it is |
| --- | --- |
| [`VISION.md`](VISION.md) | The plan of record, with its roadmap and open questions |
| [`pitch.html`](pitch.html) | The one-page pitch |
| [`GLOSSARY.md`](GLOSSARY.md) | Its terms |
| [`threat-model.md`](threat-model.md) | What it defended against |
| [`adr/`](adr/) | ADRs 0001 to 0014, 0016 to 0026, and 0029. ADRs 0015, 0027, and 0028, on the compositor, still stand in [`docs/adr/`](../../adr/) |
| [`architecture/`](architecture/) | Design notes: the state and apply engine, the capability model, the gate and monitor |
| [`milestones/`](milestones/) | Milestone 1's record and evidence, and Milestone 2's scope |
| [`reviews/`](reviews/) | The five council review rounds |

Links inside these files were updated for this location. Their text is as it
was at the tag.
