// Pi Durable, and the database it keeps. Everything of Pi Durable's that the
// Harness starts is started here, so nothing outside this file names it
// (ADR 0034). What this file gives the rest is in CrowbarOS's own terms:
// conversations with IDs of our own, and turns counted from 1.
import { randomInt } from "node:crypto";
import { chmodSync, closeSync, openSync } from "node:fs";
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import type { Models } from "@earendil-works/pi-ai/models";
import {
	type ConversationId,
	type ConversationView,
	ConversationBusy,
	configure,
	createRegistry,
	defineDoc,
	defineExtension,
	Harness,
	section,
} from "@earendil-works/pi-durable";
import { openNodeSqliteStorage } from "@earendil-works/pi-durable/storage/sqlite/node";

/** The version of Pi Durable in the bundle, which bundle.mjs reads from the installed package. */
declare const PI_DURABLE_VERSION: string;
export const DURABLE_VERSION: string = PI_DURABLE_VERSION;

/** The Model a turn is answered by: a provider of pi-ai's and one of its models. */
export interface ModelRef {
	provider: string;
	model: string;
}

/** One message from the person and the Agent's answer to it. */
export interface Turn {
	/** Counted from 1 within its conversation. */
	turn: number;
	/** What the person said. */
	message: string;
	/** The answer so far, or the whole of it once the turn has ended. */
	answer: string;
	/** `working` until the turn ends: with the whole answer, stopped by the person, or failed. */
	state: "working" | "done" | "stopped" | "failed";
	/** Why a failed turn failed, in the Model's or its provider's words. */
	error?: string;
	/** While the Model's last try failed and another waits: why, and when the next is made, in milliseconds since 1970. */
	retry?: { error: string; at: number };
}

/** A conversation, as a list of conversations shows it. */
export interface Summary {
	conversation: string;
	/** Seconds since 1970. */
	started: number;
	/** How the first message begins, or nothing before there is one. */
	title: string;
	turns: number;
	working: boolean;
}

/** A conversation as it stands: its newest turns, and whether it has turns before them. */
export interface Standing {
	turns: Turn[];
	earlier: boolean;
}

export interface Store {
	/** Starts a conversation, and gives its ID. */
	start(): Promise<string>;
	/** Every conversation, the newest first. */
	list(): Promise<Summary[]>;
	/**
	 * A conversation as it stands, and then `changed` each time it changes,
	 * until `stop`. Undefined when there is no such conversation.
	 */
	follow(conversation: string, changed: (standing: Standing) => void): Promise<(Standing & { stop(): void }) | undefined>;
	/**
	 * Sends the person's message, to be answered by `model`. Gives the turn
	 * it began, and `ended`, which resolves with how that turn ended; or
	 * `working` when the turn before it has not ended; or undefined when
	 * there is no such conversation.
	 */
	send(conversation: string, text: string, model: ModelRef): Promise<{ turn: number; ended: Promise<Turn["state"]> } | "working" | undefined>;
	/** Stops the turn that is working, and resolves once it has. False when there is no such conversation. */
	stop(conversation: string): Promise<boolean>;
	/** Closes the database, once everything committed is written to it. */
	close(): Promise<void>;
}

/** What the Harness keeps of each conversation, by the conversation's own ID. */
type Kept = {
	conversations: {
		[conversation: string]: {
			/** Pi Durable's conversation. */
			at: number;
			/** Seconds since 1970. */
			started: number;
			title: string;
			turns: number;
		};
	};
};

const Conversations = defineDoc<Kept>({
	kind: "crowbar.conversations",
	version: 1,
	scope: "session",
	initial: () => ({ conversations: {} }),
	// Kept whole every 64 changes, so opening it never replays more.
	checkpointWhen: (_value, _changes, stored) => stored.deltasSinceBase >= 64,
});

/** The kind of the entry a message from the person is kept as. Its data is `{ turn }`. */
const MESSAGE = "crowbar.message";

/**
 * What the Agent is told of itself before every conversation. It has no act
 * yet, and says so when asked for one.
 */
const STANDING = [
	"You are the Agent built into CrowbarOS, a small operating system on Void Linux.",
	"You are at the side of the person who uses this machine, and you talk with them in plain text.",
	"You cannot yet read files, run commands, or act on the desktop. When the person asks for one of those, say so plainly, and help with what you know.",
].join(" ");

/** The letters and digits of a conversation's ID: none that reads as another. */
const ID_ALPHABET = "23456789abcdefghjkmnpqrstuvwxyz";

function newId(): string {
	return Array.from({ length: 8 }, () => ID_ALPHABET[randomInt(ID_ALPHABET.length)]).join("");
}

/** The text of a message's content, without what is not text. */
function textOf(content: unknown): string {
	if (typeof content === "string") return content;
	if (!Array.isArray(content)) return "";
	return content.map((part: { type?: unknown; text?: unknown }) => (part.type === "text" && typeof part.text === "string" ? part.text : "")).join("");
}

interface Answer {
	content?: unknown;
	stopReason?: string;
	errorMessage?: string;
}

/**
 * A conversation's turns, from Pi Durable's view of it: each message of the
 * person's, the answers after it, and the answer still being made.
 */
function standing(view: ConversationView): Standing {
	const turns: (Turn & { last?: Answer })[] = [];
	for (const entry of view.entries) {
		const current = turns.at(-1);
		const first = entry.model?.[0] as (Answer & { role?: string }) | undefined;
		if (entry.kind === MESSAGE) {
			const turn = (entry.data as { turn?: unknown } | undefined)?.turn;
			turns.push({ turn: typeof turn === "number" ? turn : 0, message: textOf(first?.content), answer: "", state: "done" });
		} else if (entry.kind === "pi.assistant" && current && first) {
			current.answer += textOf(first.content);
			current.last = first;
		}
	}
	const live = view.docs["pi.live"] as
		| { run?: object; generation?: { message?: Answer; retry?: { at: number; error: string } }; runFailure?: { reason: string; detail?: string } }
		| undefined;
	for (const [index, turn] of turns.entries()) {
		const newest = index === turns.length - 1;
		if (newest && live?.run) {
			turn.state = "working";
			turn.answer += textOf(live.generation?.message?.content);
			if (live.generation?.retry) turn.retry = { error: live.generation.retry.error, at: live.generation.retry.at };
		} else if (turn.last?.stopReason === "aborted") {
			turn.state = "stopped";
		} else if (turn.last?.stopReason === "error") {
			turn.state = "failed";
			turn.error = turn.last.errorMessage ?? "the Model gave no answer";
		} else if (turn.last === undefined) {
			// Ended with no answer at all: stopped before the Model said
			// anything, or no Model to ask.
			const failure = newest ? live?.runFailure : undefined;
			if (failure === undefined || failure.reason === "aborted") turn.state = "stopped";
			else {
				turn.state = "failed";
				turn.error = failure.reason === "no_model" ? "the Model it was sent to is not there" : (failure.detail ?? failure.reason);
			}
		}
		delete turn.last;
	}
	return { turns, earlier: view.before !== undefined };
}

/**
 * Opens the database at `path`, making it if there is none, and starts Pi
 * Durable on it, reaching Models through `models`. The file is the person's
 * alone: SQLite gives the files it keeps beside a database that database's
 * permissions.
 */
export async function openStore(path: string, models: Models): Promise<Store> {
	closeSync(openSync(path, "a", 0o600));
	chmodSync(path, 0o600);
	const registry = createRegistry();
	registry.install(defineExtension({ name: "crowbaros", sections: [section("crowbaros", () => STANDING, { tag: false })] }));
	const context = BACKGROUND_CONTEXT;
	const harness = await Harness.open(await openNodeSqliteStorage(path), { models, registry, identity: { attribution: null } }, context);
	// Goes on with whatever turn the last Harness left unfinished.
	harness.resume();

	/** Pi Durable's conversation for one of ours. */
	const find = async (conversation: string) => {
		const kept = (await harness.snapshot(Conversations, context))?.conversations[conversation];
		return kept === undefined ? undefined : harness.conversation(kept.at as ConversationId, context);
	};

	return {
		async start() {
			return harness.commit(async (tx) => {
				const kept = await tx.doc(Conversations);
				let id = newId();
				while (kept.conversations[id] !== undefined) id = newId();
				const made = await tx.createConversation({ ownership: { kind: "ownerless" } });
				kept.conversations[id] = { at: made.id, started: Math.floor(Date.now() / 1000), title: "", turns: 0 };
				return id;
			}, context);
		},

		async list() {
			const kept = (await harness.snapshot(Conversations, context))?.conversations ?? {};
			const busy = new Set((await harness.inspect(context)).tasks.map((task) => Number(task.record.conversationId)));
			return Object.entries(kept)
				.map(([conversation, { at, started, title, turns }]) => ({ conversation, started, title, turns, working: busy.has(at), at }))
				.sort((a, b) => b.at - a.at)
				.map(({ at: _at, ...summary }) => summary);
		},

		async follow(conversation, changed) {
			const found = await find(conversation);
			if (found === undefined) return undefined;
			const watch = await found.watch(context);
			watch.start(async (call) => {
				if (call.kind === "value") changed(standing(call.value));
			});
			return { ...standing(watch.value), stop: () => void watch.stop() };
		},

		async send(conversation, text, model) {
			const found = await find(conversation);
			if (found === undefined) return undefined;
			try {
				const { turn, submission } = await harness.commit(async (tx) => {
					const kept = (await tx.doc(Conversations)).conversations[conversation];
					if (kept === undefined) throw new Error(`conversation ${conversation} is not kept`);
					const turn = kept.turns + 1;
					// One Model covers the whole Agent, so a conversation
					// takes the one in use now, whichever answered it before.
					await configure(tx, found.id, { model: { provider: model.provider, modelId: model.model } });
					const submission = await found.submit(tx, {
						type: "input",
						content: text,
						whenBusy: "reject",
						entry: { kind: MESSAGE, data: { turn } },
					});
					kept.turns = turn;
					if (turn === 1) kept.title = text.trim().split("\n", 1)[0]?.slice(0, 80) ?? "";
					return { turn, submission };
				}, context);
				const ended = (async (): Promise<Turn["state"]> => {
					const settled = await (await harness.submission(submission, context))?.wait(context);
					if (settled?.status === "done") return "done";
					return settled?.reason === "aborted" ? "stopped" : "failed";
				})();
				return { turn, ended };
			} catch (error) {
				if (error instanceof ConversationBusy) return "working";
				throw error;
			}
		},

		async stop(conversation) {
			const found = await find(conversation);
			if (found === undefined) return false;
			await found.abort(context);
			return true;
		},

		close: () => harness.close(context),
	};
}
