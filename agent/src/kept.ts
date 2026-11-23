// The small files the Harness keeps in its state beside the database: how
// each is read and written, so that every one is the person's alone and none
// is ever left half written.
import { closeSync, fchmodSync, fsyncSync, openSync, readFileSync, renameSync, writeFileSync } from "node:fs";

/**
 * Something the Harness will not do, in a sentence for the person: no fault
 * of the Harness, so it is told to the client and the Harness goes on.
 */
export class Refused extends Error {}

/** A file that is JSON, and not what the Harness keeps in it. */
export function wrong(path: string): Refused {
	return new Refused(`${path} does not hold what the Harness keeps there. Put it right, or remove it.`);
}

/**
 * The JSON object a file holds, or `absent` when there is no such file. A
 * file that is not JSON, or holds anything but an object, is `Refused`, by
 * its path alone: what the file holds may be a credential, and the error's
 * words go to a client.
 */
export function readKept<T extends object>(path: string, absent: T): T {
	let text: string;
	try {
		text = readFileSync(path, "utf8");
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === "ENOENT") return absent;
		throw error;
	}
	let held: unknown;
	try {
		held = JSON.parse(text);
	} catch {
		throw new Refused(`${path} is not JSON. Put it right, or remove it.`);
	}
	if (typeof held !== "object" || held === null || Array.isArray(held)) throw wrong(path);
	return held as T;
}

/**
 * Writes a JSON file the person alone can read, beside its place and then
 * moved there, so a write that stops partway loses nothing that was kept.
 */
export function keep(path: string, value: unknown): void {
	const beside = `${path}.new`;
	const file = openSync(beside, "w", 0o600);
	try {
		fchmodSync(file, 0o600);
		writeFileSync(file, `${JSON.stringify(value, null, "\t")}\n`);
		// On the disk before it takes the place of what was kept, so a power cut leaves one or the other, whole.
		fsyncSync(file);
	} finally {
		closeSync(file);
	}
	renameSync(beside, path);
}
