// Fulcrum's Harness socket, from the Harness's side (ADR 0035): one framed
// connection to Fulcrum, kept open with backoff, used only to ask for a
// snapshot of desktop context when a message arrives. The Harness works when
// Fulcrum is absent or restarts.
import { connect, type Socket } from "node:net";
import { encode, FrameReader, FrameTooLong } from "./frames.ts";

/** The version Fulcrum's Harness socket speaks. */
const VERSION = 1;
/** The longest reply frame taken from Fulcrum. */
const MAX_FRAME = 1024 * 1024;
/** Initial and maximum reconnect backoff, in milliseconds. */
const INITIAL_BACKOFF_MS = 100;
const MAX_BACKOFF_MS = 5000;
/** How long a snapshot request waits before giving up and letting the turn go on without it. */
const DEFAULT_SNAPSHOT_TIMEOUT_MS = 2000;

/** One open window, without its contents. */
export type DesktopWindow = {
	id: string;
	app_id: string;
	title: string;
	workspace: number;
};

/** One display on the desktop. */
export type DesktopDisplay = {
	name: string;
	width: number;
	height: number;
	x: number;
	y: number;
	scale: number;
};

/** The desktop context carried alongside a message from the person. */
export type DesktopContext = {
	workspace: number;
	active: DesktopWindow | null;
	locked: boolean;
	windows: DesktopWindow[];
	displays: DesktopDisplay[];
};

function isWindow(value: unknown): value is DesktopWindow {
	if (typeof value !== "object" || value === null) return false;
	const record = value as Record<string, unknown>;
	return (
		typeof record.id === "string" &&
		typeof record.app_id === "string" &&
		typeof record.title === "string" &&
		typeof record.workspace === "number"
	);
}

function isDisplay(value: unknown): value is DesktopDisplay {
	if (typeof value !== "object" || value === null) return false;
	const record = value as Record<string, unknown>;
	return (
		typeof record.name === "string" &&
		typeof record.width === "number" &&
		typeof record.height === "number" &&
		typeof record.x === "number" &&
		typeof record.y === "number" &&
		typeof record.scale === "number"
	);
}

/** Turns Fulcrum's snapshot reply into a DesktopContext, or undefined if malformed. */
export function parseDesktopContext(raw: unknown): DesktopContext | undefined {
	if (typeof raw !== "object" || raw === null) return undefined;
	const record = raw as Record<string, unknown>;
	if (
		record.reply !== "snapshot" ||
		typeof record.workspace !== "number" ||
		typeof record.locked !== "boolean" ||
		!Array.isArray(record.windows) ||
		!Array.isArray(record.displays)
	) {
		return undefined;
	}
	const windows: DesktopWindow[] = [];
	for (const item of record.windows) {
		if (!isWindow(item)) return undefined;
		windows.push({ id: item.id, app_id: sanitizeLabel(item.app_id), title: sanitizeLabel(item.title), workspace: item.workspace });
	}
	const displays: DesktopDisplay[] = [];
	for (const item of record.displays) {
		if (!isDisplay(item)) return undefined;
		displays.push({
			name: item.name,
			width: item.width,
			height: item.height,
			x: item.x,
			y: item.y,
			scale: item.scale,
		});
	}
	const activeId = typeof record.active === "string" ? record.active : null;
	const active = activeId === null ? null : (windows.find((w) => w.id === activeId) ?? null);
	return { workspace: record.workspace, active, locked: record.locked, windows, displays };
}

function formatScale(scale120: number): string {
	const factor = scale120 / 120;
	return `${Number(factor.toFixed(2))}x`;
}

const MAX_LABEL_CHARS = 200;

/** Collapses control/line-separator characters and caps code points on app-supplied metadata. */
function sanitizeLabel(value: string): string {
	const clean = value.replace(/[\x00-\x1f\x7f-\x9f\u2028\u2029]+/g, " ").trim();
	const chars = Array.from(clean);
	if (chars.length <= MAX_LABEL_CHARS) return clean;
	return `${chars.slice(0, MAX_LABEL_CHARS - 1).join("")}…`;
}

function formatWindow(window: DesktopWindow): string {
	const cleanTitle = sanitizeLabel(window.title);
	const title = cleanTitle === "" ? "(untitled)" : cleanTitle;
	const appId = sanitizeLabel(window.app_id);
	return `${title} (app_id: ${appId})`;
}

/**
 * Formats the desktop context as a plain-text header sent to the Model with
 * the person's message: active window title and app_id, every window by
 * workspace, and the displays. Never window contents.
 */
export function formatDesktopContext(context: DesktopContext): string {
	const lines: string[] = ["[Desktop context]"];
	lines.push(`Workspace: ${context.workspace}`);
	if (context.active !== null) {
		lines.push(`Active window: ${formatWindow(context.active)}`);
	} else {
		lines.push("Active window: none");
	}
	if (context.windows.length === 0) {
		lines.push("Windows: none");
	} else {
		lines.push("Windows by workspace:");
		const byWorkspace = new Map<number, DesktopWindow[]>();
		for (const window of context.windows) {
			const list = byWorkspace.get(window.workspace);
			if (list) list.push(window);
			else byWorkspace.set(window.workspace, [window]);
		}
		const workspaces = [...byWorkspace.keys()].sort((a, b) => a - b);
		for (const workspace of workspaces) {
			const group = byWorkspace.get(workspace) ?? [];
			const formatted = group
				.map((w) => `${formatWindow(w)}${context.active?.id === w.id ? " [active]" : ""}`)
				.join("; ");
			lines.push(`- Workspace ${workspace}: ${formatted}`);
		}
	}
	if (context.displays.length === 0) {
		lines.push("Displays: none");
	} else {
		lines.push("Displays:");
		for (const display of context.displays) {
			lines.push(`- ${display.name}: ${display.width}x${display.height} at (${display.x}, ${display.y}), scale ${formatScale(display.scale)}`);
		}
	}
	return lines.join("\n");
}

interface PendingSnapshot {
	resolve: (context: DesktopContext | undefined) => void;
	timer: NodeJS.Timeout;
}

/**
 * Keeps a framed connection to Fulcrum's Harness socket, reconnecting with
 * exponential backoff when Fulcrum is absent or restarts.
 */
export class FulcrumClient {
	#path: string;
	#say: (what: string) => void;
	#socket: Socket | undefined;
	#versioned = false;
	#backoff = INITIAL_BACKOFF_MS;
	#reconnectTimer: NodeJS.Timeout | undefined;
	#closed = false;
	/** Snapshot requests waiting for Fulcrum's reply, in order. */
	#pending: PendingSnapshot[] = [];
	/** Callers waiting for an already-connected socket to finish its version handshake. */
	#readyWaiters: (() => void)[] = [];

	constructor(path: string, say: (what: string) => void) {
		this.#path = path;
		this.#say = say;
		this.#dial();
	}

	#dial(): void {
		if (this.#closed) return;
		const socket = connect(this.#path);
		this.#socket = socket;
		this.#versioned = false;
		const frames = new FrameReader(MAX_FRAME);
		let hadVersion = false;

		socket.once("connect", () => {
			if (this.#closed || this.#socket !== socket) {
				socket.destroy();
				return;
			}
			socket.write(encode({ version: VERSION }));
		});

		socket.on("data", (bytes: Buffer) => {
			if (this.#socket !== socket) return;
			try {
				for (const frame of frames.read(bytes)) {
					let message: unknown;
					try {
						message = JSON.parse(frame.toString());
					} catch {
						socket.destroy();
						return;
					}
					if (!this.#versioned) {
						if (
							typeof message !== "object" ||
							message === null ||
							Array.isArray(message) ||
							Object.keys(message).length !== 1 ||
							(message as Record<string, unknown>).version !== VERSION
						) {
							socket.destroy();
							return;
						}
						this.#versioned = true;
						hadVersion = true;
						this.#backoff = INITIAL_BACKOFF_MS;
						this.#say(`connected to Fulcrum at ${this.#path}`);
						for (const wake of this.#readyWaiters.splice(0)) wake();
						continue;
					}
					const next = this.#pending.shift();
					if (next) {
						clearTimeout(next.timer);
						next.resolve(parseDesktopContext(message));
					}
				}
			} catch (error) {
				if (error instanceof FrameTooLong) socket.destroy();
				else throw error;
			}
		});

		const drop = () => {
			if (this.#socket !== socket) return;
			this.#socket = undefined;
			this.#versioned = false;
			if (hadVersion) this.#say("Fulcrum disconnected");
			this.#failPending();
			for (const wake of this.#readyWaiters.splice(0)) wake();
			this.#scheduleReconnect();
		};

		socket.on("error", () => socket.destroy());
		socket.on("close", drop);
	}

	#failPending(): void {
		for (const item of this.#pending.splice(0)) {
			clearTimeout(item.timer);
			item.resolve(undefined);
		}
	}

	#scheduleReconnect(): void {
		if (this.#closed || this.#reconnectTimer !== undefined) return;
		const delay = this.#backoff;
		this.#backoff = Math.min(this.#backoff * 2, MAX_BACKOFF_MS);
		this.#reconnectTimer = setTimeout(() => {
			this.#reconnectTimer = undefined;
			this.#dial();
		}, delay);
		this.#reconnectTimer.unref();
	}

	/**
	 * Asks Fulcrum for a desktop context snapshot. Resolves to `undefined` when
	 * Fulcrum is absent, disconnected, or does not answer in time.
	 */
	async snapshot(timeoutMs = DEFAULT_SNAPSHOT_TIMEOUT_MS): Promise<DesktopContext | undefined> {
		if (this.#closed) return undefined;
		// If a connection attempt is already underway, wait briefly for its version handshake.
		if (!this.#versioned && this.#socket !== undefined) {
			await new Promise<void>((resolve) => {
				const timer = setTimeout(resolve, Math.min(timeoutMs, 250));
				timer.unref();
				this.#readyWaiters.push(() => {
					clearTimeout(timer);
					resolve();
				});
			});
		}
		const socket = this.#socket;
		if (!this.#versioned || socket === undefined || socket.destroyed) return undefined;
		return new Promise<DesktopContext | undefined>((resolve) => {
			const entry: PendingSnapshot = {
				resolve,
				timer: setTimeout(() => {
					const idx = this.#pending.indexOf(entry);
					if (idx !== -1) this.#pending.splice(idx, 1);
					resolve(undefined);
					this.#say("Fulcrum snapshot timed out; reconnecting");
					// Drop the connection so a late reply cannot shift into a later request.
					if (this.#socket === socket) socket.destroy();
				}, timeoutMs),
			};
			entry.timer.unref();
			this.#pending.push(entry);
			try {
				socket.write(encode({ op: "snapshot" }));
			} catch {
				const idx = this.#pending.indexOf(entry);
				if (idx !== -1) this.#pending.splice(idx, 1);
				clearTimeout(entry.timer);
				resolve(undefined);
			}
		});
	}

	close(): void {
		this.#closed = true;
		if (this.#reconnectTimer !== undefined) {
			clearTimeout(this.#reconnectTimer);
			this.#reconnectTimer = undefined;
		}
		this.#failPending();
		for (const wake of this.#readyWaiters.splice(0)) wake();
		this.#socket?.destroy();
		this.#socket = undefined;
		this.#versioned = false;
	}
}
