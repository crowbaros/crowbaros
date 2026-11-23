// The person socket: where the person's own clients reach the Harness, the
// terminal's `crowbar-agent` first. README.md has its messages. A client
// only relays: the conversations are the store's, and a sign-in is taken
// step by step in models.ts.
import type { Channel, Connection } from "./channel.ts";
import type { Conversations } from "./conversations.ts";
import type { Event } from "./log.ts";
import { type Asking, type ModelAccess, Refused } from "./models.ts";
import type { Store, Turn } from "./store.ts";

/** The longest frame the Harness takes from a client: room for a long message of the person's. */
const MAX_FRAME = 1024 * 1024;

type Request = { op: string } & Record<string, unknown>;

/** A request the Harness will not carry out, with the code a client tells it by. */
class Declined extends Error {
	code: string;

	constructor(code: string, message: string) {
		super(message);
		this.code = code;
	}
}

/** A request's field that must be text, and not empty. */
function text(request: Request, field: string): string {
	const value = request[field];
	if (typeof value !== "string" || value === "") throw new Declined("request", `"${request.op}" needs "${field}", as text.`);
	return value;
}

/** A request's field that may be left out, and must be text if it is there. */
function optional(request: Request, field: string): string | undefined {
	return request[field] === undefined || request[field] === null ? undefined : text(request, field);
}

/** A turn as a reply shows it. What waits for another try is an event's to tell. */
function shown({ retry: _retry, ...turn }: Turn): Omit<Turn, "retry"> {
	return turn;
}

/**
 * The person socket's rules, for the person's user ID. `say` is given events
 * for the log, with IDs and counts and nothing a conversation says.
 */
export function person(uid: number, conversations: Conversations, store: Store, access: ModelAccess, say: (event: Event, details: string) => void): Channel {
	/** One sign-in at a time, whichever client began it. */
	let signingIn = false;

	const serve = (connection: Connection) => {
		/** The conversation this client follows. */
		let following: { conversation: string; leave(): void } | undefined;
		/** Events kept back while a request of this client's is answered, so its reply comes first. */
		let held: object[] | undefined;
		/** This client's sign-in, while one is under way. */
		let signIn: { cancel: AbortController; asked: Map<number, { resolve(answer: string): void }>; prompts: number } | undefined;
		/** Requests are answered one at a time, in the order they came. */
		let answering: Promise<void> = Promise.resolve();

		const told = (event: object) => {
			if (held) held.push(event);
			else connection.send(event);
		};

		const follow = async (conversation: string) => {
			const followed = await conversations.follow(conversation, told);
			if (followed === undefined) throw new Declined("no-conversation", `There is no conversation ${conversation}.`);
			// Resuming the conversation it already follows leaves nothing: it is one follower still.
			if (following !== undefined && following.conversation !== conversation) following.leave();
			following = { conversation, leave: followed.leave };
			return followed;
		};

		/** Takes a sign-in through its steps, and tells the client how it ended. */
		const runSignIn = async (provider: string | undefined, mine: NonNullable<typeof signIn>) => {
			const cancelled = mine.cancel.signal;
			const ask = (asking: Asking, withdrawn?: AbortSignal) =>
				new Promise<string>((resolve, reject) => {
					const prompt = ++mine.prompts;
					const end = (why: string, tell: boolean) => {
						if (!mine.asked.delete(prompt)) return;
						if (tell) told({ event: "withdrawn", prompt });
						reject(new Error(why));
					};
					cancelled.throwIfAborted();
					mine.asked.set(prompt, { resolve });
					cancelled.addEventListener("abort", () => end("the sign-in was cancelled", false), { once: true });
					// Something else answered it, as when the browser came back before the person pasted a code.
					withdrawn?.addEventListener("abort", () => end("the question was withdrawn", true), { once: true });
					told({ event: "prompt", prompt, ...asking });
				});
			try {
				const { provider: id, name, model } = await access.signIn(provider, { ask, tell: told }, cancelled);
				say("sign-in", `signed in to ${id}`);
				told({ event: "signed-in", provider: id, provider_name: name, ...(model ? { model: model.model, name: model.name } : {}) });
			} catch (error) {
				say("sign-in", cancelled.aborted ? "cancelled" : "failed");
				told({
					event: "sign-in-failed",
					cancelled: cancelled.aborted,
					message: cancelled.aborted ? "The sign-in was cancelled." : error instanceof Error ? error.message : String(error),
				});
			} finally {
				signingIn = false;
				signIn = undefined;
			}
		};

		const answer = async (request: Request): Promise<object> => {
			switch (request.op) {
				case "start": {
					const conversation = await store.start();
					await follow(conversation);
					say("conversation", `${conversation} started`);
					return { reply: "started", conversation };
				}
				case "list":
					return { reply: "conversations", conversations: await store.list() };
				case "resume": {
					const conversation = text(request, "conversation");
					const { turns, earlier } = await follow(conversation);
					return { reply: "resumed", conversation, earlier, turns: turns.map(shown) };
				}
				case "send": {
					const conversation = text(request, "conversation");
					const message = text(request, "text");
					const model = await access.inUse();
					if ("missing" in model) throw new Declined("no-model", model.missing);
					const sent = await store.send(conversation, message, model);
					if (sent === undefined) throw new Declined("no-conversation", `There is no conversation ${conversation}.`);
					if (sent === "working") throw new Declined("working", "The Agent is still working on the message before this one.");
					say("turn", `conversation ${conversation}, turn ${sent.turn} began`);
					sent.ended.then(
						(how) => say("turn", `conversation ${conversation}, turn ${sent.turn} ended: ${how}`),
						// The Harness stopped first. The turn goes on when it starts again.
						() => {},
					);
					return { reply: "sent", conversation, turn: sent.turn };
				}
				case "stop": {
					const conversation = text(request, "conversation");
					if (!(await store.stop(conversation))) throw new Declined("no-conversation", `There is no conversation ${conversation}.`);
					return { reply: "ok" };
				}
				case "status": {
					const inUse = await access.inUse();
					const signed_in = await access.signedIn();
					if ("missing" in inUse) return { reply: "status", model: null, missing: inUse.missing, signed_in };
					const { providerName, ...model } = inUse;
					return { reply: "status", model: { ...model, provider_name: providerName }, signed_in };
				}
				case "models": {
					const inUse = await access.inUse();
					const models = (await access.available()).map(({ providerName, ...model }) => ({
						...model,
						provider_name: providerName,
						in_use: !("missing" in inUse) && inUse.provider === model.provider && inUse.model === model.model,
					}));
					return { reply: "models", models };
				}
				case "model": {
					const { providerName, ...model } = await access.choose(text(request, "provider"), text(request, "model"));
					say("sign-in", `the Model in use is one of ${model.provider}'s`);
					return { reply: "model", ...model, provider_name: providerName };
				}
				case "login": {
					const provider = optional(request, "provider");
					if (signingIn) throw new Declined("signing-in", "A sign-in is already under way.");
					signingIn = true;
					signIn = { cancel: new AbortController(), asked: new Map(), prompts: 0 };
					void runSignIn(provider, signIn);
					return { reply: "ok" };
				}
				case "answer": {
					const asked = signIn?.asked.get(request.prompt as number);
					if (asked === undefined || typeof request.text !== "string") throw new Declined("request", "No such question waits for an answer.");
					signIn?.asked.delete(request.prompt as number);
					asked.resolve(request.text);
					return { reply: "ok" };
				}
				case "cancel":
					signIn?.cancel.abort();
					return { reply: "ok" };
				case "logout": {
					const { provider, name } = await access.signOut(optional(request, "provider"));
					say("sign-in", `signed out of ${provider}`);
					return { reply: "signed-out", provider, provider_name: name };
				}
				default:
					throw new Declined("request", `The Harness has no request "${request.op}".`);
			}
		};

		return {
			frame(frame: Buffer) {
				let request: Request;
				try {
					request = JSON.parse(frame.toString()) as Request;
				} catch {
					return connection.close("a client's frame was not JSON");
				}
				if (typeof request !== "object" || request === null || typeof request.op !== "string") {
					return connection.close("a client's frame was not a request");
				}
				answering = answering.then(async () => {
					held = [];
					let reply: object;
					try {
						reply = await answer(request);
					} catch (error) {
						// Anything else is the Harness's own fault, and ends it as any error it did not expect.
						if (!(error instanceof Declined) && !(error instanceof Refused)) throw error;
						reply = { reply: "error", code: error instanceof Declined ? error.code : "refused", message: error.message };
					}
					connection.send(reply);
					const kept = held;
					held = undefined;
					for (const event of kept) connection.send(event);
				});
			},
			left() {
				following?.leave();
				signIn?.cancel.abort();
			},
		};
	};

	return { version: 1, uid, peer: "a client", whose: "the person's", maxFrame: MAX_FRAME, serve };
}
