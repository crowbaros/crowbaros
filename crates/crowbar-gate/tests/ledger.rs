//! The gate counts from the ledger. It rebuilds the count from the ledger's
//! history when it starts, so a restart resets no limit and forgets no
//! idempotency key. Every decision is recorded before it is returned, and a
//! ledger that cannot be added to refuses every operation.

use std::path::Path;

use crowbar_authority::{Authority, Change, Decision, Error, Event, Grant, Holder, Keyed, Ledger, LedgerEntry, Rule};
use crowbar_capability::{
    Cost, EffectLimits, HandleId, IdempotencyKey, Principal, Rate, Resource, Right, Rights, TaskId, Time,
};
use crowbar_gate::{
    Act, Answer, AskWhy, Caller, Gate, GateRule, Held, Limit, Operation, Original, Presented, Record, Refuse, Ruling,
    Use, UseCost,
};
use crowbar_labels::{Compartment, ObjectId};

mod common;
use common::{NOW, Scratch, by, handle, later, reading, task, tool, using};

/// The gate as the broker starts it: the ledger opened first, then its
/// history read, so no other writer adds to it in between.
fn start(dir: &Path) -> Gate<Ledger> {
    let ledger = Ledger::open(dir).expect("opened");
    let history = Ledger::read(dir).expect("read");
    Gate::start(ledger, &history.entries)
}

fn authority() -> Authority {
    Authority::default()
}

fn use_of(gate: &mut Gate<Ledger>, held: &Presented, what: &Use, now: Time) -> Ruling {
    let compartment = Compartment::fresh();
    gate.decide(
        Some(&authority()),
        &by(
            &held.holder,
            &compartment,
            Act::Use {
                handle: Some(held),
                using: what,
            },
        ),
        now,
    )
}

fn decision(ruling: Ruling) -> Decision {
    ruling.decision()
}

#[test]
fn a_restart_in_the_middle_of_a_rate_window_resets_nothing() {
    let dir = Scratch::new();
    let mut held = handle(1, task("sort-photos"));
    held.limits = EffectLimits::new().rate(Rate::per(2, 3_600));

    let mut gate = start(dir.path());
    assert!(matches!(use_of(&mut gate, &held, &reading(1), NOW), Ruling::Allowed(_)));
    assert!(matches!(
        use_of(&mut gate, &held, &reading(1), later(10)),
        Ruling::Allowed(_)
    ));
    drop(gate);

    let mut gate = start(dir.path());
    assert_eq!(
        decision(use_of(&mut gate, &held, &reading(1), later(20))),
        Decision::Refused(Refuse::with(GateRule::Limits, Limit::Rate(Rate::per(2, 3_600))))
    );
    assert!(matches!(
        use_of(&mut gate, &held, &reading(1), later(3_600)),
        Ruling::Allowed(_)
    ));
}

#[test]
fn a_restart_keeps_a_copys_uses_counted_against_its_original() {
    let dir = Scratch::new();
    let mut original = handle(1, task("sort-photos"));
    original.limits = EffectLimits::new().count(2).cost(Cost::cents(100));
    let mut copy = handle(2, tool("photo-sorter"));
    copy.original = Some(Original {
        id: original.id,
        limits: original.limits.clone(),
        ended: false,
    });
    let costing = |cents| Use {
        cost: UseCost::Fixed(Cost::cents(cents)),
        ..reading(1)
    };

    let mut gate = start(dir.path());
    assert!(matches!(
        use_of(&mut gate, &copy, &costing(70), NOW),
        Ruling::Allowed(_)
    ));
    drop(gate);

    let mut gate = start(dir.path());
    assert_eq!(
        decision(use_of(&mut gate, &original, &costing(40), later(1))),
        Decision::Refused(Refuse::with(GateRule::Limits, Limit::Cost(Cost::cents(100))))
    );
    assert!(matches!(
        use_of(&mut gate, &original, &costing(30), later(2)),
        Ruling::Allowed(_)
    ));
    assert_eq!(
        decision(use_of(&mut gate, &copy, &costing(0), later(3))),
        Decision::Refused(Refuse::with(GateRule::Limits, Limit::Count(2)))
    );
}

#[test]
fn a_restart_keeps_a_standing_rules_rate() {
    let dir = Scratch::new();
    let holder = Holder::Tool("morning-pages".to_string());
    let writing = Resource::Data([ObjectId::new("writing/journal.md")].into());
    let authority = Authority::from_changes([
        (
            NOW,
            Change::Granted(Grant {
                holder: holder.clone(),
                resource: writing.clone(),
                rights: Rights::from([Right::Read]),
                limits: EffectLimits::new(),
                reason: "Keeps your morning pages.".to_string(),
            }),
        ),
        (
            NOW,
            Change::RuleAdded(Rule {
                holder,
                operation: "nextcloud.sync".to_string(),
                limits: EffectLimits::new().rate(Rate::per(1, 600)),
                sources: [].into(),
            }),
        ),
    ])
    .expect("admitted");
    let caller = tool("morning-pages");
    let held = Presented {
        resource: writing,
        ..handle(1, caller.clone())
    };
    let sync = using("nextcloud.sync", Right::Read);
    let compartment = Compartment::fresh();
    let operation = by(
        &caller,
        &compartment,
        Act::Use {
            handle: Some(&held),
            using: &sync,
        },
    );

    let mut gate = start(dir.path());
    assert!(matches!(
        gate.decide(Some(&authority), &operation, NOW),
        Ruling::Allowed(_)
    ));
    drop(gate);

    let mut gate = start(dir.path());
    assert!(matches!(
        gate.decide(Some(&authority), &operation, later(60)),
        Ruling::Asked(crowbar_gate::Ask {
            why: AskWhy::Crosses,
            answer: Answer::ConfirmGesture,
            held: None
        })
    ));
}

#[test]
fn idempotency_keys_survive_a_restart() {
    let dir = Scratch::new();
    let caller = task("sort-photos");
    let held = handle(1, caller.clone());
    let compartment = Compartment::fresh();
    let key = IdempotencyKey::new("move-1");
    let one = reading(1);
    let two = reading(2);
    let keyed = |what| Operation {
        caller: Caller {
            principal: &caller,
            standing_task: None,
            compartment: &compartment,
        },
        key: Some(&key),
        act: Act::Use {
            handle: Some(&held),
            using: what,
        },
    };

    let mut gate = start(dir.path());
    assert!(matches!(
        gate.decide(Some(&authority()), &keyed(&one), NOW),
        Ruling::Allowed(_)
    ));
    drop(gate);

    let mut gate = start(dir.path());
    assert!(matches!(
        gate.decide(Some(&authority()), &keyed(&one), later(1)),
        Ruling::Repeated(Decision::Allowed { .. })
    ));
    assert!(matches!(
        gate.decide(Some(&authority()), &keyed(&two), later(1)),
        Ruling::Refused(Refuse {
            rule: GateRule::Idempotency,
            ..
        })
    ));
    drop(gate);
    // The repeat added nothing; the refusal is recorded.
    assert_eq!(Ledger::read(dir.path()).expect("read").entries.len(), 2);
}

#[test]
fn waiting_asks_held_or_not_and_their_closing_survive_a_restart() {
    let dir = Scratch::new();
    let caller = task("reply-to-landlord");
    let held = handle(1, caller.clone());
    let send = using("mail.send", Right::Read);
    let in_its_line = common::asked_but_held(AskWhy::Crosses, Held::Waiting(1));

    let mut gate = start(dir.path());
    assert_eq!(
        decision(use_of(&mut gate, &held, &send, NOW)),
        common::asked(AskWhy::Crosses)
    );
    drop(gate);

    // The ask from before the restart is still waiting, so the next is held.
    // A held ask is still an ask: it carries no `Allowed`.
    let mut gate = start(dir.path());
    let ruling = use_of(&mut gate, &held, &send, later(1));
    assert!(matches!(ruling, Ruling::Asked(_)));
    assert_eq!(decision(ruling), in_its_line);
    gate.ask_closed(&caller, later(2)).expect("recorded");
    drop(gate);
    // The ledger keeps the mark: the second entry is the held ask.
    let recorded = Ledger::read(dir.path()).expect("read");
    assert_eq!(recorded.entries[1].entry.event, Event::Decided(in_its_line.clone()));

    // One closed and the held one still waits, so the next is held too.
    let mut gate = start(dir.path());
    assert_eq!(decision(use_of(&mut gate, &held, &send, later(3))), in_its_line);
    gate.ask_closed(&caller, later(4)).expect("recorded");
    gate.ask_closed(&caller, later(4)).expect("recorded");
    drop(gate);

    // The held asks were not counted in the hour: with none waiting, the
    // next is put to the person as usual.
    let mut gate = start(dir.path());
    assert_eq!(
        decision(use_of(&mut gate, &held, &send, later(5))),
        common::asked(AskWhy::Crosses)
    );
}

#[test]
fn each_decision_is_recorded_with_the_handle_its_original_the_task_the_objects_the_cost_and_the_key() {
    let dir = Scratch::new();
    let caller = task("sort-photos");
    let mut copy = handle(2, caller.clone());
    copy.original = Some(Original {
        id: HandleId::from_bits(1),
        limits: EffectLimits::new(),
        ended: false,
    });
    let compartment = Compartment::fresh();
    let key = IdempotencyKey::new("move-1");
    let read = Use {
        cost: UseCost::Fixed(Cost::cents(5)),
        ..reading(3)
    };
    let operation = Operation {
        caller: Caller {
            principal: &caller,
            standing_task: None,
            compartment: &compartment,
        },
        key: Some(&key),
        act: Act::Use {
            handle: Some(&copy),
            using: &read,
        },
    };

    let mut gate = start(dir.path());
    let Ruling::Allowed(allowed) = gate.decide(Some(&authority()), &operation, NOW) else {
        panic!("allowed");
    };
    drop(gate);

    let history = Ledger::read(dir.path()).expect("read");
    let [recorded] = &history.entries[..] else {
        panic!("one entry");
    };
    assert_eq!(recorded.seq, allowed.entry());
    let entry = &recorded.entry;
    assert_eq!(
        (
            &entry.principal,
            entry.operation.as_str(),
            &entry.event,
            entry.handle,
            entry.original,
            &entry.task,
            entry.objects,
            entry.cost,
            entry.key.as_ref().map(|keyed| &keyed.key),
        ),
        (
            &caller,
            "data.read",
            &Event::Decided(Decision::Allowed { under: vec![] }),
            Some(HandleId::from_bits(2)),
            Some(HandleId::from_bits(1)),
            &Some(TaskId::new("sort-photos")),
            Some(3),
            Some(Cost::cents(5)),
            Some(&key),
        )
    );
    assert!(matches!(entry.key, Some(Keyed { ref digest, .. }) if digest.len() == 64));
}

/// A ledger whose disk has gone away.
struct Broken;

impl Record for Broken {
    fn append(&mut self, _: &LedgerEntry, _: Time) -> Result<u64, Error> {
        Err(Error::NotStorable("the disk is gone".to_string()))
    }
}

#[test]
fn a_ledger_that_cannot_be_added_to_refuses_every_operation_and_dry_runs_still_answer() {
    let mut gate = Gate::start(Broken, &[]);
    let caller = task("sort-photos");
    let held = handle(1, caller.clone());
    let compartment = Compartment::fresh();
    let read = reading(1);
    let operation = by(
        &caller,
        &compartment,
        Act::Use {
            handle: Some(&held),
            using: &read,
        },
    );

    assert_eq!(
        gate.dry_run(Some(&authority()), &operation, NOW),
        Decision::Allowed { under: vec![] }
    );
    assert!(matches!(
        gate.decide(Some(&authority()), &operation, NOW),
        Ruling::Refused(Refuse {
            rule: GateRule::Ledger,
            limit: Limit::None
        })
    ));
    assert!(gate.ask_closed(&Principal::Agent, NOW).is_err());
}
