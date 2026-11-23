// The credential store: the API keys and OAuth tokens the person signed in
// with, one for each provider, in one file in the Harness's state, which is
// the person's alone (ADR 0034). pi-ai reads and writes them through this,
// and refreshes an OAuth token inside `modify`, so two requests never
// refresh one token twice.
import type { Credential, CredentialInfo, CredentialStore } from "@earendil-works/pi-ai";
import { keep, readKept } from "./kept.ts";

export class FileCredentials implements CredentialStore {
	#path: string;
	/** Every read and write, one after another. */
	#last: Promise<unknown> = Promise.resolve();

	constructor(path: string) {
		this.#path = path;
	}

	#all(): Record<string, Credential> {
		return readKept<Record<string, Credential>>(this.#path, {});
	}

	#keep(all: Record<string, Credential>): void {
		keep(this.#path, all);
	}

	#next<T>(work: () => Promise<T>): Promise<T> {
		const done = this.#last.then(work, work);
		this.#last = done.catch(() => {});
		return done;
	}

	read(providerId: string): Promise<Credential | undefined> {
		return this.#next(async () => this.#all()[providerId]);
	}

	list(): Promise<readonly CredentialInfo[]> {
		return this.#next(async () => Object.entries(this.#all()).map(([providerId, { type }]) => ({ providerId, type })));
	}

	modify(providerId: string, change: (current: Credential | undefined) => Promise<Credential | undefined>): Promise<Credential | undefined> {
		return this.#next(async () => {
			const all = this.#all();
			const changed = await change(all[providerId]);
			if (changed === undefined) return all[providerId];
			this.#keep({ ...all, [providerId]: changed });
			return changed;
		});
	}

	delete(providerId: string): Promise<void> {
		return this.#next(async () => {
			const { [providerId]: gone, ...rest } = this.#all();
			if (gone !== undefined) this.#keep(rest);
		});
	}
}
