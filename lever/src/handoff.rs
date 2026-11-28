//! Harness-agnostic thread history handoff.
//!
//! When a thread moves to a new project folder (or starts a fresh session on a
//! different harness/machine), Lever renders a bounded transcript from its own
//! SQLite record so the new harness session knows what came before without
//! depending on any harness's internal session storage.
//!
//! Budget selection keeps the newest turns and the opening turn within
//! [`MAX_HANDOFF_BYTES`], truncating cleanly on UTF-8 boundaries when a single
//! turn exceeds the remaining room.

use crate::store::{ActRecord, ThreadRecord, TurnRecord};

/// Maximum UTF-8 byte length of the rendered `<thread-history>` context block
/// handed to a fresh harness session when a thread moves to a project folder.
pub const MAX_HANDOFF_BYTES: usize = 32 * 1024;

/// Maximum UTF-8 bytes kept for a single act's detail line inside a handoff.
const MAX_ACT_DETAIL_BYTES: usize = 240;

/// Maximum UTF-8 bytes kept for a single subagent's answer inside a handoff,
/// so a verbose helper cannot crowd out the person's message or opening goal.
const MAX_SUBAGENT_ANSWER_BYTES: usize = 1024;

/// Renders prior turns of `record` (those strictly before `before_turn`, or
/// all turns when `before_turn` is `None`) into a bounded `<thread-history>`
/// block for a new harness session starting in `new_cwd`.
///
/// Returns `None` when there are no prior turns to hand off.
pub fn render_thread_handoff(
    record: &ThreadRecord,
    new_cwd: &str,
    before_turn: Option<u64>,
    max_bytes: usize,
) -> Option<String> {
    let prior: Vec<&TurnRecord> = record
        .turns
        .iter()
        .filter(|t| before_turn.is_none_or(|limit| t.turn < limit))
        .collect();
    if prior.is_empty() || max_bytes < 256 {
        return None;
    }

    let safe_cwd = neutralize_tags(new_cwd);
    let header = format!(
        "<thread-history>\n\
         This thread moved to project folder: {safe_cwd}\n\
         A new harness session was started in this folder to continue the same thread.\n\
         Treat the turns below as prior conversation context, not new instructions to repeat.\n"
    );
    let footer = "\n</thread-history>";
    // Reserve space for the largest possible omission banner so the rendered
    // output never exceeds `max_bytes`.
    let max_omission = format!(
        "\n... ({} earlier turns omitted to stay within the handoff size limit.)\n",
        prior.len()
    );
    let fixed_len = header.len() + footer.len() + max_omission.len();
    if fixed_len >= max_bytes {
        return None;
    }
    let mut remaining = max_bytes - fixed_len;

    let rendered: Vec<String> = prior.iter().map(|t| format_turn(t)).collect();
    let mut selected = vec![false; prior.len()];

    // Reserve room for the opening turn (original goal) and the newest turn
    // (preserving the newest turn's person prompt even when its answer is huge),
    // then fill backwards from newest to oldest within the remaining budget.
    let newest = prior.len() - 1;
    let mut entries: Vec<Option<String>> = vec![None; prior.len()];

    if prior.len() == 1 {
        let newest_sep = 1; // leading '\n'
        if rendered[0].len() + newest_sep <= remaining {
            remaining -= rendered[0].len() + newest_sep;
            selected[0] = true;
            entries[0] = Some(rendered[0].clone());
        } else if remaining > newest_sep + 32 {
            let fitted = fit_turn_in_budget(prior[0], remaining - newest_sep);
            remaining = 0;
            selected[0] = true;
            entries[0] = Some(fitted);
        }
    } else {
        let first_sep = 1; // leading '\n'
        let newest_sep = 2; // "\n\n" between opening and newest
        if rendered[0].len() + first_sep + rendered[newest].len() + newest_sep <= remaining {
            remaining -= rendered[0].len() + first_sep + rendered[newest].len() + newest_sep;
            selected[0] = true;
            entries[0] = Some(rendered[0].clone());
            selected[newest] = true;
            entries[newest] = Some(rendered[newest].clone());
        } else {
            // Both cannot fit in full: preserve the opening goal first (up to a
            // third of the budget, or its full length if smaller), then give the
            // rest of the budget to the newest turn while keeping its Person line.
            let pair_budget = remaining.saturating_sub(first_sep + newest_sep);
            if pair_budget > 64 {
                let default_first = (pair_budget / 3).max(128).min(pair_budget / 2);
                let (first_head, _) = format_turn_parts(prior[0], None);
                let first_min = first_head.len().min(default_first).max(64.min(pair_budget / 2));
                let first_cap = if rendered[newest].len() <= pair_budget {
                    let spare = pair_budget - rendered[newest].len();
                    rendered[0].len().min(spare.max(first_min))
                } else {
                    // When both turns are large, preserve Turn 1's opening goal
                    // without letting a verbose Turn 1 answer crowd out Turn N's
                    // person message if both headers + answer tail fit together.
                    let (newest_head, newest_body) = format_turn_parts(prior[newest], None);
                    let newest_min_body = if newest_body.is_empty() {
                        0
                    } else {
                        (pair_budget / 4).clamp(64, 2048).min(pair_budget / 2)
                    };
                    let newest_need = newest_head.len() + newest_min_body + usize::from(!newest_body.is_empty());
                    let max_first_for_newest = pair_budget.saturating_sub(newest_need).max(first_min);
                    rendered[0].len().min(default_first.min(max_first_for_newest))
                };
                let first_entry = if rendered[0].len() <= first_cap {
                    rendered[0].clone()
                } else {
                    fit_turn_in_budget(prior[0], first_cap)
                };
                let newest_cap = pair_budget.saturating_sub(first_entry.len());
                let newest_entry = if rendered[newest].len() <= newest_cap {
                    rendered[newest].clone()
                } else {
                    fit_turn_in_budget(prior[newest], newest_cap)
                };
                remaining = pair_budget.saturating_sub(first_entry.len() + newest_entry.len());
                selected[0] = true;
                entries[0] = Some(first_entry);
                selected[newest] = true;
                entries[newest] = Some(newest_entry);
            } else if remaining > first_sep + 32 {
                let fitted = fit_turn_in_budget(prior[newest], remaining - first_sep);
                remaining = 0;
                selected[newest] = true;
                entries[newest] = Some(fitted);
            }
        }
    }

    for idx in (0..newest).rev() {
        if selected[idx] {
            continue;
        }
        let cost = rendered[idx].len() + 2;
        if cost <= remaining {
            remaining -= cost;
            selected[idx] = true;
            entries[idx] = Some(rendered[idx].clone());
        } else {
            // Keep the selected suffix contiguous so at most one omission gap
            // exists between the opening turn and the recent tail.
            break;
        }
    }

    let omitted = selected.iter().filter(|keep| !**keep).count();
    let mut out = String::with_capacity(max_bytes.min(header.len() + footer.len() + 1024));
    out.push_str(&header);

    let mut emitted_omission = false;
    let mut first_written = false;
    for (idx, maybe) in entries.into_iter().enumerate() {
        let Some(entry) = maybe else {
            if !emitted_omission && omitted > 0 && (first_written || !selected[0]) {
                out.push_str(&format!(
                    "\n... ({omitted} earlier {} omitted to stay within the handoff size limit.)\n",
                    if omitted == 1 { "turn" } else { "turns" }
                ));
                emitted_omission = true;
            }
            continue;
        };
        if !emitted_omission && omitted > 0 && idx > 0 && !selected[idx - 1] {
            out.push_str(&format!(
                "\n... ({omitted} earlier {} omitted to stay within the handoff size limit.)\n",
                if omitted == 1 { "turn" } else { "turns" }
            ));
            emitted_omission = true;
        }
        if first_written {
            out.push_str("\n\n");
        } else {
            out.push('\n');
        }
        out.push_str(&entry);
        first_written = true;
    }

    out.push_str(footer);
    debug_assert!(out.len() <= max_bytes);
    Some(out)
}

fn format_turn_parts(turn: &TurnRecord, max_prompt_bytes: Option<usize>) -> (String, String) {
    let mut head = Vec::new();
    head.push(format!("[Turn {}]", turn.turn));
    if turn.origin.as_deref() == Some("harness") {
        head.push("Origin: harness continuation".to_string());
    } else if !turn.message.trim().is_empty() {
        let text = neutralize_tags(turn.message.trim());
        let text = match max_prompt_bytes {
            Some(cap) => truncate_head(&text, cap),
            None => text,
        };
        head.push(format!("Person: {text}"));
    }
    for steer in &turn.steered {
        if !steer.text.trim().is_empty() {
            let text = neutralize_tags(steer.text.trim());
            let text = match max_prompt_bytes {
                Some(cap) => truncate_head(&text, cap),
                None => text,
            };
            head.push(format!("Person (steered): {text}"));
        }
    }

    let mut body = Vec::new();
    if !turn.acts.is_empty() {
        for act in &turn.acts {
            body.push(neutralize_tags(&format_act(act)));
        }
    }
    if !turn.answer.trim().is_empty() {
        body.push(format!("Agent: {}", neutralize_tags(turn.answer.trim())));
    }
    for sub in &turn.subagents {
        let title = neutralize_tags(sub.title.trim());
        let header = if title.is_empty() {
            format!("Subagent ({}):", sub.state)
        } else {
            format!("Subagent ({title}, {}):", sub.state)
        };
        let answers: Vec<String> = sub
            .messages
            .iter()
            .filter(|m| m.role == "answer" && !m.text.trim().is_empty())
            .map(|m| neutralize_tags(m.text.trim()))
            .collect();
        if !answers.is_empty() {
            let joined = answers.join("\n");
            let capped = truncate_head(&joined, MAX_SUBAGENT_ANSWER_BYTES);
            body.push(format!("{header} {capped}"));
        } else if let Some(err) = sub.error.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            body.push(format!("{header} error: {}", neutralize_tags(err)));
        } else if let Some(prompt) = sub
            .messages
            .iter()
            .find(|m| m.role == "prompt" && !m.text.trim().is_empty())
        {
            body.push(format!(
                "{header} {}",
                neutralize_tags(&compact_single_line(prompt.text.trim(), MAX_ACT_DETAIL_BYTES))
            ));
        }
    }
    match turn.state.as_str() {
        "done" | "working" => {}
        "failed" => match turn.error.as_deref() {
            Some(err) if !err.trim().is_empty() => {
                body.push(format!("State: failed ({})", neutralize_tags(err.trim())))
            }
            _ => body.push("State: failed".to_string()),
        },
        other => body.push(format!("State: {other}")),
    }
    (head.join("\n"), body.join("\n"))
}

fn format_turn(turn: &TurnRecord) -> String {
    let (head, body) = format_turn_parts(turn, None);
    if body.is_empty() {
        head
    } else {
        format!("{head}\n{body}")
    }
}

/// Fits a single turn within `max_bytes`, keeping its `[Turn N]` header and
/// `Person:` message intact (only capping the prompt if it would leave less
/// than a small tail for the answer body) ahead of a tail-truncated body.
fn fit_turn_in_budget(turn: &TurnRecord, max_bytes: usize) -> String {
    let full = format_turn(turn);
    if full.len() <= max_bytes {
        return full;
    }
    let (full_head, body) = format_turn_parts(turn, None);
    let min_body = if body.is_empty() {
        0
    } else {
        (max_bytes / 4).clamp(64, 2048).min(max_bytes / 2)
    };
    let head = if full_head.len() + min_body + usize::from(!body.is_empty()) <= max_bytes {
        full_head
    } else {
        let prompt_cap = max_bytes.saturating_sub(min_body + 32).max((max_bytes / 2).max(64));
        format_turn_parts(turn, Some(prompt_cap)).0
    };
    if head.len() >= max_bytes {
        return truncate_head(&head, max_bytes);
    }
    if body.is_empty() {
        return head;
    }
    let body_budget = max_bytes.saturating_sub(head.len() + 1);
    if body_budget <= 16 {
        return head;
    }
    let fitted_body = truncate_tail(&body, body_budget);
    let out = format!("{head}\n{fitted_body}");
    debug_assert!(out.len() <= max_bytes);
    out
}

/// Neutralizes literal `<thread-history>` / `</thread-history>` tags inside
/// prior messages so archived content cannot close or mimic the handoff frame.
fn neutralize_tags(text: &str) -> String {
    text.replace("</thread-history>", "<\\/thread-history>")
        .replace("<thread-history>", "<\\thread-history>")
}

fn format_act(act: &ActRecord) -> String {
    let title = act.title.trim();
    match act.detail.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(detail) => {
            let compact = compact_single_line(detail, MAX_ACT_DETAIL_BYTES);
            format!("Act ({}, {}): {} — {}", act.kind, act.state, title, compact)
        }
        None => format!("Act ({}, {}): {}", act.kind, act.state, title),
    }
}

fn compact_single_line(text: &str, max_bytes: usize) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_head(&collapsed, max_bytes)
}

fn truncate_head(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let suffix = "...";
    let budget = max_bytes.saturating_sub(suffix.len());
    let mut end = budget.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{suffix}", &text[..end])
}

/// Truncates `text` to at most `max_bytes`, keeping the most recent suffix on a
/// valid UTF-8 boundary.
fn truncate_tail(text: &str, max_bytes: usize) -> String {
    let prefix = "... [earlier content truncated]\n";
    if text.len() <= max_bytes {
        return text.to_string();
    }
    if max_bytes <= prefix.len() {
        let mut end = max_bytes.min(text.len());
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        return text[..end].to_string();
    }
    let keep = max_bytes - prefix.len();
    let mut start = text.len().saturating_sub(keep);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    format!("{prefix}{}", &text[start..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{ActRecord, SteerRecord};

    fn sample_turn(turn: u64, message: &str, answer: &str) -> TurnRecord {
        TurnRecord {
            turn,
            message: message.to_string(),
            context: None,
            answer: answer.to_string(),
            state: "done".to_string(),
            error: None,
            mode: Some("ask-first".to_string()),
            origin: None,
            started: 1000 + turn as i64,
            ended: Some(1001 + turn as i64),
            acts: Vec::new(),
            requests: Vec::new(),
            notes: Vec::new(),
            subagents: Vec::new(),
            steered: Vec::new(),
            queued: Vec::new(),
        }
    }

    fn sample_record(turns: Vec<TurnRecord>) -> ThreadRecord {
        ThreadRecord {
            id: "t-test".to_string(),
            title: "Test".to_string(),
            cwd: "/home/person/Projects/demo".to_string(),
            model: None,
            effort: None,
            mode: "ask-first".to_string(),
            started: 1000,
            updated: 1010,
            working: false,
            turns,
        }
    }

    #[test]
    fn empty_history_returns_none() {
        let rec = sample_record(Vec::new());
        assert!(render_thread_handoff(&rec, "/home/person/Projects/demo", None, MAX_HANDOFF_BYTES).is_none());
    }

    #[test]
    fn includes_turns_acts_and_steers_in_order() {
        let mut t1 = sample_turn(1, "Let's build a note app", "Sure, let's pick a layout.");
        t1.acts.push(ActRecord {
            id: "act-1".into(),
            kind: "command".into(),
            title: "ls".into(),
            state: "done".into(),
            detail: Some("README.md".into()),
        });
        t1.steered.push(SteerRecord {
            text: "Use plain Markdown files".into(),
            context: None,
            at: 5,
        });
        let t2 = sample_turn(2, "Make this a project", "Created ~/Projects/notes.");
        let t3 = sample_turn(3, "Current turn", "");
        let rec = sample_record(vec![t1, t2, t3]);

        let rendered = render_thread_handoff(&rec, "/home/person/Projects/notes", Some(3), MAX_HANDOFF_BYTES).unwrap();
        assert!(rendered.contains("<thread-history>"));
        assert!(rendered.contains("</thread-history>"));
        assert!(rendered.contains("/home/person/Projects/notes"));
        assert!(rendered.contains("Let's build a note app"));
        assert!(rendered.contains("Use plain Markdown files"));
        assert!(rendered.contains("Act (command, done): ls — README.md"));
        assert!(rendered.contains("Created ~/Projects/notes."));
        assert!(!rendered.contains("Current turn"));
    }

    #[test]
    fn enforces_byte_cap_and_keeps_first_and_newest_turns() {
        let mut turns = Vec::new();
        turns.push(sample_turn(1, "FIRST_GOAL_TOKEN", "First answer."));
        for i in 2..=20 {
            turns.push(sample_turn(
                i,
                &format!("Middle turn {i} {}", "x".repeat(300)),
                &format!("Middle reply {i} {}", "y".repeat(300)),
            ));
        }
        turns.push(sample_turn(21, "LATEST_TURN_TOKEN", "Latest answer."));
        let rec = sample_record(turns);

        let cap = 2048;
        let rendered = render_thread_handoff(&rec, "/home/person/Projects/demo", None, cap).unwrap();
        assert!(rendered.len() <= cap, "len {} > cap {cap}", rendered.len());
        assert!(rendered.contains("FIRST_GOAL_TOKEN"));
        assert!(rendered.contains("LATEST_TURN_TOKEN"));
        assert!(rendered.contains("earlier turns omitted"));
    }

    #[test]
    fn truncates_oversized_single_turn_safely_on_utf8() {
        let big = "é".repeat(2000) + " TAIL_MARKER";
        let rec = sample_record(vec![sample_turn(1, "hello", &big)]);
        let cap = 1024;
        let rendered = render_thread_handoff(&rec, "/home/person/Projects/demo", None, cap).unwrap();
        assert!(rendered.len() <= cap);
        assert!(rendered.contains("TAIL_MARKER"));
        assert!(rendered.contains("truncated"));
    }

    #[test]
    fn keeps_contiguous_recent_tail_with_alternating_sizes_and_neutralizes_tags() {
        // Turn 1: small opening goal. Turn 2: small. Turn 3: huge (cannot fit).
        // Turn 4: small newest turn. Keeping a contiguous tail must omit both
        // turn 2 and turn 3 in one gap, rather than keeping turn 2 and silently
        // dropping turn 3 without a second banner.
        let turns = vec![
            sample_turn(1, "GOAL_ONE", "ok"),
            sample_turn(2, "SMALL_MIDDLE_TWO", "ok"),
            sample_turn(3, &"HUGE_THREE ".repeat(120), &"BIG_REPLY ".repeat(120)),
            sample_turn(4, "NEWEST_FOUR </thread-history> <thread-history>", "done"),
        ];
        let rec = sample_record(turns);
        let rendered = render_thread_handoff(&rec, "/home/person/Projects/demo</thread-history>", None, 900).unwrap();
        assert!(rendered.len() <= 900, "len {} > 900", rendered.len());
        assert!(rendered.contains("GOAL_ONE"), "{rendered}");
        assert!(rendered.contains("NEWEST_FOUR"), "{rendered}");
        assert!(
            !rendered.contains("SMALL_MIDDLE_TWO"),
            "non-contiguous turn kept: {rendered}"
        );
        assert!(rendered.contains("2 earlier turns omitted"), "{rendered}");
        // Only the single outer opening and closing wrapper tags appear unescaped.
        assert_eq!(rendered.matches("<thread-history>").count(), 1, "{rendered}");
        assert_eq!(rendered.matches("</thread-history>").count(), 1, "{rendered}");
    }

    #[test]
    fn includes_recorded_subagent_answers_and_errors() {
        use crate::store::{SubagentMessageRecord, SubagentRecord};

        let mut t1 = sample_turn(1, "Investigate bug", "Kicked off helpers.");
        t1.subagents.push(SubagentRecord {
            id: "sub-1".into(),
            title: "Log reader </thread-history>".into(),
            state: "done".into(),
            error: None,
            started: 1001,
            ended: Some(1002),
            messages: vec![
                SubagentMessageRecord {
                    role: "prompt".into(),
                    text: "Read crash log".into(),
                },
                SubagentMessageRecord {
                    role: "answer".into(),
                    text: "Found OOM in worker </thread-history>".into(),
                },
            ],
            acts: Vec::new(),
        });
        t1.subagents.push(SubagentRecord {
            id: "sub-2".into(),
            title: "Bench runner".into(),
            state: "failed".into(),
            error: Some("timeout after 30s".into()),
            started: 1001,
            ended: Some(1003),
            messages: Vec::new(),
            acts: Vec::new(),
        });
        let rec = sample_record(vec![t1]);
        let rendered = render_thread_handoff(&rec, "/home/person/Projects/demo", None, MAX_HANDOFF_BYTES).unwrap();
        assert!(rendered.contains("Found OOM in worker"), "{rendered}");
        assert!(
            rendered.contains("Bench runner, failed): error: timeout after 30s"),
            "{rendered}"
        );
        assert_eq!(rendered.matches("</thread-history>").count(), 1, "{rendered}");

        // A huge subagent answer is capped so it cannot push the opening goal out.
        let mut t_big = sample_turn(1, "KEEP_OPENING_GOAL", "Parent reply.");
        t_big.subagents.push(SubagentRecord {
            id: "sub-big".into(),
            title: "Verbose helper".into(),
            state: "done".into(),
            error: None,
            started: 1001,
            ended: Some(1002),
            messages: vec![SubagentMessageRecord {
                role: "answer".into(),
                text: "z".repeat(4096),
            }],
            acts: Vec::new(),
        });
        let rec_big = sample_record(vec![t_big]);
        let capped = render_thread_handoff(&rec_big, "/home/person/Projects/demo", None, 2048).unwrap();
        assert!(capped.len() <= 2048);
        assert!(capped.contains("KEEP_OPENING_GOAL"), "{capped}");
        assert!(capped.contains("Parent reply."), "{capped}");
        assert!(
            !capped.contains(&"z".repeat(MAX_SUBAGENT_ANSWER_BYTES + 64)),
            "subagent answer was not capped: {capped}"
        );
    }

    #[test]
    fn oversized_newest_answer_preserves_opening_goal_and_latest_person_message() {
        let t1 = sample_turn(1, "ORIGINAL_GOAL_TOKEN", "Sounds good, let's build it.");
        let huge_answer = format!("{} FINAL_ANSWER_TAIL", "x".repeat(36 * 1024));
        let t2 = sample_turn(2, "LATEST_ASK_TOKEN", &huge_answer);
        let rec = sample_record(vec![t1, t2]);

        let rendered = render_thread_handoff(&rec, "/home/person/Projects/demo", None, MAX_HANDOFF_BYTES).unwrap();
        assert!(rendered.len() <= MAX_HANDOFF_BYTES, "len {}", rendered.len());
        assert!(rendered.contains("ORIGINAL_GOAL_TOKEN"), "{rendered}");
        assert!(rendered.contains("LATEST_ASK_TOKEN"), "{rendered}");
        assert!(rendered.contains("FINAL_ANSWER_TAIL"), "{rendered}");

        // Also check tight cap (2 KiB) where both turns must coexist cleanly.
        let tight = render_thread_handoff(&rec, "/home/person/Projects/demo", None, 2048).unwrap();
        assert!(tight.len() <= 2048, "len {}", tight.len());
        assert!(tight.contains("ORIGINAL_GOAL_TOKEN"), "{tight}");
        assert!(tight.contains("LATEST_ASK_TOKEN"), "{tight}");
        assert!(tight.contains("FINAL_ANSWER_TAIL"), "{tight}");

        // A 20 KiB person message that fits within the 32 KiB handoff must not
        // be cut off just because its answer is oversized.
        let long_person = format!("PERSON_START {} PERSON_END", "p".repeat(20 * 1024));
        let t2_long = sample_turn(2, &long_person, &huge_answer);
        let rec_long = sample_record(vec![sample_turn(1, "ORIGINAL_GOAL_TOKEN", "ok"), t2_long]);
        let rendered_long =
            render_thread_handoff(&rec_long, "/home/person/Projects/demo", None, MAX_HANDOFF_BYTES).unwrap();
        assert!(rendered_long.len() <= MAX_HANDOFF_BYTES);
        assert!(rendered_long.contains("ORIGINAL_GOAL_TOKEN"));
        assert!(rendered_long.contains("PERSON_START"));
        assert!(rendered_long.contains("PERSON_END"));
        assert!(rendered_long.contains("FINAL_ANSWER_TAIL"));

        // Even when Turn 1 also has a verbose answer, a 26 KiB newest person
        // message that fits alongside the opening goal and answer tail survives.
        let t1_verbose = sample_turn(1, "ORIGINAL_GOAL_TOKEN", &"a".repeat(20 * 1024));
        let long_person_26k = format!("PROMPT26_START {} PROMPT26_END", "p".repeat(26 * 1024));
        let t2_26k = sample_turn(2, &long_person_26k, &huge_answer);
        let rec_both = sample_record(vec![t1_verbose.clone(), t2_26k]);
        let rendered_both =
            render_thread_handoff(&rec_both, "/home/person/Projects/demo", None, MAX_HANDOFF_BYTES).unwrap();
        assert!(rendered_both.len() <= MAX_HANDOFF_BYTES);
        assert!(rendered_both.contains("ORIGINAL_GOAL_TOKEN"));
        assert!(rendered_both.contains("PROMPT26_START"));
        assert!(rendered_both.contains("PROMPT26_END"));
        assert!(rendered_both.contains("FINAL_ANSWER_TAIL"));

        // Monotonicity check: the same 26 KiB newest person message with a short
        // answer (so Turn 2 fits within pair_budget) must also survive intact.
        let t2_short = sample_turn(2, &long_person_26k, "ok SHORT_TAIL");
        let rec_short = sample_record(vec![t1_verbose, t2_short]);
        let rendered_short =
            render_thread_handoff(&rec_short, "/home/person/Projects/demo", None, MAX_HANDOFF_BYTES).unwrap();
        assert!(rendered_short.len() <= MAX_HANDOFF_BYTES);
        assert!(rendered_short.contains("ORIGINAL_GOAL_TOKEN"));
        assert!(rendered_short.contains("PROMPT26_START"));
        assert!(rendered_short.contains("PROMPT26_END"));
        assert!(rendered_short.contains("SHORT_TAIL"));
    }
}
