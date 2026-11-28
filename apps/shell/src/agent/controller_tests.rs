// SPDX-License-Identifier: MIT
//! End-to-end in-process tests for the `AgentController` MVU reducer.
//! Every test drives the real `AgentController` state machine directly with
//! `AgentIntent` and `AgentEvent` and asserts on the declarative `AgentEffect`
//! stream — zero GPUI windows, sockets, or global state required.

use super::controller::{AgentController, AgentEffect, AgentIntent, FocusTarget, ThreadAction};
use super::types::{
    AgentCommand, AgentEvent, CommandInfo, DirEntry, DirListing, EffortInfo, Mark, ModeInfo,
    ModelInfo, Offer, Request, RequestAnswer, RequestState, StatusInfo, ThreadSummary, Turn,
    TurnState,
};

fn status(free_hosted_models: bool, models_chosen: bool) -> StatusInfo {
    StatusInfo {
        lever_version: "0.2.0".into(),
        harness_started: true,
        harnesses: vec!["opencode".into()],
        free_hosted_models,
        models_chosen,
        default_mode: Some("ask-first".into()),
    }
}

fn connected_controller() -> AgentController {
    let mut ctrl = AgentController::new(true, None, None);
    let _ = ctrl.handle_event(AgentEvent::Connected {
        status: status(true, true),
        resumed: None,
        preserved_thread: None,
        resume_error: None,
    });
    ctrl
}

fn sample_offer() -> Offer {
    Offer {
        supported: true,
        modes: vec![
            ModeInfo {
                id: "ask-first".into(),
                name: "Ask first".into(),
            },
            ModeInfo {
                id: "full-access".into(),
                name: "Full access".into(),
            },
        ],
        models: vec![
            ModelInfo {
                id: "acme/small".into(),
                name: "Small".into(),
                provider: Some("acme".into()),
                provider_name: Some("Acme".into()),
            },
            ModelInfo {
                id: "acme/large".into(),
                name: "Large".into(),
                provider: Some("acme".into()),
                provider_name: Some("Acme".into()),
            },
            ModelInfo {
                id: "beta/pro".into(),
                name: "Pro".into(),
                provider: Some("beta".into()),
                provider_name: Some("Beta".into()),
            },
        ],
        default_model: Some("acme/small".into()),
        free_hosted_models: true,
        steering: true,
    }
}

fn has_command(effects: &[AgentEffect], pred: impl Fn(&AgentCommand) -> bool) -> bool {
    effects
        .iter()
        .any(|e| matches!(e, AgentEffect::SendLeverCommand(cmd) if pred(cmd)))
}

#[test]
fn send_and_reconcile_draft_clears_exact_and_keeps_typed_ahead_suffix() {
    let mut ctrl = AgentController::new(true, Some("acme/large".into()), Some("high".into()));
    let _ = ctrl.handle_event(AgentEvent::Connected {
        status: status(true, true),
        resumed: None,
        preserved_thread: None,
        resume_error: None,
    });
    let _ = ctrl.handle_intent(AgentIntent::DrawerOpened);
    let _ = ctrl.handle_intent(AgentIntent::Action(ThreadAction::New));

    // Type a draft and submit via Enter.
    let _ = ctrl.handle_intent(AgentIntent::DraftChanged("explain this".into()));
    let effects = ctrl.handle_intent(AgentIntent::Enter);
    assert!(ctrl.state.sending);
    assert!(has_command(&effects, |cmd| matches!(
        cmd,
        AgentCommand::Send {
            thread: None,
            text,
            model,
            effort,
            ..
        } if text == "explain this"
            && model.as_deref() == Some("acme/large")
            && effort.as_deref() == Some("high")
    )));

    // User types ahead before Lever acknowledges Sent.
    let _ = ctrl.handle_intent(AgentIntent::DraftChanged(
        "explain this\nand also check tests".into(),
    ));
    let effects = ctrl.handle_event(AgentEvent::Sent {
        thread: "t-1".into(),
        turn: 1,
    });
    assert!(effects.contains(&AgentEffect::SetComposerText("and also check tests".into())));
    assert_eq!(ctrl.current_draft(), "and also check tests");
    assert_eq!(
        ctrl.drafts.get("t-1").map(String::as_str),
        Some("and also check tests")
    );

    // Send a second message on the same thread without typing ahead; Sent clears composer.
    let _ = ctrl.handle_event(AgentEvent::Done {
        thread: "t-1".into(),
        turn: 1,
    });
    let _ = ctrl.handle_intent(AgentIntent::Enter);
    let effects = ctrl.handle_event(AgentEvent::Sent {
        thread: "t-1".into(),
        turn: 2,
    });
    assert!(effects.contains(&AgentEffect::SetComposerText(String::new())));
    assert_eq!(ctrl.current_draft(), "");
    assert!(!ctrl.drafts.contains_key("t-1"));
}

#[test]
fn disconnect_clears_in_flight_submitted_so_later_draft_is_not_wiped() {
    let mut ctrl = connected_controller();
    let _ = ctrl.handle_intent(AgentIntent::DrawerOpened);
    let _ = ctrl.handle_intent(AgentIntent::Action(ThreadAction::New));
    let _ = ctrl.handle_intent(AgentIntent::DraftChanged("hello".into()));
    let _ = ctrl.handle_intent(AgentIntent::Enter);

    let _ = ctrl.handle_event(AgentEvent::Disconnected);
    assert!(!ctrl.state.connected);
    // Even if a stray Sent arrived later, nothing was submitted across reconnect.
    let effects = ctrl.handle_event(AgentEvent::Sent {
        thread: "t-1".into(),
        turn: 1,
    });
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, AgentEffect::SetComposerText(_)))
    );
    assert_eq!(ctrl.current_draft(), "hello");
}

#[test]
fn onboarding_focuses_surface_handles_sign_in_and_moves_focus_to_composer_when_ready() {
    let mut ctrl = AgentController::new(true, None, None);
    let _ = ctrl.handle_event(AgentEvent::Connected {
        status: status(false, false),
        resumed: None,
        preserved_thread: None,
        resume_error: None,
    });
    assert!(ctrl.state.needs_model_choice());

    // Opening drawer during onboarding focuses the surface, not the composer.
    let effects = ctrl.handle_intent(AgentIntent::DrawerOpened);
    assert!(effects.contains(&AgentEffect::Focus(FocusTarget::Surface)));

    // Sign in launches the helper and surfaces errors if launch fails.
    let effects = ctrl.handle_intent(AgentIntent::Action(ThreadAction::SignIn));
    assert!(ctrl.state.sign_in_launched);
    assert!(effects.contains(&AgentEffect::LaunchSignIn));

    let _ = ctrl.handle_intent(AgentIntent::SignInLaunchResult(Err(
        "no terminal available".into(),
    )));
    assert!(!ctrl.state.sign_in_launched);
    assert_eq!(ctrl.state.error.as_deref(), Some("no terminal available"));

    // Choosing free models sends SetFreeModels.
    let effects = ctrl.handle_intent(AgentIntent::Action(ThreadAction::ChooseFreeModels(true)));
    assert!(has_command(&effects, |cmd| matches!(
        cmd,
        AgentCommand::SetFreeModels { enabled: true }
    )));

    // Once Lever confirms free models, onboarding finishes and focus moves to Composer.
    let effects = ctrl.handle_event(AgentEvent::FreeModels {
        enabled: true,
        models_chosen: Some(true),
    });
    assert!(!ctrl.state.needs_model_choice());
    assert!(!ctrl.state.list_view);
    assert!(effects.contains(&AgentEffect::Focus(FocusTarget::Composer)));
    assert!(effects.contains(&AgentEffect::SyncMarkOverlay));
}

#[test]
fn slash_menu_fetch_guards_retry_on_catalog_epoch_and_model_pick_chains_into_effort() {
    // Start with a cold harness (`harness_started: false`) so `/model ` drives
    // the on-demand FetchModes request instead of eager startup prefetch.
    let mut ctrl = AgentController::new(true, None, None);
    let _ = ctrl.handle_event(AgentEvent::Connected {
        status: StatusInfo {
            lever_version: "0.2.0".into(),
            harness_started: false,
            harnesses: vec!["opencode".into()],
            free_hosted_models: true,
            models_chosen: true,
            default_mode: Some("ask-first".into()),
        },
        resumed: None,
        preserved_thread: None,
        resume_error: None,
    });
    let _ = ctrl.handle_intent(AgentIntent::DrawerOpened);
    let _ = ctrl.handle_intent(AgentIntent::Action(ThreadAction::New));

    // Typing "/" opens slash menu and fetches commands once.
    let effects = ctrl.handle_intent(AgentIntent::DraftChanged("/".into()));
    assert!(effects.contains(&AgentEffect::SyncComposerMenu {
        list_open: true,
        rail_open: false,
    }));
    assert!(has_command(&effects, |cmd| matches!(
        cmd,
        AgentCommand::FetchCommands { thread: None }
    )));

    // Narrowing to "/mo" does not re-fetch commands.
    let effects = ctrl.handle_intent(AgentIntent::DraftChanged("/mo".into()));
    assert!(!has_command(&effects, |cmd| matches!(
        cmd,
        AgentCommand::FetchCommands { .. }
    )));

    // Typing "/model " fetches modes/models once.
    let effects = ctrl.handle_intent(AgentIntent::DraftChanged("/model ".into()));
    assert!(has_command(&effects, |cmd| matches!(
        cmd,
        AgentCommand::FetchModes { refresh: false }
    )));

    // Typing "/model l" does not re-fetch modes.
    let effects = ctrl.handle_intent(AgentIntent::DraftChanged("/model l".into()));
    assert!(!has_command(&effects, |cmd| matches!(
        cmd,
        AgentCommand::FetchModes { .. }
    )));

    // Clearing the menu while models are still empty resets asked_offer so retrying fetches again.
    let _ = ctrl.handle_intent(AgentIntent::DraftChanged(String::new()));
    let effects = ctrl.handle_intent(AgentIntent::DraftChanged("/model ".into()));
    assert!(has_command(&effects, |cmd| matches!(
        cmd,
        AgentCommand::FetchModes { refresh: false }
    )));

    // Offer arrives with two providers: rail opens, and StepProvider switches providers.
    let effects = ctrl.handle_event(AgentEvent::Offer(sample_offer()));
    assert!(effects.contains(&AgentEffect::SyncComposerMenu {
        list_open: true,
        rail_open: true,
    }));
    let menu = ctrl.menu().expect("model menu open");
    assert_eq!(menu.provider.as_deref(), Some("acme"));

    let _ = ctrl.handle_intent(AgentIntent::StepProvider(1));
    let menu = ctrl.menu().expect("model menu open");
    assert_eq!(menu.provider.as_deref(), Some("beta"));

    // Escape dismisses the menu without erasing the draft; next edit re-opens it.
    let _ = ctrl.handle_intent(AgentIntent::Dismiss);
    assert!(ctrl.menu().is_none());
    assert_eq!(ctrl.current_draft(), "/model ");

    // Edit draft to filter for "large" and pick it via TakeMenuRow.
    let _ = ctrl.handle_intent(AgentIntent::DraftChanged("/model large".into()));
    let effects = ctrl.handle_intent(AgentIntent::TakeMenuRow);
    assert!(effects.contains(&AgentEffect::SavePicks {
        model: Some("acme/large".into()),
        effort: None,
    }));
    assert!(has_command(&effects, |cmd| matches!(
        cmd,
        AgentCommand::FetchEfforts { model } if model == "acme/large"
    )));
    assert_eq!(ctrl.state.pending_model.as_deref(), Some("acme/large"));

    // When Lever replies with multiple efforts for that model, controller opens "/effort ".
    let effects = ctrl.handle_event(AgentEvent::Efforts {
        model: "acme/large".into(),
        efforts: vec![
            EffortInfo {
                id: "low".into(),
                name: "Low".into(),
            },
            EffortInfo {
                id: "high".into(),
                name: "High".into(),
            },
        ],
    });
    assert!(effects.contains(&AgentEffect::SetComposerText("/effort ".into())));
    assert_eq!(ctrl.current_draft(), "/effort ");

    // Catalog epoch bump (from a Status event) resets fetch guards.
    let _ = ctrl.handle_event(AgentEvent::Commands {
        thread: None,
        commands: vec![CommandInfo {
            name: "review".into(),
            description: "Review changes".into(),
            hint: None,
        }],
        known: true,
    });
    let _ = ctrl.handle_event(AgentEvent::Status(status(true, true)));
    let effects = ctrl.handle_intent(AgentIntent::DraftChanged("/".into()));
    assert!(has_command(&effects, |cmd| matches!(
        cmd,
        AgentCommand::FetchCommands { thread: None }
    )));
}

#[test]
fn project_form_tracks_place_browses_dirs_validates_and_closes_on_project_made() {
    let mut ctrl = connected_controller();
    let _ = ctrl.handle_intent(AgentIntent::DrawerOpened);
    let _ = ctrl.handle_intent(AgentIntent::Action(ThreadAction::New));
    let _ = ctrl.handle_event(AgentEvent::Sent {
        thread: "t-1".into(),
        turn: 1,
    });
    let _ = ctrl.handle_event(AgentEvent::Done {
        thread: "t-1".into(),
        turn: 1,
    });

    // Open project form.
    let effects = ctrl.handle_intent(AgentIntent::Action(ThreadAction::AskMakeProject));
    assert!(ctrl.project.making);
    assert!(effects.contains(&AgentEffect::SetProjectNameText(String::new())));
    assert!(effects.contains(&AgentEffect::SetProjectFolderText("~/Projects".into())));
    assert!(effects.contains(&AgentEffect::Focus(FocusTarget::ProjectName)));

    // Typing a name updates the folder because `place` is attached.
    let effects = ctrl.handle_intent(AgentIntent::ProjectNameChanged("notes".into()));
    assert!(effects.contains(&AgentEffect::SetProjectFolderText(
        "~/Projects/notes".into()
    )));

    // Toggling browse sends ListDirs and Dirs populates listing + home.
    let effects = ctrl.handle_intent(AgentIntent::Action(ThreadAction::ToggleProjectBrowse));
    assert!(ctrl.project.browse_open);
    assert!(has_command(&effects, |cmd| matches!(
        cmd,
        AgentCommand::ListDirs { path } if path.as_deref() == Some("~/Projects/notes")
    )));
    let _ = ctrl.handle_event(AgentEvent::Dirs(DirListing {
        path: "/home/person/Projects".into(),
        home: "/home/person".into(),
        parent: Some("/home/person".into()),
        entries: vec![DirEntry {
            name: "existing".into(),
            path: "/home/person/Projects/existing".into(),
        }],
    }));
    assert_eq!(ctrl.project.browse_home.as_deref(), Some("/home/person"));
    assert_eq!(ctrl.project.browse_entries.len(), 1);

    // Invalid name fails validation without sending MakeProject.
    let _ = ctrl.handle_intent(AgentIntent::ProjectNameChanged("bad/name".into()));
    let effects = ctrl.handle_intent(AgentIntent::Action(ThreadAction::ConfirmMakeProject));
    assert!(ctrl.project.error.is_some());
    assert!(!has_command(&effects, |cmd| matches!(
        cmd,
        AgentCommand::MakeProject { .. }
    )));

    // Valid name sends MakeProject; ProjectMade closes form and focuses Composer.
    let _ = ctrl.handle_intent(AgentIntent::ProjectNameChanged("notes".into()));
    let effects = ctrl.handle_intent(AgentIntent::Action(ThreadAction::ConfirmMakeProject));
    assert!(ctrl.project.making);
    assert!(has_command(&effects, |cmd| matches!(
        cmd,
        AgentCommand::MakeProject {
            thread,
            name,
            place: None,
            path: None,
        } if thread == "t-1" && name.as_deref() == Some("notes")
    )));

    let effects = ctrl.handle_event(AgentEvent::ProjectMade {
        thread: "t-1".into(),
        cwd: "/home/person/Projects/notes".into(),
        created: true,
    });
    assert!(!ctrl.project.making);
    assert!(effects.contains(&AgentEffect::Focus(FocusTarget::Composer)));
    assert!(effects.contains(&AgentEffect::SyncAgentView));

    // Re-open and verify manual folder edit detaches `place`.
    let _ = ctrl.handle_intent(AgentIntent::Action(ThreadAction::AskMakeProject));
    let _ = ctrl.handle_intent(AgentIntent::ProjectFolderChanged("/srv/work".into()));
    assert!(ctrl.project.place.is_none());
    let effects = ctrl.handle_intent(AgentIntent::ProjectNameChanged("ignored".into()));
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, AgentEffect::SetProjectFolderText(_)))
    );
}

#[test]
fn mark_transitions_and_drawer_lifecycle_emit_overlay_keyboard_and_reset_ephemeral_ui() {
    let mut ctrl = connected_controller();
    let _ = ctrl.handle_intent(AgentIntent::DrawerOpened);
    let _ = ctrl.handle_intent(AgentIntent::Action(ThreadAction::New));
    let _ = ctrl.handle_intent(AgentIntent::DraftChanged("run task".into()));
    let _ = ctrl.handle_intent(AgentIntent::Enter);
    let _ = ctrl.handle_intent(AgentIntent::DrawerClosed);
    assert!(!ctrl.drawer_up);

    // Turn working & completing behind a closed drawer drives Mark transitions.
    let effects = ctrl.handle_event(AgentEvent::Sent {
        thread: "t-1".into(),
        turn: 1,
    });
    let _ = ctrl.handle_event(AgentEvent::Message {
        thread: "t-1".into(),
        turn: 1,
        text: "run task".into(),
        context: None,
        from_harness: false,
    });
    assert_eq!(ctrl.state.mark, Mark::Working);
    assert!(effects.contains(&AgentEffect::LogMarkChange {
        prev: Mark::Idle,
        next: Mark::Working,
    }));
    assert!(effects.contains(&AgentEffect::SyncMarkOverlay));
    assert!(effects.contains(&AgentEffect::RefreshWindows));

    let effects = ctrl.handle_event(AgentEvent::Done {
        thread: "t-1".into(),
        turn: 1,
    });
    assert_eq!(ctrl.state.mark, Mark::Done);
    assert!(effects.contains(&AgentEffect::LogMarkChange {
        prev: Mark::Working,
        next: Mark::Done,
    }));

    // Opening drawer acknowledges Done -> Idle and takes keyboard.
    let effects = ctrl.handle_intent(AgentIntent::DrawerOpened);
    assert_eq!(ctrl.state.mark, Mark::Idle);
    assert!(effects.contains(&AgentEffect::LogMarkChange {
        prev: Mark::Done,
        next: Mark::Idle,
    }));
    assert!(effects.contains(&AgentEffect::AgentKeyboard(true)));
    assert!(effects.contains(&AgentEffect::ScrollToBottom));

    // Toggle ephemeral UI and type a draft; closing drawer resets ephemeral UI but keeps draft.
    let _ = ctrl.handle_intent(AgentIntent::DraftChanged("keep me".into()));
    let _ = ctrl.handle_intent(AgentIntent::Action(ThreadAction::Acts(1)));
    assert!(ctrl.opened_turns.contains(&1));
    let _ = ctrl.handle_intent(AgentIntent::Action(ThreadAction::AskDelete));
    assert!(ctrl.confirm_delete);

    let effects = ctrl.handle_intent(AgentIntent::DrawerClosed);
    assert!(!ctrl.drawer_up);
    assert!(effects.contains(&AgentEffect::AgentKeyboard(false)));
    assert!(ctrl.opened_turns.is_empty());
    assert!(!ctrl.confirm_delete);
    assert_eq!(ctrl.drafts.get("t-1").map(String::as_str), Some("keep me"));

    // Re-opening drawer restores the saved draft.
    let effects = ctrl.handle_intent(AgentIntent::DrawerOpened);
    assert!(effects.contains(&AgentEffect::SetComposerText("keep me".into())));
}

#[test]
fn thread_switching_preserves_drafts_and_alt_digit_answers_waiting_request() {
    use super::types::ResumedThread;

    let mut ctrl = connected_controller();
    let _ = ctrl.handle_intent(AgentIntent::DrawerOpened);
    ctrl.state.threads = vec![
        ThreadSummary {
            thread: "t-1".into(),
            title: "First".into(),
            cwd: "/home/person".into(),
            model: None,
            mode: None,
            started: 1,
            updated: 2,
            turns: 1,
            state: "done".into(),
            working: false,
            waiting: false,
            subagents: 0,
            archived: false,
        },
        ThreadSummary {
            thread: "t-2".into(),
            title: "Second".into(),
            cwd: "/home/person".into(),
            model: None,
            mode: None,
            started: 1,
            updated: 3,
            turns: 1,
            state: "waiting".into(),
            working: false,
            waiting: true,
            subagents: 0,
            archived: false,
        },
    ];

    // Open t-1, let Lever finish resuming it, and type a draft.
    let effects = ctrl.handle_intent(AgentIntent::Action(ThreadAction::Open("t-1".into())));
    assert!(has_command(&effects, |cmd| matches!(
        cmd,
        AgentCommand::Select { thread } if thread.as_deref() == Some("t-1")
    )));
    let _ = ctrl.handle_event(AgentEvent::Resumed(ResumedThread {
        thread: "t-1".into(),
        title: "First".into(),
        cwd: "/home/person".into(),
        model: None,
        effort: None,
        mode: None,
        working: false,
        turns: Vec::new(),
    }));
    let _ = ctrl.handle_intent(AgentIntent::DraftChanged("draft one".into()));

    // Switch to t-2: draft one is saved, composer resets to empty.
    let effects = ctrl.handle_intent(AgentIntent::Action(ThreadAction::Open("t-2".into())));
    assert!(effects.contains(&AgentEffect::SetComposerText(String::new())));
    assert_eq!(ctrl.drafts.get("t-1").map(String::as_str), Some("draft one"));

    // Lever resumes t-2 with a waiting permission request; Alt+2 answers with "always".
    let _ = ctrl.handle_event(AgentEvent::Resumed(ResumedThread {
        thread: "t-2".into(),
        title: "Second".into(),
        cwd: "/home/person".into(),
        model: None,
        effort: None,
        mode: None,
        working: true,
        turns: vec![Turn {
            turn: 1,
            message: "run".into(),
            context: None,
            answer: String::new(),
            acts: Vec::new(),
            notes: Vec::new(),
            requests: vec![Request {
                id: "req-1".into(),
                act: None,
                kind: "command".into(),
                title: "Run cargo test".into(),
                detail: "cargo test".into(),
                path: None,
                answers: vec![
                    RequestAnswer {
                        answer: "once".into(),
                        name: "Allow once".into(),
                    },
                    RequestAnswer {
                        answer: "always".into(),
                        name: "Always allow".into(),
                    },
                ],
                state: RequestState::Waiting,
                answer: None,
                sending: None,
                subagent: None,
            }],
            mode: None,
            steered: Vec::new(),
            queued: Vec::new(),
            subagents: Vec::new(),
            state: TurnState::Working,
            started: Some(1),
            ended: None,
            from_harness: false,
        }],
    }));

    // Ignored when confirm_delete is open.
    let _ = ctrl.handle_intent(AgentIntent::Action(ThreadAction::AskDelete));
    let effects = ctrl.handle_intent(AgentIntent::AnswerWaiting(1));
    assert!(effects.is_empty());

    // Cancel delete, then Alt+2 (0-based index 1) answers with "always".
    let _ = ctrl.handle_intent(AgentIntent::Action(ThreadAction::CancelDelete));
    let effects = ctrl.handle_intent(AgentIntent::AnswerWaiting(1));
    assert!(has_command(&effects, |cmd| matches!(
        cmd,
        AgentCommand::Answer {
            thread,
            request,
            answer,
        } if thread == "t-2" && request == "req-1" && answer == "always"
    )));

    // Switching back to t-1 restores "draft one".
    let effects = ctrl.handle_intent(AgentIntent::Action(ThreadAction::Open("t-1".into())));
    assert!(effects.contains(&AgentEffect::SetComposerText("draft one".into())));
    let _ = ctrl.handle_event(AgentEvent::Resumed(ResumedThread {
        thread: "t-1".into(),
        title: "First".into(),
        cwd: "/home/person".into(),
        model: None,
        effort: None,
        mode: None,
        working: false,
        turns: Vec::new(),
    }));

    // Deleting t-1 emits Lifecycle(delete: true), and Lifecycle event returns to list view & focuses Surface.
    let effects = ctrl.handle_intent(AgentIntent::Action(ThreadAction::Delete));
    assert!(has_command(&effects, |cmd| matches!(
        cmd,
        AgentCommand::Lifecycle { thread, delete: true } if thread == "t-1"
    )));
    let effects = ctrl.handle_event(AgentEvent::Lifecycle {
        thread: "t-1".into(),
        deleted: true,
        warning: None,
    });
    assert!(ctrl.state.list_view);
    assert!(effects.contains(&AgentEffect::Focus(FocusTarget::Surface)));
}
