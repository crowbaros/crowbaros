//! What the person granted, and the record of it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use crowbar_broker::{EffectLimits, Resource, Rights, Time};
use crowbar_labels::{Label, MarkSource, Site};
use serde::{Deserialize, Serialize};

use crate::error::{Damage, Error};
use crate::log::{self, Contents, OnDamage, Recorded};

/// The file the authority domain is kept in, inside its directory.
pub(crate) const FILE: &str = "authority.log";

/// What can hold standing authority: a Tool, a standing task, or a
/// workbench. The Agent is not here, and nor is an ad hoc task, so neither
/// can be recorded as holding a standing grant or a standing rule (ADR 0011).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Holder {
    /// A Tool, by its identity in the Crowbar State.
    Tool(String),
    /// A standing task, by the name the person gave it.
    StandingTask(String),
    /// A per-project workbench (ADR 0017).
    Workbench(String),
}

impl fmt::Display for Holder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Holder::Tool(name) => write!(f, "Tool {name}"),
            Holder::StandingTask(name) => write!(f, "standing task {name}"),
            Holder::Workbench(name) => write!(f, "workbench {name}"),
        }
    }
}

/// A standing grant: long-lived access to a thing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub holder: Holder,
    pub resource: Resource,
    pub rights: Rights,
    pub limits: EffectLimits,
    /// Plain-language text from the manifest, as the person saw it.
    pub reason: String,
}

/// A standing rule: the person's pre-approval for a narrow kind of act that
/// crosses the undo boundary, so that act does not ask again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub holder: Holder,
    /// The operation, as its adapter names it.
    pub operation: String,
    /// The rule's limits. A rule always has a rate.
    pub limits: EffectLimits,
    /// The sources of open untrusted input the person wrote this rule for.
    /// A marked task acts under a rule only when the rule is for every
    /// source that marked it (ADR 0011). A rule for no source is for
    /// unmarked work alone.
    pub sources: BTreeSet<MarkSource>,
}

/// An outside party that has a clearance: the highest label it may hold.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Party {
    /// A model provider, by the name the broker knows it by.
    ModelProvider(String),
    /// A web site.
    Site(Site),
}

impl Party {
    /// The clearance of a party the person has set none for: personal for a
    /// model provider, and public for a web site.
    pub fn default_clearance(&self) -> Label {
        match self {
            Party::ModelProvider(_) => Label::Personal,
            Party::Site(_) => Label::Public,
        }
    }
}

/// The id of a standing grant or a standing rule: the place of the entry
/// that recorded it. It is never used twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StandingId(u64);

impl fmt::Display for StandingId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One change to authority, as it is recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    Granted(Grant),
    RuleSet(Rule),
    /// A standing grant or a standing rule was revoked. Nothing brings it
    /// back: granting the same thing again is a new grant, with a new id.
    Revoked(StandingId),
    /// The person set a party's clearance, higher or lower.
    ClearanceSet {
        party: Party,
        clearance: Label,
    },
    /// The person set the ceiling on the limits of tasks that do not cross
    /// the undo boundary.
    CeilingSet(EffectLimits),
}

/// What the person has granted: the authority domain, as it stands.
///
/// This reads and never writes. It is what the gate is given.
#[derive(Debug, Default)]
pub struct Authority {
    history: Vec<Recorded<Change>>,
    grants: BTreeMap<StandingId, Standing<Grant>>,
    rules: BTreeMap<StandingId, Standing<Rule>>,
    clearances: BTreeMap<Party, Label>,
    ceiling: Option<EffectLimits>,
    unfinished_write: Option<usize>,
}

/// A standing grant or rule, kept after it is revoked so the record can say
/// when.
#[derive(Debug)]
struct Standing<T> {
    what: T,
    revoked: Option<Time>,
}

impl Authority {
    /// Reads the authority domain kept in `dir`. A directory that holds none
    /// yet grants nothing.
    ///
    /// A record with a line that cannot be trusted is refused whole, and the
    /// error names the line. Reading around it could leave out a revocation,
    /// so nothing is read and the gate has no authority to allow under.
    pub fn load(dir: &Path) -> Result<Authority, Error> {
        let path = dir.join(FILE);
        Authority::replay(log::read(&path)?, &path)
    }

    /// The authority that the changes in `contents` add up to.
    fn replay(contents: Contents<Change>, path: &Path) -> Result<Authority, Error> {
        contents.first_damage(path)?;
        let mut authority = Authority {
            unfinished_write: contents.unfinished_write,
            ..Authority::default()
        };
        for recorded in contents.entries {
            let line = recorded.seq as usize;
            authority.record(recorded).map_err(|_| Error::Damaged {
                path: path.to_path_buf(),
                line,
                damage: Damage::NotAdmitted,
            })?;
        }
        Ok(authority)
    }

    /// The standing grants `holder` holds: those recorded for it and not
    /// revoked.
    pub fn grants(&self, holder: &Holder) -> impl Iterator<Item = (StandingId, &Grant)> + use<'_> {
        let holder = holder.clone();
        self.grants
            .iter()
            .filter(move |(_, grant)| grant.revoked.is_none() && grant.what.holder == holder)
            .map(|(id, grant)| (*id, &grant.what))
    }

    /// The standing rules `holder` holds: those recorded for it and not
    /// revoked.
    pub fn rules(&self, holder: &Holder) -> impl Iterator<Item = (StandingId, &Rule)> + use<'_> {
        let holder = holder.clone();
        self.rules
            .iter()
            .filter(move |(_, rule)| rule.revoked.is_none() && rule.what.holder == holder)
            .map(|(id, rule)| (*id, &rule.what))
    }

    /// The clearance of `party`: the one the person set last, or the default
    /// for its kind when they have set none.
    pub fn clearance(&self, party: &Party) -> Label {
        match self.clearances.get(party) {
            Some(clearance) => clearance.clone(),
            None => party.default_clearance(),
        }
    }

    /// The ceiling on the limits of tasks that do not cross the undo
    /// boundary, if the person has set one. With none set, nothing here
    /// lets a task start unasked.
    pub fn ceiling(&self) -> Option<&EffectLimits> {
        self.ceiling.as_ref()
    }

    /// The size in bytes of a write a crash cut short, if the reader found
    /// one after the last entry and left it out.
    pub fn unfinished_write(&self) -> Option<usize> {
        self.unfinished_write
    }

    /// When the standing grant or rule with this id was revoked, if it was.
    pub fn revoked_at(&self, id: StandingId) -> Option<Time> {
        self.revocation(id).flatten()
    }

    /// Every change to authority, in the order it was made. Authority has
    /// its own history, and this is all of it.
    pub fn history(&self) -> &[Recorded<Change>] {
        &self.history
    }

    /// Whether the id names a standing grant or rule, and when that was
    /// revoked.
    fn revocation(&self, id: StandingId) -> Option<Option<Time>> {
        let grant = self.grants.get(&id).map(|standing| standing.revoked);
        grant.or_else(|| self.rules.get(&id).map(|standing| standing.revoked))
    }

    /// Whether `change` may follow the changes already recorded. The store
    /// asks before it writes a change down, and again for each change it
    /// reads back.
    fn admits(&self, change: &Change) -> Result<(), Error> {
        match change {
            Change::RuleSet(rule) if rule.limits.how_often().is_none() => Err(Error::RuleWithoutRate),
            Change::Revoked(id) => self.revocation(*id).map(|_| ()).ok_or(Error::NotStanding(*id)),
            Change::Granted(_) | Change::RuleSet(_) | Change::ClearanceSet { .. } | Change::CeilingSet(_) => Ok(()),
        }
    }

    /// Takes in one recorded change.
    fn record(&mut self, recorded: Recorded<Change>) -> Result<(), Error> {
        self.admits(&recorded.entry)?;
        let id = StandingId(recorded.seq);
        match &recorded.entry {
            Change::Granted(grant) => {
                let what = grant.clone();
                self.grants.insert(id, Standing { what, revoked: None });
            }
            Change::RuleSet(rule) => {
                let what = rule.clone();
                self.rules.insert(id, Standing { what, revoked: None });
            }
            Change::Revoked(revoked) => {
                let revoked_at = match self.grants.get_mut(revoked) {
                    Some(grant) => &mut grant.revoked,
                    None => &mut self.rules.get_mut(revoked).expect("admitted").revoked,
                };
                revoked_at.get_or_insert(recorded.time);
            }
            Change::ClearanceSet { party, clearance } => {
                self.clearances.insert(party.clone(), clearance.clone());
            }
            Change::CeilingSet(limits) => self.ceiling = Some(limits.clone()),
        }
        self.history.push(recorded);
        Ok(())
    }
}

/// The authority domain, open for the person's changes.
#[derive(Debug)]
pub struct AuthorityStore {
    log: log::Log,
    authority: Authority,
}

impl AuthorityStore {
    /// Opens the authority domain kept in `dir`, and makes it if it is not
    /// there.
    pub fn open(dir: &Path) -> Result<AuthorityStore, Error> {
        let path = dir.join(FILE);
        let (log, contents) = log::Log::open(&path, OnDamage::Refuse)?;
        let authority = Authority::replay(contents, &path)?;
        Ok(AuthorityStore { log, authority })
    }

    /// The authority domain as it stands, for the gate to read.
    pub fn authority(&self) -> &Authority {
        &self.authority
    }

    /// Records a standing grant.
    pub fn grant(&mut self, grant: Grant, now: Time) -> Result<StandingId, Error> {
        self.change(Change::Granted(grant), now).map(StandingId)
    }

    /// Records a standing rule. A rule always has a rate limit, so one
    /// without is refused.
    pub fn rule(&mut self, rule: Rule, now: Time) -> Result<StandingId, Error> {
        self.change(Change::RuleSet(rule), now).map(StandingId)
    }

    /// Revokes the standing grant or rule with this id. It is no longer held
    /// from the moment this returns, and nothing brings it back.
    ///
    /// Revoking what is already revoked changes nothing and records nothing.
    pub fn revoke(&mut self, id: StandingId, now: Time) -> Result<(), Error> {
        match self.authority.revocation(id) {
            None => Err(Error::NotStanding(id)),
            Some(Some(_)) => Ok(()),
            Some(None) => self.change(Change::Revoked(id), now).map(|_| ()),
        }
    }

    /// Records the clearance the person set for `party`. It replaces the one
    /// before it, higher or lower.
    pub fn set_clearance(&mut self, party: Party, clearance: Label, now: Time) -> Result<(), Error> {
        self.change(Change::ClearanceSet { party, clearance }, now).map(|_| ())
    }

    /// Records the ceiling the person set on the limits of tasks that do not
    /// cross the undo boundary. It replaces the one before it.
    pub fn set_ceiling(&mut self, limits: EffectLimits, now: Time) -> Result<(), Error> {
        self.change(Change::CeilingSet(limits), now).map(|_| ())
    }

    /// Writes one change down, then takes it in. A change the record does
    /// not admit is refused before anything is written.
    fn change(&mut self, change: Change, now: Time) -> Result<u64, Error> {
        self.authority.admits(&change)?;
        let seq = self.log.append(now, &change)?;
        self.authority.record(Recorded {
            seq,
            time: now,
            entry: change,
        })?;
        Ok(seq)
    }
}
