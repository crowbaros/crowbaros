//! Handles and the broker's table of them.

use std::collections::{HashMap, HashSet};
use std::fmt;

use crowbar_capability::{
    EffectLimits, HandleId, Issue, Principal, Resource, ResourceKind, Right, Rights, TaskId, Time, Widening,
};
use crowbar_labels::{Compartment, LabelMark};

/// A holder passing a handle on.
#[derive(Debug)]
pub struct Delegation<'a> {
    /// Who passes it on. Only the holder may.
    pub from: &'a Principal,
    /// The delegator's compartment, as the broker recorded it when the
    /// Warden started it: never one the caller supplies. The copy is
    /// something this compartment emits, so it carries the compartment's
    /// label and mark too. A caller that could choose the compartment could
    /// pass on an unmarked copy from a marked one.
    pub compartment: &'a Compartment,
    /// Who receives it. Never the Agent.
    pub to: Principal,
    /// The copy's rights: the original's, or fewer.
    pub rights: Rights,
    /// The copy's limits: within the original's.
    pub limits: EffectLimits,
}

/// Possession of a handle.
///
/// Only the table makes one, and only the table can tell whether one is
/// real. It holds a 128-bit key drawn from the kernel's random source, which
/// is not its [`HandleId`] and is never shown. Across a process boundary,
/// possession is a file descriptor or a portal session, never text.
///
/// A handle cannot be made from its parts outside this crate:
///
/// ```compile_fail,E0451
/// fn forge(id: crowbar_broker::HandleId) -> crowbar_broker::Handle {
///     crowbar_broker::Handle { key: id, id }
/// }
/// ```
///
/// nor copied, so a holder that passes it on has only what the table gives
/// the recipient:
///
/// ```compile_fail,E0599
/// fn copy(handle: &crowbar_broker::Handle) -> crowbar_broker::Handle {
///     handle.clone()
/// }
/// ```
///
/// and the id shown to the Agent is not a handle:
///
/// ```compile_fail,E0308
/// use crowbar_broker::{HandleId, HandleTable, Principal, Right, Time};
/// fn forge(table: &HandleTable, id: HandleId) {
///     table.exercise(&id, &Principal::Agent, Right::Read, Time::from_secs(0));
/// }
/// ```
pub struct Handle {
    key: Key,
    id: HandleId,
}

impl Handle {
    /// The id the ledger and the Agent see.
    pub fn id(&self) -> HandleId {
        self.id
    }
}

/// Shows the id only. The key never appears in a log.
impl fmt::Debug for Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Handle").field(&self.id).finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Key(u128);

/// 128 bits from the kernel's random source. A broker that cannot read it
/// cannot issue anything, so it stops.
fn random_128() -> u128 {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .expect("the broker reads /dev/urandom");
    u128::from_ne_bytes(bytes)
}

#[derive(Debug)]
struct Entry {
    id: HandleId,
    issue: Issue,
    ended: Option<Ending>,
    /// The handle this one was delegated from.
    original: Option<Key>,
}

/// Why a handle stopped working before its time ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    Revoked,
    HolderEnded,
}

/// Why the table refused. Each names the rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// This table did not issue the handle.
    Unknown,
    /// The caller is not the handle's holder.
    NotHolder,
    /// The handle was revoked.
    Revoked,
    /// The holder ended: a task finished, or a Tool exited. A task that has
    /// ended is issued nothing more.
    HolderEnded,
    /// The handle's time limit ended at this time.
    Expired(Time),
    /// The handle this one was delegated from has ended.
    OriginalEnded,
    /// The handle does not carry this right.
    RightNotHeld(Right),
    /// The handle was not issued delegable, or is itself a delegated copy.
    NotDelegable,
    /// A handle never passes to the Agent (ADR 0011).
    ToAgent,
    /// The Agent is never issued a handle it could pass on (ADR 0011).
    DelegableToAgent,
    /// A delegated copy would allow more than the original in this dimension.
    Widens(Widening),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Unknown => f.write_str("the broker did not issue this handle"),
            Refusal::NotHolder => f.write_str("only the holder may use a handle"),
            Refusal::Revoked => f.write_str("the handle was revoked"),
            Refusal::HolderEnded => f.write_str("the holder has ended, and its handles ended with it"),
            Refusal::Expired(until) => write!(f, "the handle's time ran out at {}", until.as_secs()),
            Refusal::OriginalEnded => f.write_str("the handle this was passed on from has ended"),
            Refusal::RightNotHeld(right) => write!(f, "the handle does not carry the right to {right}"),
            Refusal::NotDelegable => f.write_str("the handle may not be passed on"),
            Refusal::ToAgent => f.write_str("no handle passes to the Agent"),
            Refusal::DelegableToAgent => f.write_str("the Agent is never issued a handle it could pass on"),
            Refusal::Widens(widening) => write!(f, "a copy may only narrow the original: {widening}"),
        }
    }
}

impl std::error::Error for Refusal {}

/// A handle the table let its holder use, with what the broker needs to
/// carry out the operation.
#[derive(Debug)]
pub struct Exercised<'a> {
    entry: &'a Entry,
    original: Option<HandleId>,
}

impl Exercised<'_> {
    pub fn id(&self) -> HandleId {
        self.entry.id
    }

    pub fn resource(&self) -> &Resource {
        &self.entry.issue.resource
    }

    pub fn rights(&self) -> &Rights {
        &self.entry.issue.rights
    }

    pub fn limits(&self) -> &EffectLimits {
        &self.entry.issue.limits
    }

    /// The label and mark the reader takes on.
    pub fn label_mark(&self) -> &LabelMark {
        &self.entry.issue.label_mark
    }

    /// For a delegated copy, the handle it was passed on from. A copy's
    /// count, cost, and rate are a ceiling on the copy, and its uses also
    /// count against the original, so passing a handle on never multiplies
    /// what it allows.
    pub fn original(&self) -> Option<HandleId> {
        self.original
    }
}

/// What the table states about a handle from its id: the kind of resource,
/// and the label and mark of what it names (ADR 0008). Nothing here
/// exercises the handle or names its resource.
#[derive(Debug)]
pub struct View<'a> {
    entry: &'a Entry,
}

impl View<'_> {
    pub fn id(&self) -> HandleId {
        self.entry.id
    }

    pub fn kind(&self) -> ResourceKind {
        self.entry.issue.resource.kind()
    }

    pub fn label_mark(&self) -> &LabelMark {
        &self.entry.issue.label_mark
    }
}

/// The broker's table of handles: the source of truth for what each
/// principal holds.
///
/// - **Issuance** makes a handle for one holder. A task that has ended gets
///   none, and nor does a handle whose time has already run out. The Agent
///   may hold a handle, such as a value in a plan, but never one it could
///   pass on: it holds none of its tasks' authority (ADR 0011). The broker
///   issues only for an action the gate has signed; checking that signature
///   comes with the gate.
/// - **Exercise** takes the handle itself, the caller the broker knows from
///   the connection, a right, and the time. It refuses a handle this table
///   did not issue, a caller that is not the holder, a handle that has ended
///   or run out of time, a copy whose original has ended, and a right the
///   handle does not carry.
/// - **Inspection** takes an id and states the kind, label, and mark only.
/// - **Revocation** takes an id, as the ledger names it, and ends the handle
///   at once, with every copy passed on from it.
/// - **The end of a holder** ends every handle it holds, with their copies.
/// - **Delegation** passes on a narrowed copy, under the rules on
///   [`HandleTable::delegate`].
///
/// Records of ended handles stay, so a refusal can name why.
#[derive(Debug, Default)]
pub struct HandleTable {
    entries: HashMap<Key, Entry>,
    keys: HashMap<HandleId, Key>,
    ended_tasks: HashSet<TaskId>,
}

impl HandleTable {
    pub fn new() -> Self {
        HandleTable::default()
    }

    /// Issues a handle to `issue.holder`.
    pub fn issue(&mut self, issue: Issue, now: Time) -> Result<Handle, Refusal> {
        self.admit(issue, None, now)
    }

    /// Passes `handle` on, narrowed. The rules:
    ///
    /// - only the holder may, while the handle is live, and only if it was
    ///   issued delegable;
    /// - never to the Agent, which holds none of its tasks' authority
    ///   (ADR 0011), and never to a task that has ended;
    /// - the copy carries the original's rights or fewer, and limits within
    ///   the original's in every dimension;
    /// - the copy carries the label and mark of what it names joined with
    ///   the delegator's compartment, because it is something that
    ///   compartment emits, so a marked holder cannot pass on an unmarked
    ///   copy;
    /// - the copy is never passed on again, and it ends when the original
    ///   ends: revoked, its holder ended, or its time run out.
    pub fn delegate(&mut self, handle: &Handle, delegation: Delegation<'_>, now: Time) -> Result<Handle, Refusal> {
        let original = self.held_by(handle, delegation.from, now)?;
        if !original.issue.delegable {
            return Err(Refusal::NotDelegable);
        }
        if delegation.to == Principal::Agent {
            return Err(Refusal::ToAgent);
        }
        if !delegation.rights.is_subset(&original.issue.rights) {
            return Err(Refusal::Widens(Widening::Rights));
        }
        delegation
            .limits
            .within(&original.issue.limits)
            .map_err(Refusal::Widens)?;
        let copy = Issue {
            holder: delegation.to,
            resource: original.issue.resource.clone(),
            rights: delegation.rights,
            limits: delegation.limits,
            label_mark: original.issue.label_mark.join(delegation.compartment.label_mark()),
            delegable: false,
            reason: original.issue.reason.clone(),
        };
        self.admit(copy, Some(handle.key), now)
    }

    /// Lets `caller` use `handle` for `right` at `now`, or says why not.
    pub fn exercise(
        &self,
        handle: &Handle,
        caller: &Principal,
        right: Right,
        now: Time,
    ) -> Result<Exercised<'_>, Refusal> {
        let entry = self.held_by(handle, caller, now)?;
        if !entry.issue.rights.contains(right) {
            return Err(Refusal::RightNotHeld(right));
        }
        let original = entry
            .original
            .and_then(|key| self.entries.get(&key))
            .map(|original| original.id);
        Ok(Exercised { entry, original })
    }

    /// What the table states about the handle with this id, if it issued one.
    pub fn inspect(&self, id: &HandleId) -> Option<View<'_>> {
        let entry = self.keys.get(id).and_then(|key| self.entries.get(key))?;
        Some(View { entry })
    }

    /// Revokes the handle with this id. It stops working at once, and so does
    /// every copy passed on from it.
    pub fn revoke(&mut self, id: &HandleId) -> Result<(), Refusal> {
        let key = self.keys.get(id).ok_or(Refusal::Unknown)?;
        let entry = self.entries.get_mut(key).ok_or(Refusal::Unknown)?;
        entry.ended.get_or_insert(Ending::Revoked);
        Ok(())
    }

    /// A task ended, or a Tool, installed software, or a workbench stopped.
    /// Every handle the holder has stops working, and so does every copy
    /// passed on from one. A task never starts again, so it is never issued
    /// another; anything else gets new handles at its next start.
    pub fn end(&mut self, holder: &Principal) {
        for entry in self.entries.values_mut() {
            if &entry.issue.holder == holder {
                entry.ended.get_or_insert(Ending::HolderEnded);
            }
        }
        if let Principal::Task(task) = holder {
            self.ended_tasks.insert(task.clone());
        }
    }

    /// The checks every new handle passes, issued or delegated.
    fn admit(&mut self, issue: Issue, original: Option<Key>, now: Time) -> Result<Handle, Refusal> {
        if matches!(&issue.holder, Principal::Task(task) if self.ended_tasks.contains(task)) {
            return Err(Refusal::HolderEnded);
        }
        if issue.holder == Principal::Agent && issue.delegable {
            return Err(Refusal::DelegableToAgent);
        }
        if let Some(until) = issue.limits.ended_by(now) {
            return Err(Refusal::Expired(until));
        }
        let key = Key(random_128());
        let id = HandleId::from_bits(random_128());
        self.entries.insert(
            key,
            Entry {
                id,
                issue,
                ended: None,
                original,
            },
        );
        self.keys.insert(id, key);
        Ok(Handle { key, id })
    }

    /// The entry for `handle`, if `caller` holds it and it is live at `now`.
    /// A caller that is not the holder learns nothing about the handle's
    /// state.
    fn held_by(&self, handle: &Handle, caller: &Principal, now: Time) -> Result<&Entry, Refusal> {
        let entry = self.entries.get(&handle.key).ok_or(Refusal::Unknown)?;
        if &entry.issue.holder != caller {
            return Err(Refusal::NotHolder);
        }
        match entry.ended {
            Some(Ending::Revoked) => return Err(Refusal::Revoked),
            Some(Ending::HolderEnded) => return Err(Refusal::HolderEnded),
            None => {}
        }
        if let Some(until) = entry.issue.limits.ended_by(now) {
            return Err(Refusal::Expired(until));
        }
        if let Some(key) = entry.original {
            // A copy's time is within its original's, so only an ending can
            // stop the original first. An original the table cannot find
            // has ended too.
            let original = self.entries.get(&key).ok_or(Refusal::OriginalEnded)?;
            if original.ended.is_some() {
                return Err(Refusal::OriginalEnded);
            }
        }
        Ok(entry)
    }
}
