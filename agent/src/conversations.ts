// Conversations as clients follow them. Any number of clients may follow one
// conversation: each gets it as it stands when it joins, and then the same
// events as the rest. A client that leaves stops nothing: the turn goes on
// in the store, followed or not.
import type { Standing, Store, Turn } from "./store.ts";

/** One event of a conversation, as README.md lists them. */
export type ConversationEvent = { event: string; conversation: string; turn: number } & Record<string, unknown>;

/** Whoever is told a conversation's events. */
export type Follower = (event: ConversationEvent) => void;

/** The events that take a conversation from how it stood to how it stands. */
export function events(conversation: string, before: Map<number, Turn>, turns: readonly Turn[]): ConversationEvent[] {
	const out: ConversationEvent[] = [];
	for (const turn of turns) {
		const known = before.get(turn.turn);
		const about = { conversation, turn: turn.turn };
		if (known === undefined) out.push({ event: "message", ...about, text: turn.message });
		// Already ended, and told so: nothing of an ended turn changes.
		if (known !== undefined && known.state !== "working") continue;
		// The Model was asked again, and its answer starts over from nothing:
		// a new try, or an answer that does not go on from the one told. The
		// new try's may begin with the very words the last one got out.
		const again = known?.attempt !== undefined && turn.attempt !== undefined && known.attempt !== turn.attempt;
		const over = known !== undefined && known.answer !== "" && (again || !turn.answer.startsWith(known.answer));
		if (over) out.push({ event: "start-over", ...about });
		const more = over ? turn.answer : turn.answer.slice(known?.answer.length ?? 0);
		if (more !== "") out.push({ event: "text", ...about, text: more });
		if (turn.retry !== undefined && turn.retry.at !== known?.retry?.at) {
			out.push({ event: "retry", ...about, message: turn.retry.error, seconds: Math.max(0, Math.round((turn.retry.at - Date.now()) / 1000)) });
		}
		if (turn.state === "failed") out.push({ event: "failed", ...about, message: turn.error ?? "" });
		else if (turn.state !== "working") out.push({ event: turn.state, ...about });
	}
	return out;
}

/** One conversation that at least one client follows. */
class Feed {
	followers = new Set<Follower>();
	/** Every turn this feed has seen, as it was last seen: what its followers have been told. */
	turns = new Map<number, Turn>();
	/** The conversation as it stands: what a client that joins now is given, however much this feed has seen. */
	standing: Standing = { turns: [], earlier: false };
	stop: () => void = () => {};
	conversation: string;

	constructor(conversation: string) {
		this.conversation = conversation;
	}

	advance(standing: Standing): void {
		const told = events(this.conversation, this.turns, standing.turns);
		// The turns that left the newest ones are kept as they were last
		// seen, so one that comes back with the next change is not new.
		for (const turn of standing.turns) this.turns.set(turn.turn, turn);
		this.standing = standing;
		for (const event of told) for (const follower of this.followers) follower(event);
	}
}

export class Conversations {
	#store: Store;
	#feeds = new Map<string, Promise<Feed | undefined>>();

	constructor(store: Store) {
		this.#store = store;
	}

	/**
	 * Has `follower` told each event of this conversation from now on, and
	 * gives the conversation as it stands now, and `leave`, which ends the
	 * telling. Undefined when there is no such conversation.
	 */
	async follow(conversation: string, follower: Follower): Promise<(Standing & { leave(): void }) | undefined> {
		let opening = this.#feeds.get(conversation);
		if (opening === undefined) {
			const feed = new Feed(conversation);
			opening = this.#store.follow(conversation, (standing) => feed.advance(standing)).then((followed) => {
				if (followed === undefined) {
					this.#feeds.delete(conversation);
					return undefined;
				}
				feed.stop = followed.stop;
				// Nobody follows yet, so this tells nobody: it only records how the conversation stands.
				feed.advance({ turns: followed.turns, earlier: followed.earlier });
				return feed;
			});
			this.#feeds.set(conversation, opening);
		}
		const feed = await opening;
		if (feed === undefined) return undefined;
		feed.followers.add(follower);
		return {
			turns: feed.standing.turns,
			earlier: feed.standing.earlier,
			leave: () => {
				feed.followers.delete(follower);
				if (feed.followers.size === 0 && this.#feeds.get(conversation) === opening) {
					this.#feeds.delete(conversation);
					feed.stop();
				}
			},
		};
	}
}
