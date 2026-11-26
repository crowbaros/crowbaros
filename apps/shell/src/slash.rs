//! The commands of the Agent's surface: what a `/` in the message box
//! offers, and what taking one does.
//!
//! The drawer has commands of its own, which do what its controls did, and
//! lists beside them the commands the thread's harness offers. A command of
//! the drawer's that needs a choice, as `/model` does, shows the choices in
//! the same list. A command of the harness's is put in the message box and
//! sent as the thread's next message: the harness takes it for a command or
//! for words.

use crate::agent::{self, AgentState, Provider};

/// A command of the drawer's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Own {
    Model,
    Effort,
    Mode,
    DefaultMode,
    FreeModels,
    SignIn,
    New,
    Threads,
    Stop,
    Archive,
    Delete,
}

impl Own {
    /// Every command of the drawer's, in the order the list has them.
    pub const ALL: [Own; 11] = [
        Own::Model,
        Own::Effort,
        Own::Mode,
        Own::DefaultMode,
        Own::FreeModels,
        Own::SignIn,
        Own::New,
        Own::Threads,
        Own::Stop,
        Own::Archive,
        Own::Delete,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Own::Model => "model",
            Own::Effort => "effort",
            Own::Mode => "mode",
            Own::DefaultMode => "default-mode",
            Own::FreeModels => "free-models",
            Own::SignIn => "sign-in",
            Own::New => "new",
            Own::Threads => "threads",
            Own::Stop => "stop",
            Own::Archive => "archive",
            Own::Delete => "delete",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Own::Model => "Pick this thread's Model",
            Own::Effort => "Pick how hard the Model thinks",
            Own::Mode => "Pick how much the Agent asks before it acts",
            Own::DefaultMode => "Pick the mode new threads start in",
            Own::FreeModels => "Switch OpenCode's free hosted Models on or off",
            Own::SignIn => "Sign in to a Model provider in a terminal",
            Own::New => "Start a new thread",
            Own::Threads => "Go to the list of threads",
            Own::Stop => "Stop the turn the Agent is working on",
            Own::Archive => "Put this thread away, and keep it",
            Own::Delete => "Delete this thread for good",
        }
    }

    /// Whether the command is finished by a choice from a list.
    pub fn takes_choice(self) -> bool {
        matches!(
            self,
            Own::Model | Own::Effort | Own::Mode | Own::DefaultMode | Own::FreeModels
        )
    }

    fn named(name: &str) -> Option<Own> {
        Own::ALL.into_iter().find(|own| own.name() == name)
    }

    /// Whether the list offers the command at all, as the thread now is.
    fn offered(self, state: &AgentState) -> bool {
        match self {
            Own::Stop => state.can_stop(),
            Own::Archive | Own::Delete => state.thread.is_some(),
            _ => true,
        }
    }

    /// Why the command cannot be taken now, when it cannot.
    fn refusal(self, state: &AgentState) -> Option<String> {
        const WORKING: &str = "The Agent is working on this thread. /stop ends the turn.";
        if !state.connected && self != Own::Threads {
            return Some("The Agent is not reachable right now.".into());
        }
        match self {
            Own::Model | Own::Effort | Own::Mode | Own::FreeModels | Own::SignIn if state.no_harness => {
                Some("No harness is available.".into())
            }
            Own::Model | Own::Effort | Own::Mode if state.mode_locked() => Some(WORKING.into()),
            Own::Effort => {
                let model = state.shown_model()?;
                let levels = state.efforts.get(&model)?;
                levels
                    .is_empty()
                    .then(|| format!("{} has no effort levels.", state.model_label(&model)))
            }
            Own::Stop if !state.can_stop() => Some("Nothing is working.".into()),
            Own::Archive | Own::Delete if state.thread.is_none() => {
                Some("This thread has not begun.".into())
            }
            Own::Delete if state.can_stop() => {
                Some("Stop the turn before deleting the thread.".into())
            }
            Own::New | Own::Threads if state.sending || state.loading => {
                Some("One moment: the thread is still opening.".into())
            }
            _ => None,
        }
    }
}

/// What the message box holds, read as a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Typed<'a> {
    /// Words for the Agent, and no command.
    Words,
    /// `/` and the start of a name, with nothing after it yet.
    Name(&'a str),
    /// A name, and what follows it.
    Command { name: &'a str, arg: &'a str },
}

/// Reads the message box. A command is one line that begins with `/` and a
/// name. A path, as `/etc/hosts`, is words.
pub fn typed(text: &str) -> Typed<'_> {
    let Some(rest) = text.strip_prefix('/') else {
        return Typed::Words;
    };
    if text.contains('\n') {
        return Typed::Words;
    }
    let (name, arg) = match rest.find(char::is_whitespace) {
        Some(at) => (&rest[..at], Some(rest[at..].trim())),
        None => (rest, None),
    };
    if name.contains('/') || name.contains('\\') {
        return Typed::Words;
    }
    match arg {
        None => Typed::Name(name),
        Some(_) if name.is_empty() => Typed::Words,
        Some(arg) => Typed::Command { name, arg },
    }
}

/// What a list shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuKind {
    Commands,
    Models,
    Efforts,
    Modes,
    DefaultModes,
    FreeModels,
}

/// What taking an item does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Take {
    /// Put this in the message box: a command that waits for a choice, or
    /// for whatever the person adds before they send it.
    Fill(String),
    Run(Run),
    /// It cannot be taken now, and why.
    Refuse(String),
}

/// What the drawer carries out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Run {
    PickModel(String),
    PickEffort(String),
    PickMode(String),
    PickDefaultMode(String),
    SetFreeModels(bool),
    SignIn,
    New,
    Threads,
    Stop,
    Archive,
    Delete,
}

/// One row of a list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// The control's ID on the shell's control socket: `command:<name>` or
    /// `choice:<value>`.
    pub id: String,
    /// What the row says first: `/name`, or a choice's name.
    pub label: String,
    /// What it says after, more quietly.
    pub detail: String,
    /// Whose command it is, for one of the harness's.
    pub tag: Option<String>,
    /// The choice the thread is at now.
    pub current: bool,
    /// Why it cannot be taken now.
    pub refusal: Option<String>,
    pub take: Take,
}

/// A list above the message box.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Menu {
    pub kind: MenuKind,
    /// What the list is of, for a list of choices.
    pub title: Option<String>,
    pub items: Vec<Item>,
    /// What the list says in place of items: that it is being read, or that
    /// there is nothing to list.
    pub note: Option<String>,
    /// The providers beside a list of Models, and the one whose Models show.
    pub rail: Vec<Provider>,
    pub provider: Option<String>,
    /// What the drawer must ask Lever for before the list is whole.
    pub wants: Option<Wants>,
}

/// What a list needs from Lever.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wants {
    /// The harness's Models and modes.
    Offer,
    /// A Model's effort levels.
    Efforts(String),
}

impl Menu {
    /// The row a new list starts on: the choice the thread is at, else the
    /// first.
    pub fn start(&self) -> usize {
        self.items.iter().position(|item| item.current).unwrap_or(0)
    }

    fn of(kind: MenuKind, title: &str) -> Self {
        Menu {
            kind,
            title: Some(title.to_string()),
            items: Vec::new(),
            note: None,
            rail: Vec::new(),
            provider: None,
            wants: None,
        }
    }
}

/// How well `query` finds `name` and `description`: lower is better, and
/// `None` is no match. A name counts for more than a description, and its
/// start for more than its middle. A `loose` search, as of the commands,
/// also finds a name by its letters in order.
fn score(query: &str, name: &str, description: &str, loose: bool) -> Option<u32> {
    let query = query.to_lowercase();
    if query.is_empty() {
        return Some(0);
    }
    let rank = |text: &str, base: u32| -> Option<u32> {
        let text = text.to_lowercase();
        if text == query {
            Some(base)
        } else if text.starts_with(&query) {
            Some(base + 2)
        } else if text
            .match_indices(&query)
            .any(|(at, _)| at > 0 && !text.as_bytes()[at - 1].is_ascii_alphanumeric())
        {
            Some(base + 4)
        } else if text.contains(&query) {
            Some(base + 6)
        } else {
            None
        }
    };
    // Letters in order, with others between: `dfm` finds `default-mode`.
    let in_order = || {
        let mut letters = name.chars().flat_map(char::to_lowercase);
        query.chars().all(|wanted| letters.any(|c| c == wanted))
    };
    [rank(name, 0), rank(description, 20), (loose && in_order()).then_some(100)]
        .into_iter()
        .flatten()
        .min()
}

/// Keeps the items `query` finds, best first, and in their own order where
/// two are found as well.
fn narrowed(items: Vec<(Item, String)>, query: &str, loose: bool) -> Vec<Item> {
    let mut found: Vec<(u32, usize, Item)> = items
        .into_iter()
        .enumerate()
        .filter_map(|(at, (item, name))| score(query, &name, &item.detail, loose).map(|s| (s, at, item)))
        .collect();
    found.sort_by_key(|(score, at, _)| (*score, *at));
    found.into_iter().map(|(_, _, item)| item).collect()
}

/// Whether `text` is a slash command owned by the drawer itself (not plain
/// words or a harness command). Used so unsent drafts of plain text, paths,
/// and harness commands are preserved while abandoned drawer commands are not.
pub fn is_drawer_command(text: &str) -> bool {
    match typed(text) {
        Typed::Name(name) | Typed::Command { name, .. } => Own::named(name).is_some(),
        Typed::Words => false,
    }
}

/// The list the message box calls for, if it calls for one: the commands
/// while a name is typed, and the choices of a command that takes one.
/// `provider` is the provider picked in the rail of a list of Models.
pub fn menu(text: &str, state: &AgentState, provider: Option<&str>) -> Option<Menu> {
    match typed(text) {
        Typed::Words => None,
        Typed::Name(query) => Some(commands(query, state)),
        Typed::Command { name, arg } => match Own::named(name)? {
            Own::Model => Some(models(arg, state, provider)),
            Own::Effort => Some(efforts(arg, state)),
            Own::Mode => Some(modes(arg, state, false)),
            Own::DefaultMode => Some(modes(arg, state, true)),
            Own::FreeModels => Some(free_models(arg, state)),
            _ => None,
        },
    }
}

fn commands(query: &str, state: &AgentState) -> Menu {
    let mut items: Vec<(Item, String)> = Vec::new();
    for own in Own::ALL.into_iter().filter(|own| own.offered(state)) {
        let refusal = own.refusal(state);
        let take = match &refusal {
            Some(why) => Take::Refuse(why.clone()),
            None if own.takes_choice() => Take::Fill(format!("/{} ", own.name())),
            None => Take::Run(match own {
                Own::SignIn => Run::SignIn,
                Own::New => Run::New,
                Own::Threads => Run::Threads,
                Own::Stop => Run::Stop,
                Own::Archive => Run::Archive,
                _ => Run::Delete,
            }),
        };
        items.push((
            Item {
                id: format!("command:{}", own.name()),
                label: format!("/{}", own.name()),
                detail: own.description().to_string(),
                tag: None,
                current: false,
                refusal,
                take,
            },
            own.name().to_string(),
        ));
    }
    let harness = state.harnesses.first().cloned().unwrap_or_else(|| "Harness".into());
    for command in state.harness_commands() {
        // A command of the drawer's own keeps its name.
        if Own::named(&command.name).is_some() {
            continue;
        }
        let detail = if !command.description.trim().is_empty() {
            command.description.clone()
        } else {
            command.hint.clone().unwrap_or_default()
        };
        items.push((
            Item {
                id: format!("command:{}", command.name),
                label: format!("/{}", command.name),
                detail,
                tag: Some(harness.clone()),
                current: false,
                refusal: None,
                take: Take::Fill(format!("/{} ", command.name)),
            },
            command.name.clone(),
        ));
    }
    let items = narrowed(items, query, true);
    Menu {
        kind: MenuKind::Commands,
        title: None,
        note: items.is_empty().then(|| "No command by that name. Enter sends it to the Agent as it is.".into()),
        items,
        rail: Vec::new(),
        provider: None,
        wants: None,
    }
}

fn choice(value: &str, label: String, detail: String, current: bool, run: Run) -> (Item, String) {
    (
        Item {
            id: format!("choice:{value}"),
            label: label.clone(),
            detail,
            tag: None,
            current,
            refusal: None,
            take: Take::Run(run),
        },
        label,
    )
}

fn models(query: &str, state: &AgentState, provider: Option<&str>) -> Menu {
    let mut menu = Menu::of(MenuKind::Models, "Model");
    if let Some(why) = Own::Model.refusal(state) {
        menu.note = Some(why);
        return menu;
    }
    // Always ask Lever once when `/model` opens so reopening picks up any
    // Models added out-of-band (for example after `crowbar-agent login`).
    menu.wants = Some(Wants::Offer);
    if !state.offer_known {
        if state.models.is_empty() {
            menu.note = Some(
                state
                    .modes_error
                    .clone()
                    .unwrap_or_else(|| "Reading the Models…".into()),
            );
            return menu;
        }
    }
    if state.models.is_empty() {
        menu.note = Some(
            state
                .missing_model
                .clone()
                .unwrap_or_else(|| "The harness offers no Model right now.".into()),
        );
        return menu;
    }
    let current = state.shown_model();
    let rail = agent::providers(&state.models);
    // The rail shows while nothing is searched for, and a search looks
    // through every provider.
    let searching = !query.is_empty();
    let picked = provider
        .filter(|id| rail.iter().any(|p| p.id == *id))
        .map(str::to_string)
        .or_else(|| {
            let of_current = state
                .models
                .iter()
                .find(|m| Some(&m.id) == current.as_ref())
                .map(|m| m.provider.clone().unwrap_or_default());
            of_current.or_else(|| rail.first().map(|p| p.id.clone()))
        });
    let items: Vec<(Item, String)> = state
        .models
        .iter()
        .filter(|m| searching || picked.as_deref() == Some(m.provider.as_deref().unwrap_or("")))
        .map(|m| {
            let detail = if searching {
                m.provider_name.clone().or_else(|| m.provider.clone()).unwrap_or_default()
            } else {
                String::new()
            };
            let (item, _) = choice(
                &m.id,
                m.label().to_string(),
                detail,
                Some(&m.id) == current.as_ref(),
                Run::PickModel(m.id.clone()),
            );
            // A search finds a Model by its name or by its ID.
            (item, format!("{} {}", m.label(), m.id))
        })
        .collect();
    menu.items = narrowed(items, query, false);
    if menu.items.is_empty() {
        menu.note = Some("No Model by that name.".into());
    }
    if !searching && rail.len() > 1 {
        menu.rail = rail;
        menu.provider = picked;
    }
    menu
}

fn efforts(query: &str, state: &AgentState) -> Menu {
    let mut menu = Menu::of(MenuKind::Efforts, "Effort");
    if let Some(why) = Own::Effort.refusal(state) {
        menu.note = Some(why);
        return menu;
    }
    let Some(model) = state.shown_model() else {
        // Which Model a thread starts with is the harness's to say.
        menu.wants = Some(Wants::Offer);
        menu.note = Some(if state.offer_known {
            "The harness offers no Model right now.".into()
        } else {
            "Reading the Models…".into()
        });
        return menu;
    };
    menu.title = Some(format!("Effort · {}", state.model_label(&model)));
    let Some(levels) = state.efforts.get(&model) else {
        menu.wants = Some(Wants::Efforts(model));
        menu.note = Some("Reading the effort levels…".into());
        return menu;
    };
    let current = state.shown_effort();
    let items = levels
        .iter()
        .map(|level| {
            choice(
                &level.id,
                level.name.clone(),
                String::new(),
                Some(&level.id) == current.as_ref(),
                Run::PickEffort(level.id.clone()),
            )
        })
        .collect();
    menu.items = narrowed(items, query, false);
    if menu.items.is_empty() {
        menu.note = Some("No effort by that name.".into());
    }
    menu
}

fn modes(query: &str, state: &AgentState, starting: bool) -> Menu {
    let (kind, title, own) = if starting {
        (MenuKind::DefaultModes, "New threads start in", Own::DefaultMode)
    } else {
        (MenuKind::Modes, "Mode", Own::Mode)
    };
    let mut menu = Menu::of(kind, title);
    if let Some(why) = own.refusal(state) {
        menu.note = Some(why);
        return menu;
    }
    if !state.offer_known {
        menu.wants = Some(Wants::Offer);
        if state.modes.is_empty() {
            menu.note = Some(
                state
                    .modes_error
                    .clone()
                    .unwrap_or_else(|| "Reading the modes…".into()),
            );
            return menu;
        }
    }
    let current = if starting { state.default_mode.clone() } else { state.shown_mode() };
    let items: Vec<(Item, String)> = state
        .modes
        .iter()
        .filter(|mode| !starting || agent::DEFAULT_MODES.contains(&mode.id.as_str()))
        .map(|mode| {
            let name = agent::mode_name(&mode.id, &state.modes);
            let run = if starting {
                Run::PickDefaultMode(mode.id.clone())
            } else {
                Run::PickMode(mode.id.clone())
            };
            let (item, _) = choice(&mode.id, name.clone(), mode_says(&mode.id).into(), Some(&mode.id) == current.as_ref(), run);
            (item, format!("{name} {}", mode.id))
        })
        .collect();
    if items.is_empty() {
        menu.note = Some("This harness offers no modes.".into());
        return menu;
    }
    menu.items = narrowed(items, query, false);
    if menu.items.is_empty() {
        menu.note = Some("No mode by that name.".into());
    }
    menu
}

fn free_models(query: &str, state: &AgentState) -> Menu {
    let mut menu = Menu::of(MenuKind::FreeModels, "OpenCode free hosted Models");
    if let Some(why) = Own::FreeModels.refusal(state) {
        menu.note = Some(why);
        return menu;
    }
    let items = vec![
        choice(
            "on",
            "On".into(),
            "Use OpenCode's free hosted Models (requests go to OpenCode's makers)".into(),
            state.free_hosted_models,
            Run::SetFreeModels(true),
        ),
        choice(
            "off",
            "Off".into(),
            "Use only providers you signed in to".into(),
            !state.free_hosted_models,
            Run::SetFreeModels(false),
        ),
    ];
    menu.items = narrowed(items, query, false);
    if menu.items.is_empty() {
        menu.note = Some("Choose `on` or `off`.".into());
    }
    menu
}

/// What a mode does, in a few words, for the modes the drawer knows.
fn mode_says(id: &str) -> &'static str {
    match id {
        "ask-first" => "Asks before a command or an edit",
        "full-access" => "Asks nothing your account can do",
        "plan" => "Reads and plans, and changes nothing",
        _ => "",
    }
}

/// What Enter does with the message box when no list has a row to take.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolve {
    /// Send it to the Agent: words, or a command the harness is to take.
    Send,
    Run(Run),
    Fill(String),
    Refuse(String),
}

/// Reads a whole typed command, as `/mode plan` or `/stop`. What is no
/// command of the drawer's goes to the Agent as it is.
pub fn resolve(text: &str, state: &AgentState) -> Resolve {
    let (name, arg) = match typed(text) {
        Typed::Words => return Resolve::Send,
        Typed::Name(name) => (name, ""),
        Typed::Command { name, arg } => (name, arg),
    };
    let Some(own) = Own::named(name) else {
        return Resolve::Send;
    };
    if let Some(why) = own.refusal(state) {
        return Resolve::Refuse(why);
    }
    if !own.takes_choice() {
        if !arg.is_empty() {
            return Resolve::Refuse(format!("/{} takes nothing after it.", own.name()));
        }
        return Resolve::Run(match own {
            Own::SignIn => Run::SignIn,
            Own::New => Run::New,
            Own::Threads => Run::Threads,
            Own::Stop => Run::Stop,
            Own::Archive => Run::Archive,
            _ => Run::Delete,
        });
    }
    if arg.is_empty() {
        return Resolve::Fill(format!("/{} ", own.name()));
    }
    // The choice named: by its ID or its name, whole, or else the one
    // choice the words find.
    let Some(menu) = menu(text, state, None) else {
        return Resolve::Send;
    };
    let named = |item: &&Item| {
        item.id.strip_prefix("choice:").is_some_and(|id| id.eq_ignore_ascii_case(arg))
            || item.label.eq_ignore_ascii_case(arg)
    };
    let found = menu.items.iter().find(named).or(match menu.items.as_slice() {
        [only] => Some(only),
        _ => None,
    });
    match found.map(|item| &item.take) {
        Some(Take::Run(run)) => Resolve::Run(run.clone()),
        _ => Resolve::Refuse(match (menu.items.is_empty(), menu.note) {
            (true, Some(note)) => note,
            _ => format!("More than one choice fits “{arg}”."),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{CommandInfo, EffortInfo, ModeInfo, ModelInfo};

    fn model(id: &str, name: &str, provider: &str) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            name: name.into(),
            provider: Some(provider.into()),
            provider_name: Some(provider.to_uppercase()),
        }
    }

    /// A drawer on a new thread, with what a harness offers known.
    fn ready() -> AgentState {
        let mut state = AgentState::new(true);
        state.connected = true;
        state.list_view = false;
        state.harnesses = vec!["OpenCode".into()];
        state.offer_known = true;
        state.modes = ["ask-first", "full-access", "plan"]
            .map(|id| ModeInfo { id: id.into(), name: String::new() })
            .to_vec();
        state.default_mode = Some("ask-first".into());
        state.models = vec![
            model("acme/small", "Small", "acme"),
            model("acme/large", "Large", "acme"),
            model("other/tiny", "Tiny", "other"),
        ];
        state.default_model = Some("acme/small".into());
        state.commands = vec![
            CommandInfo { name: "review".into(), description: "review changes".into(), hint: None },
            CommandInfo { name: "compact".into(), description: "Compact the session".into(), hint: None },
        ];
        state
    }

    fn labels(menu: &Menu) -> Vec<&str> {
        menu.items.iter().map(|item| item.label.as_str()).collect()
    }

    #[test]
    fn a_command_is_one_line_that_begins_with_a_slash_and_a_name() {
        assert_eq!(typed("hello"), Typed::Words);
        assert_eq!(typed(""), Typed::Words);
        assert_eq!(typed("/"), Typed::Name(""));
        assert_eq!(typed("/mo"), Typed::Name("mo"));
        assert_eq!(typed("/mode "), Typed::Command { name: "mode", arg: "" });
        assert_eq!(typed("/mode  plan "), Typed::Command { name: "mode", arg: "plan" });
        assert_eq!(typed("/review the last commit"), Typed::Command { name: "review", arg: "the last commit" });
        // A path is words, and so is a slash with a space after it, or a
        // message of more than one line.
        assert_eq!(typed("/etc/hosts is wrong"), Typed::Words);
        assert_eq!(typed("/ what"), Typed::Words);
        assert_eq!(typed("/mode\nplan"), Typed::Words);
        assert_eq!(typed(" /mode"), Typed::Words);
    }

    #[test]
    fn a_slash_lists_the_drawers_commands_and_then_the_harness_s() {
        let state = ready();
        let menu = menu("/", &state, None).unwrap();
        assert_eq!(menu.kind, MenuKind::Commands);
        // A new thread has nothing to stop, archive, or delete.
        assert_eq!(
            labels(&menu),
            [
                "/model",
                "/effort",
                "/mode",
                "/default-mode",
                "/free-models",
                "/sign-in",
                "/new",
                "/threads",
                "/review",
                "/compact"
            ]
        );
        let review = &menu.items[8];
        assert_eq!((review.tag.as_deref(), review.detail.as_str()), (Some("OpenCode"), "review changes"));
        assert_eq!(review.take, Take::Fill("/review ".into()));
        assert_eq!(review.id, "command:review");
        assert_eq!(menu.items[0].take, Take::Fill("/model ".into()));
        assert_eq!(menu.items[4].take, Take::Fill("/free-models ".into()));
        assert_eq!(menu.items[5].take, Take::Run(Run::SignIn));
        assert_eq!(menu.items[6].take, Take::Run(Run::New));
        assert_eq!(menu.start(), 0);
    }

    #[test]
    fn typing_narrows_the_list_by_name_first_and_then_by_what_a_command_does() {
        let state = ready();
        let found = |text: &str| -> Vec<String> {
            menu(text, &state, None).unwrap().items.into_iter().map(|i| i.label).collect()
        };
        // The start of a name comes before its middle.
        // `/effort` and `/sign-in` are found last, by what they do: they say Model.
        assert_eq!(found("/mo"), ["/model", "/mode", "/default-mode", "/free-models", "/effort", "/sign-in"]);
        assert_eq!(found("/mode"), ["/mode", "/model", "/default-mode", "/free-models", "/effort", "/sign-in"]);
        assert_eq!(found("/MODEL"), ["/model", "/free-models", "/effort", "/sign-in"]);
        // A word after a dash counts as a start, and letters in order find
        // a name with others between.
        assert_eq!(found("/dfm"), ["/default-mode"]);
        // What a command does is looked through too, after the names.
        assert_eq!(found("/session"), ["/compact"]);
        assert_eq!(found("/thread"), ["/threads", "/model", "/default-mode", "/new"]);
        let none = menu("/zzz", &state, None).unwrap();
        assert!(none.items.is_empty());
        assert!(none.note.unwrap().contains("Enter sends it"));
    }

    #[test]
    fn a_thread_offers_what_it_can_take_and_says_why_it_cannot() {
        let mut state = ready();
        state.thread = Some("t-1".into());
        state.mode = Some("ask-first".into());
        let idle = menu("/", &state, None).unwrap();
        assert!(labels(&idle).contains(&"/archive") && labels(&idle).contains(&"/delete"));
        assert!(!labels(&idle).contains(&"/stop"));

        // While it works: Stop is offered, and the thread's settings rest.
        state.sending = true;
        let working = menu("/", &state, None).unwrap();
        let refused: Vec<&str> = working
            .items
            .iter()
            .filter(|item| item.refusal.is_some())
            .map(|item| item.label.as_str())
            .collect();
        assert_eq!(refused, ["/model", "/effort", "/mode", "/new", "/threads"]);
        assert!(matches!(&working.items[0].take, Take::Refuse(why) if why.contains("/stop")));
        assert_eq!(resolve("/mode plan", &state), Resolve::Refuse(Own::Mode.refusal(&state).unwrap()));

        // Out of reach, only the list of threads is at hand.
        state.sending = false;
        state.connected = false;
        let away = menu("/", &state, None).unwrap();
        let own = away.items.iter().filter(|item| item.tag.is_none());
        assert!(own.clone().count() >= 7);
        assert!(own.into_iter().all(|item| item.refusal.is_some() == (item.label != "/threads")));
    }

    #[test]
    fn the_harness_s_commands_are_the_threads_own_once_lever_has_said_them() {
        let mut state = ready();
        state.thread = Some("t-1".into());
        state.thread_commands = Some(vec![
            CommandInfo { name: "deploy".into(), description: String::new(), hint: Some("where to".into()) },
            // One named as a command of the drawer's is the drawer's.
            CommandInfo { name: "new".into(), description: "the harness's".into(), hint: None },
        ]);
        let menu = menu("/", &state, None).unwrap();
        let harness: Vec<(&str, &str)> = menu
            .items
            .iter()
            .filter(|item| item.tag.is_some())
            .map(|item| (item.label.as_str(), item.detail.as_str()))
            .collect();
        assert_eq!(harness, [("/deploy", "where to")]);
    }

    #[test]
    fn model_lists_one_providers_models_beside_a_rail_and_a_search_looks_through_all() {
        let mut state = ready();
        let list = menu("/model ", &state, None).unwrap();
        assert_eq!(list.kind, MenuKind::Models);
        assert_eq!(list.rail.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["ACME", "OTHER"]);
        // It opens on the provider of the Model the thread is at, on that Model.
        assert_eq!(list.provider.as_deref(), Some("acme"));
        assert_eq!(labels(&list), ["Small", "Large"]);
        assert_eq!(list.start(), 0);
        assert_eq!(list.items[1].take, Take::Run(Run::PickModel("acme/large".into())));
        assert_eq!(list.items[1].id, "choice:acme/large");

        let other = menu("/model ", &state, Some("other")).unwrap();
        assert_eq!((other.provider.as_deref(), labels(&other)), (Some("other"), vec!["Tiny"]));

        // A search hides the rail, and says each Model's provider.
        let found = menu("/model ti", &state, Some("acme")).unwrap();
        assert!(found.rail.is_empty());
        assert_eq!((labels(&found), found.items[0].detail.as_str()), (vec!["Tiny"], "OTHER"));
        assert_eq!(labels(&menu("/model acme/l", &state, None).unwrap()), ["Large"]);
        assert_eq!(menu("/model zzz", &state, None).unwrap().note.as_deref(), Some("No Model by that name."));

        // Before Lever has said what the harness offers, the list asks.
        state.offer_known = false;
        state.models.clear();
        let unread = menu("/model ", &state, None).unwrap();
        assert_eq!((unread.wants, unread.note.as_deref()), (Some(Wants::Offer), Some("Reading the Models…")));
        state.offer_known = true;
        state.missing_model = Some("No Model is available.".into());
        assert_eq!(menu("/model ", &state, None).unwrap().note.as_deref(), Some("No Model is available."));
    }

    #[test]
    fn effort_lists_the_levels_of_the_threads_model_and_asks_for_them_once() {
        let mut state = ready();
        let unread = menu("/effort ", &state, None).unwrap();
        assert_eq!(unread.wants, Some(Wants::Efforts("acme/small".into())));
        assert_eq!(unread.title.as_deref(), Some("Effort · Small"));

        state.efforts.insert(
            "acme/small".into(),
            ["low", "high", "default"]
                .map(|id| EffortInfo { id: id.into(), name: id.to_uppercase() })
                .to_vec(),
        );
        state.pending_effort = Some("high".into());
        let list = menu("/effort ", &state, None).unwrap();
        assert_eq!((labels(&list), list.start(), list.wants.clone()), (vec!["LOW", "HIGH", "DEFAULT"], 1, None));
        assert_eq!(resolve("/effort low", &state), Resolve::Run(Run::PickEffort("low".into())));

        // A Model with none says so, in the list of commands too.
        state.efforts.insert("acme/small".into(), Vec::new());
        assert_eq!(menu("/effort ", &state, None).unwrap().note.as_deref(), Some("Small has no effort levels."));
        let commands = menu("/eff", &state, None).unwrap();
        assert!(matches!(&commands.items[0].take, Take::Refuse(why) if why == "Small has no effort levels."));
    }

    #[test]
    fn mode_and_default_mode_list_what_the_harness_offers() {
        let mut state = ready();
        state.thread = Some("t-1".into());
        state.mode = Some("plan".into());
        let list = menu("/mode ", &state, None).unwrap();
        assert_eq!((labels(&list), list.start()), (vec!["Ask first", "Full access", "Plan"], 2));
        assert_eq!(list.items[0].detail, "Asks before a command or an edit");
        assert_eq!(labels(&menu("/mode fu", &state, None).unwrap()), ["Full access"]);
        // New threads start in Ask first or Full access alone.
        let starting = menu("/default-mode ", &state, None).unwrap();
        assert_eq!((labels(&starting), starting.start()), (vec!["Ask first", "Full access"], 0));
        assert_eq!(starting.items[1].take, Take::Run(Run::PickDefaultMode("full-access".into())));

        state.modes.clear();
        assert_eq!(menu("/mode ", &state, None).unwrap().note.as_deref(), Some("This harness offers no modes."));
        state.offer_known = false;
        assert_eq!(menu("/mode ", &state, None).unwrap().wants, Some(Wants::Offer));
    }

    #[test]
    fn a_command_typed_whole_needs_no_list() {
        let mut state = ready();
        state.thread = Some("t-1".into());
        state.mode = Some("ask-first".into());
        assert_eq!(resolve("hello there", &state), Resolve::Send);
        assert_eq!(resolve("/mode plan", &state), Resolve::Run(Run::PickMode("plan".into())));
        assert_eq!(resolve("/mode Full Access", &state), Resolve::Run(Run::PickMode("full-access".into())));
        // The one choice the words find is the choice.
        assert_eq!(resolve("/mode fu", &state), Resolve::Run(Run::PickMode("full-access".into())));
        assert_eq!(resolve("/model other/tiny", &state), Resolve::Run(Run::PickModel("other/tiny".into())));
        assert_eq!(resolve("/default-mode full-access", &state), Resolve::Run(Run::PickDefaultMode("full-access".into())));
        assert_eq!(resolve("/mode", &state), Resolve::Fill("/mode ".into()));
        assert_eq!(resolve("/new", &state), Resolve::Run(Run::New));
        assert_eq!(resolve("/threads", &state), Resolve::Run(Run::Threads));
        assert_eq!(resolve("/archive", &state), Resolve::Run(Run::Archive));
        assert_eq!(resolve("/delete", &state), Resolve::Run(Run::Delete));
        // What cannot be carried out says why, and changes nothing.
        assert_eq!(resolve("/mode nope", &state), Resolve::Refuse("No mode by that name.".into()));
        assert_eq!(resolve("/model a", &state), Resolve::Refuse("More than one choice fits “a”.".into()));
        assert_eq!(resolve("/stop", &state), Resolve::Refuse("Nothing is working.".into()));
        assert_eq!(resolve("/new now", &state), Resolve::Refuse("/new takes nothing after it.".into()));
        // What is no command of the drawer's is the harness's to read.
        assert_eq!(resolve("/review the last commit", &state), Resolve::Send);
        assert_eq!(resolve("/nothing-known", &state), Resolve::Send);
        assert_eq!(resolve("/etc/hosts is wrong", &state), Resolve::Send);
        assert!(menu("/review the last commit", &state, None).is_none());
    }

    #[test]
    fn only_drawer_owned_commands_are_excluded_from_drafts_and_empty_catalog_retries() {
        assert!(is_drawer_command("/mode"));
        assert!(is_drawer_command("/mode plan"));
        assert!(is_drawer_command("/model "));
        assert!(is_drawer_command("/delete"));
        assert!(!is_drawer_command("/review focus on lock screen"));
        assert!(!is_drawer_command("/rev"));
        assert!(!is_drawer_command("/etc/hosts is wrong"));
        assert!(!is_drawer_command("/mode\nplan"));
        assert!(!is_drawer_command("hello"));

        let mut state = ready();
        state.models.clear();
        state.offer_known = true;
        let picker = menu("/model ", &state, None).unwrap();
        assert_eq!(picker.wants, Some(Wants::Offer));
        assert_eq!(picker.note.as_deref(), Some("The harness offers no Model right now."));
    }

    #[test]
    fn free_models_and_sign_in_commands_list_and_resolve() {
        let mut state = ready();
        let list = menu("/free-models ", &state, None).unwrap();
        assert_eq!(list.kind, MenuKind::FreeModels);
        assert_eq!(labels(&list), ["On", "Off"]);
        assert_eq!(list.start(), 1);
        assert_eq!(resolve("/free-models on", &state), Resolve::Run(Run::SetFreeModels(true)));
        assert_eq!(resolve("/free-models off", &state), Resolve::Run(Run::SetFreeModels(false)));
        assert_eq!(resolve("/sign-in", &state), Resolve::Run(Run::SignIn));
        assert!(is_drawer_command("/free-models on"));
        assert!(is_drawer_command("/sign-in"));

        state.free_hosted_models = true;
        assert_eq!(menu("/free-models ", &state, None).unwrap().start(), 0);

        state.no_harness = true;
        assert!(matches!(resolve("/sign-in", &state), Resolve::Refuse(_)));
        assert!(matches!(resolve("/free-models on", &state), Resolve::Refuse(_)));
    }
}
