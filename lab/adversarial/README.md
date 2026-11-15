# Adversarial lab (Milestone 2)

**Not started.** This is where the stand-ins and runs for
[Milestone 2](../../docs/milestones/milestone-2.md) go. Milestone 2 runs on stock
Linux and needs nothing from CrowbarOS Base, so this lab needs no VM image.

What belongs here, from the Milestone 2 scope:

- **Fixture services** on the local machine: a mail server, a search service, a
  public site, a hostile public site, a site with a login, and a payment service
  behind an adapter. Nothing touches a real site or a real account.
- **A stand-in for the trusted ask**, with scripted answers, and a stand-in for
  pointing.
- **The red-team harness** that plays a hostile Agent.
- **The runs**: attacks A1–A20 and B1–B7, the twenty ordinary tasks, and the three
  measurements of how far marks spread.

The code under test lives in [`crates/`](../../crates/README.md). Evidence from
runs goes to `docs/milestones/evidence/`.
