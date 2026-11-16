//! Asks are rate-limited: one principal may have one open at a time and ten
//! an hour, and the Agent and all its tasks together thirty an hour. Over a
//! limit the gate refuses, names the limit and when its window resets, and
//! queues nothing.

use crowbar_capability::{Principal, Rate, Right, Time};
use crowbar_gate::{AGENT_ASKS_AN_HOUR, ASKS_AN_HOUR, Act, AskWhy, Decision, GateRule, Limit, OPEN_ASKS};
use crowbar_labels::Compartment;

mod common;
use common::{Bench, NOW, asked, by, handle, later, refused, task, tool, using};

/// `caller` asks to send a mail, which crosses and has no standing rule.
fn ask(bench: &mut Bench, caller: &Principal, now: Time) -> Decision {
    let held = handle(1, caller.clone());
    let send = using("mail.send", Right::Read);
    let compartment = Compartment::fresh();
    bench.decide(
        &by(
            caller,
            &compartment,
            Act::Use {
                handle: Some(&held),
                using: &send,
            },
        ),
        now,
    )
}

/// `caller` asks, and the ask closes at once.
fn ask_and_close(bench: &mut Bench, caller: &Principal, now: Time) -> Decision {
    let decision = ask(bench, caller, now);
    bench.gate.ask_closed(caller, now).expect("recorded");
    decision
}

#[test]
fn the_limits_are_one_open_ten_an_hour_and_thirty_an_hour_for_the_agent_and_its_tasks() {
    assert_eq!(OPEN_ASKS, 1);
    assert_eq!(ASKS_AN_HOUR, Rate::per(10, 3_600));
    assert_eq!(AGENT_ASKS_AN_HOUR, Rate::per(30, 3_600));
}

#[test]
fn a_principal_has_one_ask_open_at_a_time() {
    let mut bench = Bench::new();
    let caller = task("reply-to-landlord");

    assert_eq!(ask(&mut bench, &caller, NOW), asked(AskWhy::Crosses));
    assert_eq!(
        ask(&mut bench, &caller, later(1)),
        refused(GateRule::AskRate, Limit::OpenAsks(1))
    );
    // Another task is not held up by this one.
    assert_eq!(ask(&mut bench, &task("other"), later(1)), asked(AskWhy::Crosses));

    bench.gate.ask_closed(&caller, later(2)).expect("recorded");
    assert_eq!(ask(&mut bench, &caller, later(3)), asked(AskWhy::Crosses));
}

#[test]
fn a_principal_asks_ten_times_an_hour_and_is_told_when_it_may_ask_again() {
    let mut bench = Bench::new();
    let caller = task("reply-to-landlord");
    for minute in 0..10 {
        assert_eq!(
            ask_and_close(&mut bench, &caller, later(minute * 60)),
            asked(AskWhy::Crosses)
        );
    }

    assert_eq!(
        ask(&mut bench, &caller, later(3_599)),
        refused(
            GateRule::AskRate,
            Limit::AskRate {
                rate: Rate::per(10, 3_600),
                resets_at: later(3_600),
            }
        )
    );
    // The first ask leaves the window an hour after it was raised.
    assert_eq!(ask(&mut bench, &caller, later(3_600)), asked(AskWhy::Crosses));
}

#[test]
fn the_agent_and_its_tasks_together_ask_thirty_times_an_hour() {
    let mut bench = Bench::new();
    // Nine tasks ask three times each, and the Agent three times: its asks,
    // such as those that start tasks, count too.
    for n in 0..9 {
        let caller = task(&format!("task-{n}"));
        for i in 0..3 {
            assert_eq!(
                ask_and_close(&mut bench, &caller, later(n * 10 + i)),
                asked(AskWhy::Crosses)
            );
        }
    }
    for i in 0..3 {
        assert_eq!(
            ask_and_close(&mut bench, &Principal::Agent, later(100 + i)),
            asked(AskWhy::Crosses)
        );
    }

    let limit = refused(
        GateRule::AskRate,
        Limit::AskRate {
            rate: Rate::per(30, 3_600),
            resets_at: later(3_600),
        },
    );
    assert_eq!(ask(&mut bench, &task("a-new-task"), later(200)), limit);
    assert_eq!(ask(&mut bench, &Principal::Agent, later(200)), limit);
    // A Tool's asks are not the Agent's.
    assert_eq!(
        ask(&mut bench, &tool("morning-pages"), later(200)),
        asked(AskWhy::Crosses)
    );
}

#[test]
fn an_ask_over_the_rate_is_not_counted_or_queued() {
    let mut bench = Bench::new();
    let caller = task("reply-to-landlord");
    assert_eq!(ask(&mut bench, &caller, NOW), asked(AskWhy::Crosses));
    for _ in 0..20 {
        assert_eq!(
            ask(&mut bench, &caller, later(1)),
            refused(GateRule::AskRate, Limit::OpenAsks(1))
        );
    }

    // The refusals opened nothing: one close is enough, and nine asks are
    // left in the hour.
    bench.gate.ask_closed(&caller, later(2)).expect("recorded");
    for i in 0..9 {
        assert_eq!(
            ask_and_close(&mut bench, &caller, later(10 + i)),
            asked(AskWhy::Crosses)
        );
    }
}
