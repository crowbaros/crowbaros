# The origin table fails closed, and labels and marks only rise

**Status:** accepted on November 15, 2026, by Jim Martin, in the design questions for [issue 41](https://github.com/crowbaros/crowbaros/issues/41).

[ADR 0008](0008-private-values-stay-out-of-the-agent-context.md) and [ADR 0011](0011-task-principals.md) say that a label comes from where the data came from, that open input marks, and that nothing a model does removes a label or a mark. They do not say what an origin no rule covers gets, how fine a mark's source is, or what happens when a compartment whose label was fixed at start is offered more. The origin table in `crates/crowbar-labels` needs an answer to each, and its stored records keep that answer. This record gives the answers.

## The origin table

- **An origin no rule covers fails closed.** It is private, in the `unknown` category, and marked. No party is cleared for `unknown` unless the person names it. The same applies to a file that came from outside the fleet by a way the table does not know, such as removable media, to a paste whose source was not recorded, and to a stored object with no record.
- **A paste carries its source's label and mark, and nothing more**, as ADR 0008 says. A paste of the person's own note stays personal and unmarked. The glossary and the capability model no longer list a paste as open input.
- **A mark keeps its sources.** ADR 0011 lets a marked task act under a standing rule the person wrote for that source, so the gate has to know the source when it checks the act. A site is its host. Mail is the account it arrived in, because a sender address can be forged.
- **A page fetched under a login is private to that site, and nothing else.** The site is its only category. A bank's pages are not also finance.
- **The categories are messages, health, finance, a site, and `unknown`.** Credentials get no category of their own for now: their values are in the vault.
- **Only the two sets ADR 0011 names are admitted:** the Base package snapshot and the dependency set for build tasks. A catalog snapshot ([ADR 0017](0017-a-pinned-catalog-and-workbenches.md)) is not admitted until a record decides it is.
- **Voice is not a row yet.** ADR 0014 marks it until the confirm gesture. Its label is decided when voice input is built.

## Labels and marks only rise

- **A compartment whose label was fixed at start refuses more.** A native sandbox is fixed at the join of its view, and a workbench is also always marked. A value above that, offered later, is refused with a refusal that names the rule. The compartment does not rise, and it is not restarted.
- **A write joins with what is stored.** A writer that never read the old object still cannot clear its mark. An object with no record may have been on disk before records were kept, so a write to it joins with the unknown row. Only a new object starts at the label and mark of what wrote it, and only the broker, which knows the object is new, records it so. A Tool re-derived from its intent and checks writes its source as a new object, and the old source stays marked in history.
- **A stored record names both its label and its mark.** Loading refuses a record that is missing either, that names a field, category, or source the code does not know, or that names one object twice. A missing field is never read as public or unmarked.

## Considered Options

- **An unknown origin is personal and marked.** Rejected: a private value with an unknown origin could then reach a model provider cleared for personal.
- **An unknown origin cannot be read until the person gives it a label.** Rejected: a task would stop at the first file with no record, even where reading it as private and marked is safe.
- **Every paste is open input and marked.** Rejected: it contradicts ADR 0008, and every task that read a paste of the person's own words would be marked.
- **A mark that says only marked or not.** Rejected: ADR 0011's rule for a source cannot be checked without the source, and stored records could not gain it later.
- **Mail marked by its authenticated sender.** Rejected for now: the account is what the broker knows for certain. A finer source can be added later, because a mark only gains sources.
- **A logged-in site also carries a category the person or an adapter sets.** Rejected for now: ADR 0008 says private to that site, and a second category is a policy no record has made.
- **A fixed compartment rises from then on.** Rejected: the gate cannot see a native process write inside its view, so a live write made earlier would keep the lower label.
- **A fixed compartment is restarted at the higher label.** Rejected for now: it stops the person's work in a running app to deliver one value.
- **A full replace by an unmarked writer resets the record.** Rejected: the broker cannot tell a blind replace from a read and a write.

## Consequences

- Files already on disk have no record, so all of them start private and marked. Milestone 2's first measurement of how far marks spread needs a label and mark for each of them first. Path rules propose a label and mark for each file, and the person confirms them at import ([issue 161](https://github.com/crowbaros/crowbaros/issues/161)).
- A paste into a window of a native sandbox or a workbench is refused when its source is above that compartment's start label and mark. The clipboard work for the compositor has to show that refusal to the person.
- A Tool that runs as native code must be started with everything it will read already in its view.
- Lowering a label is still a declassify, which only the person can do. Nothing in `crates/crowbar-labels` lowers a label or removes a mark.
