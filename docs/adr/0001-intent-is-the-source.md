# Intents, checks, and the pinned build are the source; the Crowbar State is the lockfile

The durable thing a person owns is what they asked for. But words and a few examples do not capture everything a person comes to rely on: the first build makes many unstated choices. So the source of a person's OS is three things together: the **intent** (their words), its **checks**, and the **pinned build** they accepted. The **Crowbar State** is the lockfile that pins all three, and **generations** are builds of the State.

A **check** is a scenario in a fixed data format: the events the broker delivers at stated simulated times, and the effects the Tool must and must not ask the broker for. The person confirms it in plain language that fixed templates produce from the scenario. No model writes or translates that wording. A scenario with no expected effect is rejected. A check that needs logic the format cannot express may be written as code; wherever it is shown, it is marked "wording not verified". A confirmed check is frozen in the State: changing it is an intent edit, and a re-derive can never do it.

Checks run in **rehearsal**, against simulated time, simulated events, and recorded inputs. A **runner** that is part of Base runs them, not the task that built the Tool. The runner signs the result together with the content hash of the build and of every check, and a promote accepts only a build whose hash carries a passing result. What passed is what goes live.

Compiling happens only when an intent changes or the person asks to re-derive. A re-derive is accepted only when every frozen check passes and a replay of the Tool's recorded inputs gives the same results, apart from differences the person accepts. The runner does the replay, and the build task learns only whether it passed (ADR 0011).

Revised after council rounds 1, 2, and 3. The first version made intents alone the source, with plain-language checks. The second compiled each check into test code and generated the wording from the test.

## Considered Options

- **The State is the truth, prompts are ledger metadata.** Rejected: the OS can't be re-derived with a better model or on different hardware.
- **Intents only, regenerated every time.** Rejected: not reproducible; the system would drift with every model change.
- **Intents with plain-language checks as the whole source (the first decision).** Rejected: something must turn a plain-language check into a test, and that something is a model, so a model would judge its own re-derive. A few examples also do not pin behavior.
- **Checks compiled into test code, with wording generated from the code (the second decision).** Rejected as the default: a model stands between the code and the wording, so a test that always passes can carry an honest-looking description. Nothing tied a passing result to the build that was promoted.

## Consequences

- The OS must provide rehearsal: every interface the broker offers a Tool needs a simulated form and a record-and-replay form.
- The scenario format limits what a check can say. That is the price of wording the person can trust.
- Code is disposable only after a replay shows the new build behaves the same. Until then the pinned build is kept.
- Writing an intent is a design conversation: the agent asks questions and proposes checks before it builds.
