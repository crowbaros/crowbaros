//! Asks are counted against limits: one principal may have one waiting at a
//! time and ten an hour, and the Agent and all its tasks together thirty an
//! hour. Over a limit the gate still asks, and marks the ask as held, with
//! the limit it passed. Nothing is refused for asking too often (ADR 0026).

use crowbar_capability::{Principal, Rate, Right, Time};
use crowbar_gate::{AGENT_ASKS_AN_HOUR, ASKS_AN_HOUR, Act, AskWhy, Decision, Held, OPEN_ASKS};
use crowbar_labels::Compartment;

mod common;
use common::{Bench, NOW, asked, asked_but_held, by, handle, later, task, tool, using};

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
fn a_second_ask_from_a_principal_with_one_waiting_is_held_and_not_refused() {
    let mut bench = Bench::new();
    let caller = tool("morning-pages");

    assert_eq!(ask(&mut bench, &caller, NOW), asked(AskWhy::Crosses));
    assert_eq!(
        ask(&mut bench, &caller, later(1)),
        asked_but_held(AskWhy::Crosses, Held::Waiting(1))
    );
    // Another principal is not held up by this one.
    assert_eq!(ask(&mut bench, &tool("other"), later(1)), asked(AskWhy::Crosses));
}

#[test]
fn the_eleventh_ask_in_an_hour_is_held_and_names_when_the_window_lets_one_more_through() {
    let mut bench = Bench::new();
    let caller = task("reply-to-landlord");
    for minute in 0..10 {
        assert_eq!(
            ask_and_close(&mut bench, &caller, later(minute * 60)),
            asked(AskWhy::Crosses)
        );
    }

    assert_eq!(
        ask_and_close(&mut bench, &caller, later(3_599)),
        asked_but_held(
            AskWhy::Crosses,
            Held::Rate {
                rate: Rate::per(10, 3_600),
                resets_at: later(3_600),
            }
        )
    );
    // The first ask leaves the window an hour after it was raised. The held
    // ask is not in the window: the hour counts the asks that were put to
    // the person one at a time.
    assert_eq!(ask_and_close(&mut bench, &caller, later(3_600)), asked(AskWhy::Crosses));
}

#[test]
fn past_thirty_an_hour_the_asks_of_the_agent_and_all_its_tasks_are_held_in_one_line_together() {
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

    let together = asked_but_held(
        AskWhy::Crosses,
        Held::AgentRate {
            rate: Rate::per(30, 3_600),
            resets_at: later(3_600),
        },
    );
    assert_eq!(ask(&mut bench, &task("a-new-task"), later(200)), together);
    assert_eq!(ask(&mut bench, &Principal::Agent, later(200)), together);
    // A Tool's asks are not the Agent's.
    assert_eq!(
        ask(&mut bench, &tool("morning-pages"), later(200)),
        asked(AskWhy::Crosses)
    );
}

#[test]
fn an_ask_past_the_agents_limit_and_its_own_goes_in_the_agents_line() {
    let mut bench = Bench::new();
    // One task has an ask waiting, and other tasks raise twenty-nine more.
    let busy = task("busy");
    assert_eq!(ask(&mut bench, &busy, NOW), asked(AskWhy::Crosses));
    for n in 0..29 {
        assert_eq!(
            ask_and_close(&mut bench, &task(&format!("task-{n}")), later(1 + n)),
            asked(AskWhy::Crosses)
        );
    }

    // Its next ask is past its own limit and the Agent's. Were it held in
    // its own line, many tasks would make many lines.
    assert_eq!(
        ask(&mut bench, &busy, later(100)),
        asked_but_held(
            AskWhy::Crosses,
            Held::AgentRate {
                rate: Rate::per(30, 3_600),
                resets_at: later(3_600),
            }
        )
    );
}

#[test]
fn a_held_ask_waits_until_it_closes_and_does_not_count_in_the_hour() {
    // Attack A20: one task raises asks in a loop.
    let mut bench = Bench::new();
    let caller = task("reply-to-landlord");
    let in_its_line = asked_but_held(AskWhy::Crosses, Held::Waiting(1));
    assert_eq!(ask(&mut bench, &caller, NOW), asked(AskWhy::Crosses));
    for _ in 0..40 {
        assert_eq!(ask(&mut bench, &caller, later(1)), in_its_line);
    }
    // One task's flood does not use up the limit the Agent and all its
    // tasks share: another task's ask is put to the person as usual.
    assert_eq!(
        ask_and_close(&mut bench, &task("other"), later(1)),
        asked(AskWhy::Crosses)
    );

    // The person answers the first ask. The line is still there, so the
    // task's next ask joins it.
    bench.gate.ask_closed(&caller, later(2)).expect("recorded");
    assert_eq!(ask(&mut bench, &caller, later(3)), in_its_line);

    // The person dismisses the line, which closes every ask in it. The
    // flood used one of the ten in the hour, so nine are left.
    for _ in 0..41 {
        bench.gate.ask_closed(&caller, later(4)).expect("recorded");
    }
    for i in 0..9 {
        assert_eq!(
            ask_and_close(&mut bench, &caller, later(10 + i)),
            asked(AskWhy::Crosses)
        );
    }
    assert_eq!(
        ask(&mut bench, &caller, later(20)),
        asked_but_held(
            AskWhy::Crosses,
            Held::Rate {
                rate: Rate::per(10, 3_600),
                resets_at: later(3_600),
            }
        )
    );
}

#[test]
fn a_flood_of_new_tasks_is_held_in_one_line_and_none_of_it_is_refused() {
    // Attack A20: the harness proposes new tasks in a loop, and each asks.
    let mut bench = Bench::new();
    for n in 0..100 {
        let expected = match n < 30 {
            true => asked(AskWhy::Crosses),
            false => asked_but_held(
                AskWhy::Crosses,
                Held::AgentRate {
                    rate: Rate::per(30, 3_600),
                    resets_at: later(3_600),
                },
            ),
        };
        assert_eq!(ask(&mut bench, &task(&format!("task-{n}")), later(n)), expected);
    }
}
