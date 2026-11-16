//! A Data handle's label and mark are at or above the origin of every
//! object it names, checked against the origin table before the gate allows
//! the issue (ADR 0019). An object with no record is the unknown row.

use crowbar_capability::{EffectLimits, Issue, Principal, Resource, Right, Rights};
use crowbar_gate::{Act, GateRule, Limit};
use crowbar_labels::{
    Category, Compartment, Fetch, Label, LabelMark, LabelStore, MailAccount, MarkSource, ObjectId, Origin, Site,
};

mod common;
use common::{Bench, NOW, allowed, at, by, personal, refused, task};

fn mail() -> LabelMark {
    Origin::Mail {
        account: MailAccount::new("me@mail.example"),
    }
    .label_mark()
}

fn news() -> LabelMark {
    Origin::WebPage(Fetch::WithoutLogin(Site::new("news.example"))).label_mark()
}

/// A label store that records `objects` with these labels and marks.
fn store(objects: &[(&str, LabelMark)]) -> LabelStore {
    let mut labels = LabelStore::default();
    for (name, label_mark) in objects {
        labels.create(&ObjectId::new(name), label_mark).expect("new");
    }
    labels
}

/// An issue to `holder` of a read handle on `objects`, at `label_mark`.
fn issue(holder: Principal, objects: &[&str], label_mark: LabelMark) -> Issue {
    Issue {
        holder,
        resource: Resource::Data(objects.iter().map(|name| ObjectId::new(name)).collect()),
        rights: Rights::from([Right::Read]),
        limits: EffectLimits::new(),
        label_mark,
        delegable: false,
        reason: "Answers your mail.".to_string(),
    }
}

fn decide_issue(bench: &mut Bench, issue: &Issue, labels: &LabelStore) -> crowbar_gate::Decision {
    let broker = task("reply-to-landlord");
    let compartment = Compartment::fresh();
    bench.decide(&by(&broker, &compartment, Act::Issue { issue, labels }), NOW)
}

#[test]
fn a_handle_at_the_join_of_its_objects_origins_is_issued() {
    let mut bench = Bench::new();
    let labels = store(&[("mail/1", mail()), ("notes/1", personal())]);

    let at_join = issue(
        task("reply-to-landlord"),
        &["mail/1", "notes/1"],
        mail().join(&personal()),
    );
    assert_eq!(decide_issue(&mut bench, &at_join, &labels), allowed());
    // Stricter than it needs to be is allowed too.
    let stricter = issue(task("reply-to-landlord"), &["notes/1"], mail());
    assert_eq!(decide_issue(&mut bench, &stricter, &labels), allowed());
}

#[test]
fn a_handle_with_a_label_below_its_objects_origin_is_refused() {
    let mut bench = Bench::new();
    let labels = store(&[("mail/1", mail())]);

    let lowered = issue(task("reply-to-landlord"), &["mail/1"], personal().join(&news()));
    assert_eq!(
        decide_issue(&mut bench, &lowered, &labels),
        refused(
            GateRule::Label,
            Limit::Origin {
                label: Label::private(Category::Messages),
                mark: [MarkSource::Mail(MailAccount::new("me@mail.example"))].into(),
            }
        )
    );
}

#[test]
fn a_handle_with_a_mark_missing_a_source_is_refused() {
    let mut bench = Bench::new();
    let labels = store(&[("mail/1", mail()), ("page/1", news())]);

    // The label is high enough, and the mark names the mail but not the page.
    let missing = issue(task("reply-to-landlord"), &["mail/1", "page/1"], mail());
    assert_eq!(
        decide_issue(&mut bench, &missing, &labels),
        refused(
            GateRule::Label,
            Limit::Origin {
                label: Label::private(Category::Messages),
                mark: [
                    MarkSource::Site(Site::new("news.example")),
                    MarkSource::Mail(MailAccount::new("me@mail.example")),
                ]
                .into(),
            }
        )
    );
}

#[test]
fn an_object_with_no_record_counts_as_the_unknown_row() {
    let mut bench = Bench::new();
    let labels = LabelStore::default();

    let decision = decide_issue(
        &mut bench,
        &issue(task("reply-to-landlord"), &["old/file"], personal()),
        &labels,
    );
    assert_eq!(
        decision,
        refused(
            GateRule::Label,
            Limit::Origin {
                label: Label::private(Category::Unknown),
                mark: [MarkSource::Unknown].into(),
            }
        )
    );
    let unknown = issue(task("reply-to-landlord"), &["old/file"], Origin::Unknown.label_mark());
    assert_eq!(decide_issue(&mut bench, &unknown, &labels), allowed());
}

#[test]
fn an_issue_already_out_of_time_or_delegable_to_the_agent_or_to_no_one_is_refused() {
    let mut bench = Bench::new();
    let labels = store(&[("notes/1", personal())]);

    let mut expired = issue(task("reply-to-landlord"), &["notes/1"], personal());
    expired.limits = EffectLimits::new().until(at(NOW.as_secs()));
    assert_eq!(
        decide_issue(&mut bench, &expired, &labels),
        refused(GateRule::Expired, Limit::Until(NOW))
    );

    let mut to_agent = issue(Principal::Agent, &["notes/1"], personal());
    to_agent.delegable = true;
    assert_eq!(
        decide_issue(&mut bench, &to_agent, &labels),
        refused(GateRule::ToAgent, Limit::None)
    );

    let nameless = issue(Principal::Tool(String::new()), &["notes/1"], personal());
    assert_eq!(
        decide_issue(&mut bench, &nameless, &labels),
        refused(GateRule::Unknown, Limit::None)
    );
}
