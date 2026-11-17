//! An ask names the answer it takes (ADR 0026). What answers it follows
//! from a fact the gate already has: whether the effect crosses the undo
//! boundary. A plain answer is accepted only for what can be undone.

use crowbar_authority::Party;
use crowbar_capability::{Principal, Right};
use crowbar_gate::{Act, Answer, Ask, AskWhy, Decision, Use, UseCost};
use crowbar_labels::{Compartment, Site};

mod common;
use common::{Bench, NOW, by, handle, task, tool, using};

/// The ask `caller` gets for `using`, as an egress when `party` is set.
/// Nothing here holds standing authority for it.
fn ask(caller: &Principal, using: &Use, party: Option<&Party>) -> Ask {
    let mut bench = Bench::new();
    let held = handle(1, caller.clone());
    let compartment = Compartment::fresh();
    let act = match party {
        Some(party) => Act::Egress {
            handle: Some(&held),
            using,
            party,
        },
        None => Act::Use {
            handle: Some(&held),
            using,
        },
    };
    match bench.decide(&by(caller, &compartment, act), NOW) {
        Decision::Asked(ask) => ask,
        other => panic!("not an ask: {other:?}"),
    }
}

/// A use whose cost depends on a value the task cannot read, so it asks
/// wherever it is.
fn at_a_cost_the_gate_cannot_read(operation: &str) -> Use {
    Use {
        cost: UseCost::DependsOnValue,
        ..using(operation, Right::Write)
    }
}

#[test]
fn an_ask_for_a_use_that_crosses_takes_the_confirm_gesture() {
    let send = using("mail.send", Right::Read);
    let asked = ask(&task("reply-to-landlord"), &send, None);
    assert_eq!(
        asked,
        Ask {
            why: AskWhy::Crosses,
            answer: Answer::ConfirmGesture,
            held: None
        }
    );
}

#[test]
fn an_ask_for_an_egress_takes_the_confirm_gesture() {
    // An egress always crosses, whatever the boundary table says of the
    // operation's name.
    let read = at_a_cost_the_gate_cannot_read("data.read");
    let party = Party::Site(Site::new("news.example"));
    let asked = ask(&tool("morning-pages"), &read, Some(&party));
    assert_eq!(asked.answer, Answer::ConfirmGesture);
}

#[test]
fn an_ask_for_a_use_inside_the_undo_boundary_takes_a_plain_answer() {
    let write = at_a_cost_the_gate_cannot_read("data.write");
    let asked = ask(&task("sort-photos"), &write, None);
    assert_eq!(
        asked,
        Ask {
            why: AskWhy::DependsOnValue,
            answer: Answer::Plain,
            held: None
        }
    );
}

#[test]
fn the_answer_follows_the_boundary_and_not_why_the_gate_asks() {
    // The same reason to ask, on an operation that crosses.
    let pay = at_a_cost_the_gate_cannot_read("payments.pay");
    let asked = ask(&task("pay-bills"), &pay, None);
    assert_eq!(
        asked,
        Ask {
            why: AskWhy::DependsOnValue,
            answer: Answer::ConfirmGesture,
            held: None
        }
    );
}
