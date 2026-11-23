// Reaching a Model: pi-ai's providers, the sign-in to each, and the one Model
// the Agent uses. The person signs in through a client, which only relays:
// every step of a sign-in is taken here, and the credentials are kept in the
// Harness's state (credentials.ts). README.md has the files and the messages.
import { randomUUID } from "node:crypto";
import { chmodSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { AuthEvent, AuthPrompt } from "@earendil-works/pi-ai";
import { openAICompletionsApi } from "@earendil-works/pi-ai/api/openai-completions.lazy";
import { registerLazyBedrockProvider } from "@earendil-works/pi-ai/lazy-bedrock-provider";
import { registerLazyOAuthFlows } from "@earendil-works/pi-ai/lazy-oauth";
import { createModels, createProvider, type MutableModels } from "@earendil-works/pi-ai/models";
import { FileCredentials } from "./credentials.ts";

/** The provider an endpoint file makes. No sign-in reaches it. */
const ENDPOINT = "endpoint";

/** The Model the Agent uses. */
export interface InUse {
	/** pi-ai's name for the provider, as `login` takes it. */
	provider: string;
	/** The provider's name for a person to read. */
	providerName: string;
	/** The provider's own ID for the Model. */
	model: string;
	/** The Model's name for a person to read. */
	name: string;
}

/** A question a sign-in puts to the person. `code` is a code or an address to paste. */
export type Asking =
	| { kind: "text" | "secret" | "code"; message: string; placeholder?: string }
	| { kind: "select"; message: string; options: { id: string; label: string; description?: string }[] };

/** What a sign-in tells the person without waiting for an answer. */
export type Telling =
	| { event: "notice"; message: string }
	/** An address to open in a browser. */
	| { event: "browser"; url: string; message?: string }
	/** A code to enter at an address. */
	| { event: "code"; code: string; url: string };

/** The person's side of a sign-in, which a client relays. */
export interface Relay {
	/** Resolves with what the person answered, or the ID of what they picked. Rejects once `withdrawn` or the sign-in is cancelled. */
	ask(asking: Asking, withdrawn?: AbortSignal): Promise<string>;
	tell(telling: Telling): void;
}

/** A sign-in the person can be told about: no fault of the Harness. */
export class Refused extends Error {}

interface Settings {
	/** The Model the person chose. */
	model?: { provider: string; model: string };
	/** This installation's ID, for the sign-ins that ask for one. Made the first time one does. */
	device?: string;
}

export class ModelAccess {
	/** What Pi Durable asks Models through. */
	readonly models: MutableModels;
	#credentials: FileCredentials;
	#settings: string;
	#endpoint: string;
	/** The endpoint file as it was last read, or nothing when there was none. */
	#endpointRead: string | undefined;

	private constructor(state: string, models: MutableModels, credentials: FileCredentials) {
		this.models = models;
		this.#credentials = credentials;
		this.#settings = join(state, "settings.json");
		this.#endpoint = join(state, "endpoint.json");
	}

	/** Every provider pi-ai has, with credentials and settings kept in `state`. */
	static async open(state: string): Promise<ModelAccess> {
		// pi-ai loads these two through names a bundler cannot follow; this
		// puts them in the one file, loaded when first used.
		registerLazyOAuthFlows();
		registerLazyBedrockProvider();
		const credentials = new FileCredentials(join(state, "credentials.json"));
		const models = createModels({ credentials });
		// Loaded this way and not named at the top: the bundle then holds
		// the providers' catalogs in a part that costs less memory.
		const { builtinProviders } = await import("@earendil-works/pi-ai/providers/all");
		for (const provider of builtinProviders()) models.setProvider(provider);
		const access = new ModelAccess(state, models, credentials);
		// A turn the last Harness left unfinished goes on as soon as the
		// store opens, so the endpoint it was sent to must be there by then.
		try {
			access.#fromEndpoint();
		} catch (error) {
			if (!(error instanceof Refused)) throw error;
		}
		return access;
	}

	#read(): Settings {
		try {
			return JSON.parse(readFileSync(this.#settings, "utf8")) as Settings;
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code === "ENOENT") return {};
			throw error;
		}
	}

	#keep(settings: Settings): void {
		const beside = `${this.#settings}.new`;
		writeFileSync(beside, `${JSON.stringify(settings, null, "\t")}\n`, { mode: 0o600 });
		chmodSync(beside, 0o600);
		renameSync(beside, this.#settings);
	}

	/**
	 * The endpoint file's Model, or nothing when there is no such file. The
	 * file is read at each use, so it takes effect when it is written.
	 * Throws `Refused` for a file that does not say what it must.
	 */
	#fromEndpoint(): InUse | undefined {
		let text: string | undefined;
		try {
			text = readFileSync(this.#endpoint, "utf8");
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
		}
		if (text === undefined) {
			if (this.#endpointRead !== undefined) this.models.deleteProvider(ENDPOINT);
			this.#endpointRead = undefined;
			return undefined;
		}
		let file: { url?: unknown; model?: unknown; key?: unknown };
		try {
			file = JSON.parse(text) as typeof file;
		} catch {
			file = {};
		}
		const { url, model, key } = file ?? {};
		if (typeof url !== "string" || typeof model !== "string" || (key !== undefined && typeof key !== "string")) {
			throw new Refused(`${this.#endpoint} must hold a "url" and a "model", and may hold a "key".`);
		}
		if (text !== this.#endpointRead) {
			this.models.setProvider(
				createProvider({
					id: ENDPOINT,
					name: "Endpoint",
					baseUrl: url,
					auth: { apiKey: { name: "Endpoint", resolve: async () => ({ auth: { apiKey: key ?? "none" } }) } },
					models: [
						{
							id: model,
							name: model,
							api: "openai-completions",
							provider: ENDPOINT,
							baseUrl: url,
							reasoning: false,
							input: ["text"],
							cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
							contextWindow: 128_000,
							maxTokens: 16_384,
						},
					],
					api: openAICompletionsApi(),
				}),
			);
			this.#endpointRead = text;
		}
		return { provider: ENDPOINT, providerName: `the endpoint at ${url}`, model, name: model };
	}

	#named(provider: string, model: string): InUse | undefined {
		const found = this.models.getModel(provider, model);
		return found && { provider, providerName: this.models.getProvider(provider)?.name ?? provider, model, name: found.name };
	}

	/** The Model the Agent uses, or `missing`, a sentence that says why there is none. */
	async inUse(): Promise<InUse | { missing: string }> {
		try {
			const endpoint = this.#fromEndpoint();
			if (endpoint) return endpoint;
		} catch (error) {
			if (error instanceof Refused) return { missing: error.message };
			throw error;
		}
		const chosen = this.#read().model;
		const signedIn = await this.#credentials.list();
		if (chosen && signedIn.some(({ providerId }) => providerId === chosen.provider)) {
			const named = this.#named(chosen.provider, chosen.model);
			if (named) return named;
		}
		return { missing: signedIn.length === 0 ? "No Model is signed in." : "No Model is chosen." };
	}

	/** Each provider the person is signed in to, and how. */
	async signedIn(): Promise<{ provider: string; name: string; how: "API key" | "OAuth" }[]> {
		return (await this.#credentials.list()).map(({ providerId, type }) => ({
			provider: providerId,
			name: this.models.getProvider(providerId)?.name ?? providerId,
			how: type === "oauth" ? "OAuth" : "API key",
		}));
	}

	/** Every Model of every provider the person is signed in to. */
	async available(): Promise<InUse[]> {
		const all: InUse[] = [];
		for (const { providerId } of await this.#credentials.list()) {
			const providerName = this.models.getProvider(providerId)?.name ?? providerId;
			for (const model of await this.models.getAvailable(providerId)) {
				all.push({ provider: providerId, providerName, model: model.id, name: model.name });
			}
		}
		return all;
	}

	/** Makes this the Model the Agent uses. */
	async choose(provider: string, model: string): Promise<InUse> {
		if (!(await this.available()).some((one) => one.provider === provider && one.model === model)) {
			throw new Refused(`No Model named ${model} at a provider named ${provider} that is signed in.`);
		}
		this.#keep({ ...this.#read(), model: { provider, model } });
		return this.#named(provider, model) as InUse;
	}

	/**
	 * Signs in to a provider, the one named or the one the person picks,
	 * and has them pick its Model for the Agent to use. pi-ai takes every
	 * step and keeps the credential; `relay` carries its questions to the
	 * person. `cancelled` ends it, and it then rejects.
	 */
	async signIn(named: string | undefined, relay: Relay, cancelled: AbortSignal): Promise<{ provider: string; name: string; model?: InUse }> {
		const offered = this.models.getProviders().filter((one) => one.id !== ENDPOINT && (one.auth.oauth || one.auth.apiKey?.login));
		const id =
			named ??
			(await relay.ask({
				kind: "select",
				message: "Which provider do you want to sign in to?",
				options: offered.map((one) => ({ id: one.id, label: one.name })),
			}));
		const provider = offered.find((one) => one.id === id);
		if (provider === undefined) throw new Refused(`No provider named ${id} to sign in to.`);
		const { oauth, apiKey } = provider.auth;
		const type =
			oauth && apiKey?.login
				? ((await relay.ask({
						kind: "select",
						message: `How do you want to sign in to ${provider.name}?`,
						options: [
							{ id: "oauth", label: oauth.loginLabel ?? oauth.name },
							{ id: "api_key", label: apiKey.name },
						],
					})) as "oauth" | "api_key")
				: oauth
					? "oauth"
					: "api_key";
		if (type !== "oauth" && type !== "api_key") throw new Refused(`${type} is not a way to sign in.`);
		await this.models.login(
			provider.id,
			type,
			{
				signal: cancelled,
				prompt: (prompt: AuthPrompt) =>
					relay.ask(
						prompt.type === "select"
							? { kind: "select", message: prompt.message, options: prompt.options.map((option) => ({ ...option })) }
							: {
									kind: prompt.type === "manual_code" ? "code" : prompt.type,
									message: prompt.message,
									...(prompt.placeholder === undefined ? {} : { placeholder: prompt.placeholder }),
								},
						prompt.signal,
					),
				notify: (event: AuthEvent) => {
					if (event.type === "auth_url") {
						relay.tell({ event: "browser", url: event.url, ...(event.instructions === undefined ? {} : { message: event.instructions }) });
					} else if (event.type === "device_code") {
						relay.tell({ event: "code", code: event.userCode, url: event.verificationUri });
					} else {
						const links = event.type === "info" ? (event.links ?? []).map((link) => `\n${link.label ?? "More"}: ${link.url}`) : [];
						relay.tell({ event: "notice", message: event.message + links.join("") });
					}
				},
			},
			{
				agentName: "CrowbarOS",
				getDeviceId: () => {
					const settings = this.#read();
					if (settings.device) return settings.device;
					const device = randomUUID();
					this.#keep({ ...settings, device });
					return device;
				},
			},
		);
		// The Model of this provider the Agent uses: the one it used before,
		// the only one, or the one the person picks now.
		const models = await this.models.getAvailable(provider.id);
		const before = this.#read().model;
		let model = before?.provider === provider.id ? models.find((one) => one.id === before.model) : undefined;
		if (model === undefined && models.length === 1) model = models[0];
		if (model === undefined && models.length > 1) {
			const picked = await relay.ask({
				kind: "select",
				message: "Which Model should the Agent use?",
				options: models.map((one) => ({ id: one.id, label: one.name })),
			});
			model = models.find((one) => one.id === picked);
			if (model === undefined) throw new Refused(`${provider.name} has no Model named ${picked}. You are signed in, and no Model is chosen.`);
		}
		if (model === undefined) return { provider: provider.id, name: provider.name };
		return { provider: provider.id, name: provider.name, model: await this.choose(provider.id, model.id) };
	}

	/**
	 * Signs out of a provider: the one named, or the only one the person is
	 * signed in to. Its credential is removed from the Harness's state.
	 */
	async signOut(named: string | undefined): Promise<{ provider: string; name: string }> {
		const signedIn = await this.signedIn();
		if (named === undefined && signedIn.length > 1) {
			throw new Refused(`Signed in to ${signedIn.map((one) => one.provider).join(", ")}. Name the one to sign out of.`);
		}
		const from = named === undefined ? signedIn[0] : signedIn.find((one) => one.provider === named);
		if (from === undefined) throw new Refused(named === undefined ? "Not signed in to any provider." : `Not signed in to ${named}.`);
		await this.models.logout(from.provider);
		const settings = this.#read();
		if (settings.model?.provider === from.provider) {
			const { model: _gone, ...rest } = settings;
			this.#keep(rest);
		}
		return { provider: from.provider, name: from.name };
	}
}
