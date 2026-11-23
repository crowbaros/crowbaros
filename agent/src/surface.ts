// The surface socket: where the shell reaches the Harness. README.md has its
// framing and its messages. It speaks the same conversation and status
// messages as the person socket, and refuses sign-in requests.
import type { Channel } from "./channel.ts";
import type { Conversations } from "./conversations.ts";
import type { Event } from "./log.ts";
import type { ModelAccess } from "./models.ts";
import { type ContextSource, clientChannel } from "./person.ts";
import type { Store } from "./store.ts";

/** The surface socket's rules, for the shell's user ID. */
export function surface(
	shellUid: number,
	conversations: Conversations,
	store: Store,
	access: ModelAccess,
	say: (event: Event, details: string) => void,
	fulcrum?: ContextSource,
): Channel {
	return clientChannel({
		uid: shellUid,
		peer: "the shell",
		whose: "the shell's",
		allowSignIn: false,
		conversations,
		store,
		access,
		say,
		fulcrum,
	});
}
