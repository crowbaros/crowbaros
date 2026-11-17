//! What the gate decides on: an operation, as the broker states it.

use std::collections::BTreeSet;

use crowbar_authority::Party;
use crowbar_capability::{Cost, EffectLimits, HandleId, IdempotencyKey, Issue, Principal, Resource, Right, Rights};
use crowbar_labels::{Compartment, LabelMark, LabelStore, ObjectId};
use serde::Serialize;

/// One operation for the gate to decide: who asks, the key they attached,
/// and the act.
///
/// The broker fills every field from its own records. The caller is the
/// principal it knows from the connection, and the compartment is the one
/// the Warden started for it. Nothing here is taken from what the task says.
#[derive(Debug)]
pub struct Operation<'a> {
    pub caller: Caller<'a>,
    /// The idempotency key the task attached, if it attached one.
    pub key: Option<&'a IdempotencyKey>,
    pub act: Act<'a>,
}

/// Who asks, as the broker knows it.
#[derive(Debug)]
pub struct Caller<'a> {
    /// The principal on the connection.
    pub principal: &'a Principal,
    /// For a task, the standing task it is a run of, by the name the person
    /// gave it, as the broker recorded when the task started. An ad hoc task
    /// has none.
    pub standing_task: Option<&'a str>,
    /// Whether the compartment holds a login: a session or a credential the
    /// broker would use for it on some site. With one, a request can act on
    /// the person's account. This is the broker's record, like the rest.
    pub holds_login: bool,
    /// The caller's compartment, as the broker holds it from the Warden's
    /// start: never one the caller supplies.
    pub compartment: &'a Compartment,
}

/// The four operations the gate decides.
#[derive(Debug)]
pub enum Act<'a> {
    /// Issue a new handle. The label store gives the origin of each object
    /// a Data handle names.
    Issue { issue: &'a Issue, labels: &'a LabelStore },
    /// Pass a narrowed copy of a held handle on to `to`.
    Delegate {
        /// The handle presented, as the broker's table states it, or `None`
        /// when the table did not issue it.
        handle: Option<&'a Presented>,
        to: &'a Principal,
        rights: &'a Rights,
        limits: &'a EffectLimits,
    },
    /// An adapter operation on a held handle.
    Use {
        handle: Option<&'a Presented>,
        using: &'a Use,
    },
    /// A connection out of the fleet, to `party`, on a held handle.
    Egress {
        handle: Option<&'a Presented>,
        using: &'a Use,
        party: &'a Party,
    },
}

/// What a use declares, as its adapter states it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Use {
    /// The operation, as its adapter names it: `service.operation`, such as
    /// `mail.send`.
    pub operation: String,
    /// The right the operation needs on the handle.
    pub right: Right,
    /// Which objects it affects. A use that affects no object counts as one
    /// operation.
    pub objects: BTreeSet<ObjectId>,
    pub cost: UseCost,
}

impl Use {
    /// What the use counts against a "how many" limit.
    pub(crate) fn units(&self) -> u64 {
        (self.objects.len() as u64).max(1)
    }
}

/// What a use costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UseCost {
    /// A fixed amount, known before the use.
    Fixed(Cost),
    /// An amount that depends on a value the task cannot read, such as an
    /// amount in a handle. The gate cannot check it. Under a standing rule
    /// with a cap the use is allowed if the amount is within the cap, and
    /// its adapter checks that. Otherwise it asks, and the person sees the
    /// value.
    DependsOnValue,
}

/// What the broker's table states about a handle the caller presented. The
/// gate checks it before anything else: known, held by the caller, live,
/// and not expired.
#[derive(Debug, Clone)]
pub struct Presented {
    pub id: HandleId,
    pub holder: Principal,
    pub resource: Resource,
    pub rights: Rights,
    pub limits: EffectLimits,
    pub label_mark: LabelMark,
    /// Whether it was issued delegable. A copy never is.
    pub delegable: bool,
    /// Why it stopped working, if it has.
    pub ended: Option<Ending>,
    /// For a copy, the handle it was passed on from.
    pub original: Option<Original>,
}

/// Why a handle stopped working before its time ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    Revoked,
    HolderEnded,
}

/// The handle a copy was passed on from. Every use of the copy counts
/// against its limits too.
#[derive(Debug, Clone)]
pub struct Original {
    pub id: HandleId,
    pub limits: EffectLimits,
    /// Whether it has ended: revoked, or its holder ended.
    pub ended: bool,
}

/// What the ledger calls the operations that are not adapter operations.
pub(crate) const ISSUE: &str = "handle.issue";
pub(crate) const DELEGATE: &str = "handle.delegate";

impl Operation<'_> {
    /// The operation's name, as the ledger records it.
    pub fn name(&self) -> &str {
        match &self.act {
            Act::Issue { .. } => ISSUE,
            Act::Delegate { .. } => DELEGATE,
            Act::Use { using, .. } | Act::Egress { using, .. } => &using.operation,
        }
    }

    /// The SHA-256 of what the operation asks for, as 64 hexadecimal digits.
    /// The same key on an operation with a different digest is a different
    /// operation. The compartment is left out: a task that reads more and
    /// then retries is retrying the same operation.
    pub(crate) fn digest(&self) -> String {
        let id = |handle: &Option<&Presented>| handle.map(|handle| handle.id);
        let fingerprint = match &self.act {
            Act::Issue { issue, .. } => Fingerprint::Issue(issue),
            Act::Delegate {
                handle,
                to,
                rights,
                limits,
            } => Fingerprint::Delegate {
                handle: id(handle),
                to,
                rights,
                limits,
            },
            Act::Use { handle, using } => Fingerprint::Use {
                handle: id(handle),
                using,
            },
            Act::Egress { handle, using, party } => Fingerprint::Egress {
                handle: id(handle),
                using,
                party,
            },
        };
        let json = serde_json::to_vec(&fingerprint).expect("an operation is written as JSON");
        let mut text = String::with_capacity(64);
        for byte in <sha2::Sha256 as sha2::Digest>::digest(json) {
            use std::fmt::Write;
            write!(text, "{byte:02x}").expect("writing to a string");
        }
        text
    }
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Fingerprint<'a> {
    Issue(&'a Issue),
    Delegate {
        handle: Option<HandleId>,
        to: &'a Principal,
        rights: &'a Rights,
        limits: &'a EffectLimits,
    },
    Use {
        handle: Option<HandleId>,
        using: &'a Use,
    },
    Egress {
        handle: Option<HandleId>,
        using: &'a Use,
        party: &'a Party,
    },
}
