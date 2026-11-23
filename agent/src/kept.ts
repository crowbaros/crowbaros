// The small files the Harness keeps in its state beside the database: how
// each is read and written, so that every one is the person's alone and none
// is ever left half written.
import { chmodSync, readFileSync, renameSync, writeFileSync } from "node:fs";

/**
 * Something the Harness will not do, in a sentence for the person: no fault
 * of the Harness, so it is told to the client and the Harness goes on.
 */
export class Refused extends Error {}

/**
 * What a JSON file holds, or `absent` when there is no such file. A file
 * that is not JSON is `Refused`, by its path alone: what the file holds may
 * be a credential, and the error's words go to a client.
 */
export function readKept<T>(path: string, absent: T): T {
	let text: string;
	try {
		text = readFileSync(path, "utf8");
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === "ENOENT") return absent;
		throw error;
	}
	try {
		return JSON.parse(text) as T;
	} catch {
		throw new Refused(`${path} is not JSON. Put it right, or remove it.`);
	}
}

/**
 * Writes a JSON file the person alone can read, beside its place and then
 * moved there, so a write that stops partway loses nothing that was kept.
 */
export function keep(path: string, value: unknown): void {
	const beside = `${path}.new`;
	writeFileSync(beside, `${JSON.stringify(value, null, "\t")}\n`, { mode: 0o600 });
	chmodSync(beside, 0o600);
	renameSync(beside, path);
}
