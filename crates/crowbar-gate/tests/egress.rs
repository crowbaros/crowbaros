//! Three kinds of egress cross on authority the person already gave, and do
//! not ask (ADR 0011, ADR 0021): a model request to a cleared provider,
//! installed software or a workbench reaching what its approval showed, and
//! a compartment that holds only public data and no login reaching a host
//! on the open internet. Every other crossing still needs a standing grant
//! and a standing rule, or it asks.

use crowbar_authority::{Authority, Change, Grant, Holder, Party, Rule, StandingId};
use crowbar_capability::{EffectLimits, Principal, Rate, Resource, Right, Rights};
use crowbar_gate::{Act, Answer, Ask, AskWhy, Caller, Decision, GateRule, Limit, Operation, Presented};
use crowbar_labels::{Category, Compartment, Fetch, Label, LabelMark, Labeled, MarkSource, ObjectId, Origin, Site};

mod common;
use common::{Bench, NOW, allowed, asked, at, by, handle, later, personal, refused, task, using};

fn provider() -> Party {
    Party::ModelProvider("provider.example".to_string())
}

/// A compartment that has read each of `values`.
fn having_read(values: &[LabelMark]) -> Compartment {
    let mut compartment = Compartment::fresh();
    for value in values {
        compartment.read(value).expect("a task's compartment grows");
    }
    compartment
}

/// A page of a news site, fetched with no login: public, and marked.
fn a_web_page() -> LabelMark {
    Origin::WebPage(Fetch::WithoutLogin(Site::new("news.example"))).label_mark()
}

/// A page of the bank's site, read under a login: private to that site.
fn a_bank_page() -> LabelMark {
    Origin::WebPage(Fetch::UnderLogin(Site::new("bank.example"))).label_mark()
}

/// The handle `holder` makes model requests on.
fn model(holder: Principal) -> Presented {
    Presented {
        resource: Resource::Service("model".to_string()),
        rights: Rights::from([Right::Connect]),
        ..handle(1, holder)
    }
}

/// `caller`, in `compartment`, makes a model request.
fn model_request(bench: &mut Bench, caller: &Principal, compartment: &Compartment) -> Decision {
    let held = model(caller.clone());
    let request = using("model.request", Right::Connect);
    bench.decide(
        &by(
            caller,
            compartment,
            Act::Egress {
                handle: Some(&held),
                using: &request,
                party: &provider(),
            },
        ),
        NOW,
    )
}

#[test]
fn a_model_request_to_a_provider_cleared_for_what_the_compartment_holds_does_not_ask() {
    let mut bench = Bench::new();
    // The Agent and an ad hoc task hold no standing grant or rule. The
    // clearance is the standing authority for a model request.
    let conversation = having_read(&[personal()]);
    assert_eq!(model_request(&mut bench, &Principal::Agent, &conversation), allowed());
    assert_eq!(
        model_request(&mut bench, &task("sort-photos"), &conversation),
        allowed()
    );

    // A mark does not change that: a conversation that read a web page
    // still reaches its cleared provider.
    let marked = having_read(&[personal(), a_web_page()]);
    assert!(marked.label_mark().mark().is_marked());
    assert_eq!(model_request(&mut bench, &Principal::Agent, &marked), allowed());
}

#[test]
fn a_model_request_to_a_provider_that_is_not_cleared_is_still_refused() {
    // Attack A3: a task holding finance data calls the model provider,
    // which is cleared for personal.
    let mut bench = Bench::new();
    assert_eq!(
        model_request(&mut bench, &task("budget"), &having_read(&[a_bank_page()])),
        refused(GateRule::Clearance, Limit::Clearance(Label::Personal))
    );
}

fn news() -> Site {
    Site::new("news.example")
}

fn hostile() -> Site {
    Site::new("hostile.example")
}

/// A connection handle to `host`, held by `holder`.
fn connection(holder: Principal, host: Site) -> Presented {
    Presented {
        resource: Resource::Endpoint { host, port: 443 },
        rights: Rights::from([Right::Connect]),
        ..handle(1, holder)
    }
}

/// Whether the broker's record says the compartment holds a login.
#[derive(Clone, Copy)]
enum Login {
    None,
    Held,
}

/// `caller`, in `compartment`, connects to `host` on a handle for it.
fn connect(bench: &mut Bench, caller: &Principal, compartment: &Compartment, login: Login, host: Site) -> Decision {
    let held = connection(caller.clone(), host.clone());
    let get = using("http.get", Right::Connect);
    bench.decide(
        &Operation {
            caller: Caller {
                principal: caller,
                standing_task: None,
                holds_login: matches!(login, Login::Held),
                compartment,
            },
            key: None,
            act: Act::Egress {
                handle: Some(&held),
                using: &get,
                party: &Party::Site(host),
            },
        },
        NOW,
    )
}

#[test]
fn a_compartment_that_holds_only_public_data_and_no_login_reaches_the_open_internet_without_asking() {
    let mut bench = Bench::new();
    let lookup = task("lookup");
    assert_eq!(
        connect(&mut bench, &lookup, &Compartment::fresh(), Login::None, news()),
        allowed()
    );
    // A lookup is public and marked by what it read. It reaches any host:
    // nothing private is inside to leak.
    let marked = having_read(&[a_web_page()]);
    assert!(marked.label_mark().mark().is_marked());
    assert_eq!(connect(&mut bench, &lookup, &marked, Login::None, hostile()), allowed());
}

#[test]
fn a_public_compartment_that_holds_a_login_still_asks() {
    // With a login, a request can act on the person's account.
    let mut bench = Bench::new();
    assert_eq!(
        connect(
            &mut bench,
            &task("book-a-slot"),
            &Compartment::fresh(),
            Login::Held,
            news()
        ),
        asked(AskWhy::Crosses)
    );
}

#[test]
fn a_compartment_that_holds_more_than_public_data_is_not_let_through_by_a_higher_clearance() {
    // The person cleared this site for personal data. The clearance check
    // passes, and the egress still crosses: the compartment does not hold
    // only public data.
    let mut bench = Bench::with(vec![Change::ClearanceSet {
        party: Party::Site(news()),
        clearance: Label::Personal,
    }]);
    assert_eq!(
        connect(
            &mut bench,
            &task("research"),
            &having_read(&[personal()]),
            Login::None,
            news()
        ),
        asked(AskWhy::Crosses)
    );
}

#[test]
fn private_data_still_does_not_leave_for_a_site_that_is_not_cleared() {
    let mut bench = Bench::new();
    let refusal = refused(GateRule::Clearance, Limit::Clearance(Label::Public));
    // Attacks A1 and A2: a task holding a private file connects to the
    // hostile site, and to a public site named in the task.
    let holding_a_file = having_read(&[Origin::Unknown.label_mark()]);
    for host in [hostile(), news()] {
        assert_eq!(
            connect(&mut bench, &task("summarize"), &holding_a_file, Login::None, host),
            refusal
        );
    }
    // Attack A14: a task holding data from the login site reads the hostile
    // site's page, then connects to the hostile site.
    let after_the_page = having_read(&[a_bank_page(), a_web_page()]);
    assert_eq!(
        connect(
            &mut bench,
            &task("book-a-slot"),
            &after_the_page,
            Login::Held,
            hostile()
        ),
        refusal
    );
}

#[test]
fn only_a_connection_to_the_host_its_handle_names_goes_unasked() {
    let mut bench = Bench::new();
    let caller = task("lookup");
    let compartment = Compartment::fresh();
    let get = using("http.get", Right::Connect);

    // The handle names one host, and the broker states another party.
    let to_news = connection(caller.clone(), news());
    let elsewhere = Party::Site(hostile());
    assert_eq!(
        bench.decide(
            &by(
                &caller,
                &compartment,
                Act::Egress {
                    handle: Some(&to_news),
                    using: &get,
                    party: &elsewhere,
                },
            ),
            NOW,
        ),
        asked(AskWhy::Crosses)
    );

    // An adapter operation is not a connection to a host: the adapter may
    // act on the person's account.
    let other = task("other");
    let mail = Presented {
        resource: Resource::Service("mail".to_string()),
        ..handle(2, other.clone())
    };
    let send = using("mail.send", Right::Read);
    assert_eq!(
        bench.decide(
            &by(
                &other,
                &compartment,
                Act::Egress {
                    handle: Some(&mail),
                    using: &send,
                    party: &Party::Site(news()),
                },
            ),
            NOW,
        ),
        asked(AskWhy::Crosses)
    );
}

#[test]
fn with_no_authority_to_read_every_egress_still_asks() {
    let mut bench = Bench::without_authority();
    assert_eq!(
        connect(&mut bench, &task("lookup"), &Compartment::fresh(), Login::None, news()),
        asked(AskWhy::NoAuthority)
    );
    assert_eq!(
        model_request(&mut bench, &task("other"), &Compartment::fresh()),
        asked(AskWhy::NoAuthority)
    );
}

fn editor() -> Principal {
    Principal::Installed("editor".to_string())
}

fn sync_host() -> Site {
    Site::new("sync.example")
}

/// What the editor's approval screen showed, as the authority record keeps
/// it: it opens and saves the person's documents, which the screen showed
/// as personal, and it may connect to its sync host. The approval sets no
/// clearance: a site stays cleared for public data.
fn the_editors_approval() -> Vec<Change> {
    let holder = Holder::Installed("editor".to_string());
    vec![
        Change::Granted(Grant {
            holder: holder.clone(),
            resource: Resource::Data([ObjectId::new("documents/essay.txt")].into()),
            rights: Rights::from([Right::Read, Right::Write]),
            limits: EffectLimits::new(),
            reason: "Opens and saves your documents.".to_string(),
            shows: Some(Label::Personal),
        }),
        Change::Granted(Grant {
            holder: holder.clone(),
            resource: Resource::Endpoint {
                host: sync_host(),
                port: 443,
            },
            rights: Rights::from([Right::Connect]),
            limits: EffectLimits::new(),
            reason: "Syncs your settings.".to_string(),
            shows: None,
        }),
        Change::RuleAdded(Rule {
            holder,
            operation: "http.get".to_string(),
            limits: EffectLimits::new().rate(Rate::per(60, 3_600)),
            sources: [].into(),
            cap: None,
        }),
    ]
}

/// The ids of the grant on `host` and the rule that `holder` holds, in that
/// order: what a connection to a host its approval lists goes under.
fn standing(authority: &Authority, holder: &Holder, host: &Site) -> Vec<StandingId> {
    let grants = authority
        .grants(holder)
        .filter(|(_, grant)| matches!(&grant.resource, Resource::Endpoint { host: named, .. } if named == host))
        .map(|(id, _)| id);
    grants.chain(authority.rules(holder).map(|(id, _)| id)).collect()
}

#[test]
fn installed_software_reaches_what_its_approval_showed_without_asking() {
    let mut bench = Bench::with(the_editors_approval());
    // Its view holds the person's files, so its compartment is personal,
    // and the person has cleared its sync host for nothing.
    let compartment = Compartment::fixed([&personal()]);
    let authority = bench.authority.as_ref().expect("read");
    assert_eq!(authority.clearance(&Party::Site(sync_host())), Label::Public);
    let under = standing(authority, &Holder::Installed("editor".to_string()), &sync_host());
    assert_eq!(under.len(), 2);
    assert_eq!(
        connect(&mut bench, &editor(), &compartment, Login::None, sync_host()),
        Decision::Allowed { under }
    );
}

#[test]
fn an_approval_clears_a_host_for_what_it_approves_and_for_nothing_else() {
    let mut bench = Bench::with(the_editors_approval());
    // Each holds what the editor holds, and connects to the editor's host.
    let compartment = Compartment::fixed([&personal()]);
    let others = [
        task("backup"),
        Principal::Tool("editor".to_string()),
        Principal::Agent,
        Principal::Workbench("editor".to_string()),
        Principal::Installed("notes".to_string()),
    ];
    for other in others {
        assert_eq!(
            connect(&mut bench, &other, &compartment, Login::None, sync_host()),
            refused(GateRule::Clearance, Limit::Clearance(Label::Public)),
            "{other:?}"
        );
    }
}

/// A health summary: the person's own words, made private.
fn a_health_summary() -> LabelMark {
    Labeled::from_origin((), &Origin::Typed)
        .raise(&Label::private(Category::Health))
        .label_mark()
        .clone()
}

#[test]
fn installed_software_that_holds_more_than_its_approval_showed_is_refused_at_any_host() {
    // The approval showed personal documents. A task has since written a
    // private health summary into them, so the editor's view now holds it.
    let mut bench = Bench::with(the_editors_approval());
    let compartment = Compartment::fixed([&personal(), &a_health_summary()]);
    for host in [sync_host(), news()] {
        assert_eq!(
            connect(&mut bench, &editor(), &compartment, Login::None, host),
            refused(GateRule::Clearance, Limit::Clearance(Label::Personal))
        );
    }
}

#[test]
fn installed_software_holding_the_persons_files_asks_for_a_signature_at_a_host_its_approval_did_not_show() {
    // Attack A32, as it will usually be met: the editor holds what its
    // approval showed, and the host is not one the approval lists. It is
    // asked, and not refused: a yes would add the host to the approval.
    let mut bench = Bench::with(the_editors_approval());
    let compartment = Compartment::fixed([&personal()]);
    assert_eq!(
        connect(&mut bench, &editor(), &compartment, Login::None, news()),
        Decision::Asked(Ask {
            why: AskWhy::Crosses,
            answer: Answer::Signature,
            held: None
        })
    );
}

#[test]
fn installed_software_reaching_a_host_its_approval_did_not_show_asks_for_a_signature() {
    // Attack A32. Saying yes would add a need to its approval, which is
    // authority that lasts.
    let mut bench = Bench::with(the_editors_approval());
    // It holds only public data and no login. That lets a task through,
    // and not installed software: its reach is what its approval showed.
    let compartment = Compartment::fixed([]);
    assert_eq!(compartment.label_mark().label(), &Label::Public);
    assert_eq!(
        connect(&mut bench, &editor(), &compartment, Login::None, news()),
        Decision::Asked(Ask {
            why: AskWhy::Crosses,
            answer: Answer::Signature,
            held: None
        })
    );
}

#[test]
fn a_tool_and_the_agent_holding_only_public_data_still_ask_before_reaching_a_host() {
    // A Tool reaches what the person granted it, and the Agent makes no
    // connection of its own. Neither is let through for holding nothing
    // private.
    let mut bench = Bench::new();
    let compartment = Compartment::fresh();
    for caller in [Principal::Tool("morning-pages".to_string()), Principal::Agent] {
        assert_eq!(
            connect(&mut bench, &caller, &compartment, Login::None, news()),
            asked(AskWhy::Crosses)
        );
    }
}

fn ledger_workbench() -> Principal {
    Principal::Workbench("ledger".to_string())
}

fn registry() -> Site {
    Site::new("registry.example")
}

/// A file of the ledger project: the person's own words, made private.
fn a_ledger_file() -> LabelMark {
    Labeled::from_origin((), &Origin::Typed)
        .raise(&Label::private(Category::Finance))
        .label_mark()
        .clone()
}

/// The profile of the workbench for a private project, as the authority
/// record keeps it: it works on the project's files, which the profile
/// showed as private finance data, and it may connect to the package
/// registry. Its rule is for the mark a workbench always carries.
fn the_ledger_profile() -> Vec<Change> {
    let holder = Holder::Workbench("ledger".to_string());
    vec![
        Change::Granted(Grant {
            holder: holder.clone(),
            resource: Resource::Data([ObjectId::new("projects/ledger/ledger.txt")].into()),
            rights: Rights::from([Right::Read, Right::Write]),
            limits: EffectLimits::new(),
            reason: "Works on the files of your ledger project.".to_string(),
            shows: Some(Label::private(Category::Finance)),
        }),
        Change::Granted(Grant {
            holder: holder.clone(),
            resource: Resource::Endpoint {
                host: registry(),
                port: 443,
            },
            rights: Rights::from([Right::Connect]),
            limits: EffectLimits::new(),
            reason: "Fetches packages from the registry.".to_string(),
            shows: None,
        }),
        Change::RuleAdded(Rule {
            holder,
            operation: "http.get".to_string(),
            limits: EffectLimits::new().rate(Rate::per(600, 3_600)),
            sources: [MarkSource::Workbench].into(),
            cap: None,
        }),
    ]
}

#[test]
fn a_workbench_for_a_private_project_reaches_a_site_on_its_profile_without_asking() {
    let mut bench = Bench::with(the_ledger_profile());
    let compartment = Compartment::workbench([&a_ledger_file()]);
    assert_eq!(compartment.label_mark().label(), &Label::private(Category::Finance));
    let authority = bench.authority.as_ref().expect("read");
    assert_eq!(authority.clearance(&Party::Site(registry())), Label::Public);
    let under = standing(authority, &Holder::Workbench("ledger".to_string()), &registry());
    assert_eq!(under.len(), 2);
    assert_eq!(
        connect(&mut bench, &ledger_workbench(), &compartment, Login::None, registry()),
        Decision::Allowed { under }
    );
}

#[test]
fn a_workbench_for_a_private_project_asks_each_time_it_connects_to_a_host_not_on_its_profile() {
    // Attack A31, at the gate. The workbench holds what its profile
    // showed, so it is asked, and not refused. A yes adds the host for
    // that run only (ADR 0017), and the gate keeps nothing of it: after
    // the workbench stops and starts again, the same connection asks.
    let mut bench = Bench::with(the_ledger_profile());
    let first_run = Compartment::workbench([&a_ledger_file()]);
    assert_eq!(
        connect(&mut bench, &ledger_workbench(), &first_run, Login::None, news()),
        asked(AskWhy::Crosses)
    );
    bench.gate.ask_closed(&ledger_workbench(), later(60)).expect("recorded");

    let second_run = Compartment::workbench([&a_ledger_file()]);
    assert_eq!(
        connect(&mut bench, &ledger_workbench(), &second_run, Login::None, news()),
        asked(AskWhy::Crosses)
    );
}

#[test]
fn a_workbench_that_holds_more_than_its_profile_showed_is_refused_at_any_host() {
    // The profile showed finance data. The workbench's view also holds a
    // health summary, which no one approved it to hold.
    let mut bench = Bench::with(the_ledger_profile());
    let compartment = Compartment::workbench([&a_ledger_file(), &a_health_summary()]);
    for host in [registry(), news()] {
        assert_eq!(
            connect(&mut bench, &ledger_workbench(), &compartment, Login::None, host),
            refused(GateRule::Clearance, Limit::Clearance(Label::private(Category::Finance)))
        );
    }
}

#[test]
fn revoking_what_an_approval_showed_takes_back_what_it_cleared() {
    let holder = Holder::Installed("editor".to_string());
    let approved = Bench::with(the_editors_approval());
    let (documents, _) = approved
        .authority
        .as_ref()
        .expect("read")
        .grants(&holder)
        .find(|(_, grant)| grant.shows.is_some())
        .expect("the approval showed the documents");

    let mut changes = the_editors_approval();
    changes.push(Change::Revoked(documents));
    let mut bench = Bench::with(changes);
    let compartment = Compartment::fixed([&personal()]);
    assert_eq!(
        connect(&mut bench, &editor(), &compartment, Login::None, sync_host()),
        refused(GateRule::Clearance, Limit::Clearance(Label::Public))
    );
}

#[test]
fn a_grant_that_has_ended_shows_nothing() {
    // The documents were approved until a time that has passed.
    let mut changes = the_editors_approval();
    let Change::Granted(documents) = &mut changes[0] else {
        panic!("the approval starts with the grant on the documents");
    };
    assert!(documents.shows.is_some());
    documents.limits = EffectLimits::new().until(at(9_000));
    let mut bench = Bench::with(changes);
    let compartment = Compartment::fixed([&personal()]);
    assert_eq!(
        connect(&mut bench, &editor(), &compartment, Login::None, sync_host()),
        refused(GateRule::Clearance, Limit::Clearance(Label::Public))
    );
}

#[test]
fn an_approval_does_not_raise_a_model_providers_clearance() {
    // A model provider is cleared by the person for each category (ADR
    // 0008). The ledger project's profile showed finance data, and the
    // provider is cleared for personal.
    let mut bench = Bench::with(the_ledger_profile());
    let compartment = Compartment::workbench([&a_ledger_file()]);
    assert_eq!(
        model_request(&mut bench, &ledger_workbench(), &compartment),
        refused(GateRule::Clearance, Limit::Clearance(Label::Personal))
    );
}

#[test]
fn with_no_authority_to_read_an_approval_clears_nothing() {
    // The person may have revoked the approval, so nothing is read from it.
    let mut bench = Bench::without_authority();
    let compartment = Compartment::fixed([&personal()]);
    assert_eq!(
        connect(&mut bench, &editor(), &compartment, Login::None, sync_host()),
        refused(GateRule::Clearance, Limit::Clearance(Label::Public))
    );
}

#[test]
fn a_workbench_for_a_public_project_reaches_the_open_internet_without_asking() {
    let mut bench = Bench::new();
    let workbench = Principal::Workbench("website".to_string());
    // A workbench is always marked, and a public project's holds only
    // public data.
    let compartment = Compartment::workbench([]);
    assert_eq!(compartment.label_mark().label(), &Label::Public);
    assert_eq!(
        connect(&mut bench, &workbench, &compartment, Login::None, news()),
        allowed()
    );
}
