//! What the person granted, and the record of it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use crowbar_capability::{Cost, EffectLimits, Resource, Rights, Time};
use crowbar_labels::{Label, MarkSource, Site};
use serde::{Deserialize, Serialize};

use crate::chain::{self, Chain, Contents, OnDamage, Recorded};
use crate::error::{Damage, Error};

/// The file the authority domain is kept in, inside its directory.
const FILE: &str = "changes";

/// What can hold standing authority: a Tool, a standing task, a workbench,
/// or installed software. The Agent is not here, and nor is an ad hoc task,
/// so neither can be recorded as holding a standing grant or a standing rule
/// (ADR 0011).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Holder {
    /// A Tool, by its identity in the Crowbar State.
    Tool(String),
    /// A standing task, by the name the person gave it.
    StandingTask(String),
    /// A per-project workbench (ADR 0017).
    Workbench(String),
    /// Installed software. Its approval screen is its standing grant, and
    /// its standing rule for what that screen showed (ADR 0017).
    Installed(String),
}

impl fmt::Display for Holder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Holder::Tool(name) => write!(f, "Tool {name}"),
            Holder::StandingTask(name) => write!(f, "standing task {name}"),
            Holder::Workbench(name) => write!(f, "workbench {name}"),
            Holder::Installed(name) => write!(f, "installed software {name}"),
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
    /// The label of what this grant gives, as the approval showed it. The
    /// join over a holder's grants is the most its approval showed it
    /// holding, and the gate checks its compartment against that when it
    /// reaches a site (ADR 0021). A grant with none is stored without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shows: Option<Label>,
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
    /// The cap: the most one act under the rule may cost, such as "pay the
    /// electric bill, up to $150". With a cap, an act whose cost depends on
    /// a value the task cannot read crosses under the rule without an ask,
    /// and its adapter checks the amount against the cap when it carries
    /// the act out (ADR 0021). A rule with none is stored without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap: Option<Cost>,
}

/// An outside party that has a clearance: the highest label it may hold.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Party {
    /// A model provider, by the name the broker knows it by.
    ModelProvider(String),
    /// A web site: a host on the open internet. A machine in the fleet and
    /// an address on the home network are not sites, and the broker never
    /// names one as a site (ADR 0011).
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
    RuleAdded(Rule),
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
    held: T,
    revoked: Option<Time>,
}

impl<T> Standing<T> {
    fn status(&self) -> Status {
        match self.revoked {
            Some(time) => Status::Revoked(time),
            None => Status::Held,
        }
    }
}

/// Whether a standing grant or rule is still held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Held,
    /// Revoked at this time. Nothing brings it back.
    Revoked(Time),
}

/// Those of `all` that `holder` holds: recorded for it and not revoked.
fn held_by<'a, T>(
    all: &'a BTreeMap<StandingId, Standing<T>>,
    holder: &Holder,
    holder_of: fn(&T) -> &Holder,
) -> impl Iterator<Item = (StandingId, &'a T)> + use<'a, T> {
    let holder = holder.clone();
    all.iter()
        .filter(move |(_, standing)| standing.revoked.is_none() && holder_of(&standing.held) == &holder)
        .map(|(id, standing)| (*id, &standing.held))
}

impl Authority {
    /// Reads the authority domain kept in `dir`, as it stands at this
    /// moment. A change made after the read is not in it.
    ///
    /// A record with a line that cannot be trusted is refused whole, and the
    /// error names the line. Reading around it could leave out a revocation,
    /// so nothing is read and the gate has no authority to allow under. A
    /// directory with no record is refused too: a record that is missing is
    /// not an empty one.
    pub fn load(dir: &Path) -> Result<Authority, Error> {
        let path = dir.join(FILE);
        match chain::read(&path)? {
            Some(contents) => Authority::replay(contents, &path),
            None => Err(Error::Missing { path }),
        }
    }

    /// The authority that `changes` add up to, held in memory and kept
    /// nowhere: for a rehearsal on a simulated clock, and for tests. Each
    /// change is admitted as it would be when read from a record, so one the
    /// changes before it do not admit is refused.
    pub fn from_changes(changes: impl IntoIterator<Item = (Time, Change)>) -> Result<Authority, Error> {
        let mut authority = Authority::default();
        for (seq, (time, entry)) in (1..).zip(changes) {
            authority.record(Recorded { seq, time, entry })?;
        }
        Ok(authority)
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
        held_by(&self.grants, holder, |grant| &grant.holder)
    }

    /// The standing rules `holder` holds: those recorded for it and not
    /// revoked.
    pub fn rules(&self, holder: &Holder) -> impl Iterator<Item = (StandingId, &Rule)> + use<'_> {
        held_by(&self.rules, holder, |rule| &rule.holder)
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

    /// Whether the standing grant or rule with this id is still held, or
    /// `None` when no grant or rule has the id.
    pub fn status(&self, id: StandingId) -> Option<Status> {
        let grant = self.grants.get(&id).map(Standing::status);
        grant.or_else(|| self.rules.get(&id).map(Standing::status))
    }

    /// Every change to authority, in the order it was made. Authority has
    /// its own history, and this is all of it.
    pub fn history(&self) -> &[Recorded<Change>] {
        &self.history
    }

    /// Whether `change` may follow the changes already recorded. The store
    /// asks before it writes a change down, and again for each change it
    /// reads back.
    fn admits(&self, change: &Change) -> Result<(), Error> {
        match change {
            Change::RuleAdded(rule) if rule.limits.how_often().is_none() => Err(Error::RuleWithoutRate),
            Change::Revoked(id) => self.status(*id).map(|_| ()).ok_or(Error::NotStanding(*id)),
            Change::Granted(_) | Change::RuleAdded(_) | Change::ClearanceSet { .. } | Change::CeilingSet(_) => Ok(()),
        }
    }

    /// Takes in one recorded change.
    fn record(&mut self, recorded: Recorded<Change>) -> Result<(), Error> {
        self.admits(&recorded.entry)?;
        let id = StandingId(recorded.seq);
        match &recorded.entry {
            Change::Granted(grant) => {
                let held = grant.clone();
                self.grants.insert(id, Standing { held, revoked: None });
            }
            Change::RuleAdded(rule) => {
                let held = rule.clone();
                self.rules.insert(id, Standing { held, revoked: None });
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
    chain: Chain,
    authority: Authority,
}

impl AuthorityStore {
    /// Makes the authority domain in `dir`, with nothing granted. A machine
    /// does this once. Where there is already a record, none is made.
    pub fn create(dir: &Path) -> Result<AuthorityStore, Error> {
        let path = dir.join(FILE);
        match path.try_exists() {
            Ok(false) => AuthorityStore::at(path),
            Ok(true) => Err(Error::AlreadyThere { path }),
            Err(error) => Err(Error::Io { path, error }),
        }
    }

    /// Opens the authority domain kept in `dir`. One writer has it open at a
    /// time. A directory with no record is refused: a record that is missing
    /// is not an empty one.
    pub fn open(dir: &Path) -> Result<AuthorityStore, Error> {
        let path = dir.join(FILE);
        match path.try_exists() {
            Ok(true) => AuthorityStore::at(path),
            Ok(false) => Err(Error::Missing { path }),
            Err(error) => Err(Error::Io { path, error }),
        }
    }

    fn at(path: PathBuf) -> Result<AuthorityStore, Error> {
        let (chain, contents) = Chain::open(&path, OnDamage::Refuse)?;
        let authority = Authority::replay(contents, &path)?;
        Ok(AuthorityStore { chain, authority })
    }

    /// The authority domain as it stands, for the gate to read.
    ///
    /// After a change that failed to reach the disk, this is refused until
    /// the store is opened again: the record may hold that change, such as
    /// a revocation, and what is held here does not.
    pub fn authority(&self) -> Result<&Authority, Error> {
        self.chain.usable()?;
        Ok(&self.authority)
    }

    /// Records a standing grant.
    pub fn grant(&mut self, grant: Grant, now: Time) -> Result<StandingId, Error> {
        self.change(Change::Granted(grant), now).map(StandingId)
    }

    /// Records a standing rule. A rule always has a rate limit, so one
    /// without is refused.
    pub fn add_rule(&mut self, rule: Rule, now: Time) -> Result<StandingId, Error> {
        self.change(Change::RuleAdded(rule), now).map(StandingId)
    }

    /// Revokes the standing grant or rule with this id. It is no longer held
    /// from the moment this returns, and nothing brings it back.
    ///
    /// Revoking what is already revoked changes nothing and records nothing.
    pub fn revoke(&mut self, id: StandingId, now: Time) -> Result<(), Error> {
        match self.authority.status(id) {
            None => Err(Error::NotStanding(id)),
            Some(Status::Revoked(_)) => Ok(()),
            Some(Status::Held) => self.change(Change::Revoked(id), now).map(|_| ()),
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
        let seq = self.chain.append(now, &change)?;
        self.authority.record(Recorded {
            seq,
            time: now,
            entry: change,
        })?;
        Ok(seq)
    }
}
