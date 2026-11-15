//! The origin table. The rules are written out on [`Origin`].

use crate::label::{Category, Label, Site};
use crate::mark::{LabelMark, MailAccount, Mark, MarkSource};

/// Where an object came from, as the broker states it.
///
/// The origin table gives each origin its label and mark. No model takes part. The table below is the rule; [`Origin::label_mark`] is
/// the same rule as code, and a test keeps the two the same. Where a row uses an
/// example, `bank.example` is a site the person is logged in to, `news.example`
/// is a site they are not, and `me@mail.example` is one of their mail accounts.
///
/// | Origin | Label | Mark |
/// | --- | --- | --- |
/// | Text the person typed | `personal` | `none` |
/// | Text a fixed template produced | `public` | `none` |
/// | Mail in `me@mail.example` | `private: messages` | `marked: mail me@mail.example` |
/// | A page from `news.example` | `public` | `marked: site news.example` |
/// | A page from `bank.example`, under a login | `private: site bank.example` | `marked: site bank.example` |
/// | A file downloaded from `news.example` | `public` | `marked: site news.example` |
/// | A file saved from an attachment in `me@mail.example` | `private: messages` | `marked: mail me@mail.example` |
/// | A file from outside the fleet, by any other way | `private: unclassified` | `marked: outside` |
/// | The Base package snapshot | `public` | `none` |
/// | The dependency set for build tasks | `public` | `none` |
/// | A paste of a note the person typed | `personal` | `marked: paste` |
/// | A paste from a page of `bank.example`, under a login | `private: site bank.example` | `marked: site bank.example, paste` |
/// | A paste whose source was not recorded | `private: unclassified` | `marked: paste` |
/// | A stored result | as it was stored | as it was stored |
/// | Anything else | `private: unclassified` | `marked: unknown` |
///
/// The rules behind the rows:
///
/// - **The person's own words are personal and unmarked** (ADR 0008). Strings
///   a fixed template produced are the OS's words, and are public.
/// - **Open input marks** (ADR 0011): web pages, mail, files from outside, and
///   what the person pastes. The mark names its source.
/// - **Admitted input does not mark** (ADR 0011). Only the two sets ADR 0011
///   names are admitted. Whether a catalog snapshot is admitted is undecided
///   (ADR 0017), so it is not a row.
/// - **Messages are private by default.** A page fetched under a login is
///   private to that site (ADR 0008). A web site's default is public.
/// - **A file takes the row of where its content came from.** A file written
///   inside the fleet is a stored result.
/// - **A paste carries its source's label and mark** (ADR 0008), and is
///   marked as a paste.
/// - **Where no rule gives a label, the row fails closed**: private, in the
///   `unclassified` category, which no party is cleared for unless the person
///   names it, and marked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Text the person typed in trusted input.
    Typed,
    /// Text a fixed template in the OS produced.
    Template,
    /// Content the person pasted or dropped. `copied_from` is the label and
    /// mark of the compartment it was copied from, when that was recorded.
    Pasted { copied_from: Option<LabelMark> },
    /// A mail message in one of the person's accounts.
    Mail { account: MailAccount },
    /// A web page the broker fetched, with or without a login.
    WebPage { site: Site, login: bool },
    /// A file brought into the fleet.
    File(FileOrigin),
    /// Content from a set the person admitted in advance.
    Admitted(AdmittedSet),
    /// A value read back from storage, with the label and mark it was stored
    /// with.
    Stored(LabelMark),
    /// An origin the table does not know.
    Unknown,
}

/// How a file came into the fleet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOrigin {
    /// Downloaded by the broker from a site.
    Downloaded { site: Site, login: bool },
    /// Saved from an attachment in one of the person's mail accounts.
    Attachment { account: MailAccount },
    /// From outside the fleet by any other way, such as removable media.
    FromOutside,
}

/// The pinned sets ADR 0011 admits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmittedSet {
    /// The Base package snapshot.
    BasePackages,
    /// The dependency set, with its documentation, that build tasks may use.
    BuildDependencies,
}

impl Origin {
    /// The label and mark this origin gives.
    pub fn label_mark(&self) -> LabelMark {
        match self {
            Origin::Typed => LabelMark::new(Label::Personal, Mark::none()),
            Origin::Template | Origin::Admitted(_) => LabelMark::new(Label::Public, Mark::none()),
            Origin::Mail { account } | Origin::File(FileOrigin::Attachment { account }) => LabelMark::new(
                Label::private(Category::Messages),
                Mark::from_source(MarkSource::Mail(account.clone())),
            ),
            Origin::WebPage { site, login } | Origin::File(FileOrigin::Downloaded { site, login }) => {
                let label = if *login {
                    Label::private(Category::Site(site.clone()))
                } else {
                    Label::Public
                };
                LabelMark::new(label, Mark::from_source(MarkSource::Site(site.clone())))
            }
            Origin::File(FileOrigin::FromOutside) => unclassified(MarkSource::Outside),
            Origin::Pasted {
                copied_from: Some(source),
            } => source.join(&LabelMark::new(Label::Public, Mark::from_source(MarkSource::Paste))),
            Origin::Pasted { copied_from: None } => unclassified(MarkSource::Paste),
            Origin::Stored(stored) => stored.clone(),
            Origin::Unknown => unclassified(MarkSource::Unknown),
        }
    }
}

/// The fail-closed row: private, unclassified, and marked.
fn unclassified(source: MarkSource) -> LabelMark {
    LabelMark::new(Label::private(Category::Unclassified), Mark::from_source(source))
}
