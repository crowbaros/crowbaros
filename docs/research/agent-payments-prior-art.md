# Agent payments: prior art

How AI agents spend money for a person, and how others give an agent its
own bounded funds. Researched November 2026 from vendor docs, specs,
network rules, and first-party announcements. Written for the CrowbarOS
Allowance design.

Markers:

- **[unverified]**: only secondary sources, or I could not open the primary page.
- **[docs conflict]**: the primary sources disagree with each other.

## Takeaways for CrowbarOS

1. **Issuer-side caps are normal and available to individuals today.**
   Stripe Issuing, Privacy.com, Rain, Mercury, and Ramp enforce limits at
   card authorization. Coinbase Agentic Wallet and Skyfire enforce them on
   their servers before any payment. Our software never has to be the
   limit. Only raw protocols leave it to the client: the x402 spec puts
   "client-side budget management" out of scope.
2. **"Capped" is not the same as "separately funded."** A Privacy.com
   card debits the linked bank account. Its cap is issuer-enforced, but
   the money is not set aside. Prefunded pots exist: Stripe Issuing
   balances (businesses and platforms), Rain Agent Vaults (via partner
   apps), Coinbase Agentic Wallet (USDC), Skyfire, and Payman. For one
   person today, the closest fit is a capped card on a dedicated funding
   account, or a prefunded wallet.
3. **The worst case is the cap, not "what we can dispute."** Visa's rules
   say the cardholder "is responsible for any actions taken by an Agentic
   Payment Provider ... as if the Cardholder initiated the Transaction."
   No network has an agent-specific dispute reason code. US Regulation E
   may not treat an agent's mistake as unauthorized. Stablecoin payments
   are final unless the x402 `auth-capture` scheme is used. So the
   Allowance's funded amount is the real loss bound. Disputes only cover
   ordinary merchant failures (not delivered, not as described).
4. **Standing "spend without asking" grants are just arriving.** Link's
   agent wallet has an approval policy (no manual approval under a
   per-purchase limit) and hard daily and 30-day caps. AP2 v0.2 has
   `payment.budget` and `payment.agent_recurrence` constraints. ChatGPT
   Wallet asks on every purchase today and only plans standing limits.
   Our Allowance is ahead of most consumer products, not behind them.
5. **Agents still see card numbers in the long tail.** Tokens (Stripe
   SPTs, Link Pay Tokens, Visa and Mastercard agentic tokens) hide the
   PAN, but only at merchants that support them. For ordinary card forms,
   Link, Privacy.com (`get_pan`), Mercury, and Stripe Issuing all hand the
   agent a PAN. Link even warns to write it to a `0600` file, not stdout.
   Our broker-fill design is stricter than the industry default. Keep it.
6. **The refill path must be out of the agent's reach.** Coinbase sets
   limits only in its wallet UI ("Agents respect these limits but can't
   change them"). Rain and Mercury say the agent cannot raise its own
   limit. But Privacy.com's MCP has `update_card_spend_limit`, and
   Stripe's MCP can create cards. The broker must not expose "raise
   limit", "create card", or "fund" to the agent. Those are the refill.
7. **"Allowance" already means something else.** In ACP, `allowance` is
   a one-time, one-checkout constraint on a delegated token (`reason`
   MUST be `one_time`). AP2 says "budget". Stripe says
   `spending_controls`. Link says "spend request" and "approval policy".
   Our glossary should say that our Allowance is a standing, funded pot,
   and map it to these terms.
8. **Agent identity is a merchant-facing problem, not a loss bound.**
   Visa TAP and Web Bot Auth sign HTTP requests (RFC 9421). Mastercard
   Agent Pay requires registered agents. AP2 binds open mandates to an
   agent key (`cnf`). Some merchants block agents anyway (Amazon,
   United). If we sign requests as an agent, the broker should hold that
   key, like any other secret.

**Note on caps:** card caps bound authorizations. Settlement can differ.
Stripe lists "over-captures, and force captures" among the transaction
types it tracks. An issuer cap is close to a hard bound, but not exact.
https://docs.stripe.com/issuing/agents

## 1. Stripe

### Issuing for agents (preview)

- Issues cards "that agents can use to make purchases autonomously, with
  spend controls, real-time authorization decisioning, and full
  transaction visibility." Access is by application.
  https://docs.stripe.com/issuing/agents
- Two products. "Cards for your own business" gives each agent its own
  card. "Cards for your platform" lets a platform issue commercial or
  consumer cards to its users. Individuals cannot sign up directly.
  Source as above.
- **What the agent sees.** Three paths: card details from a single-use
  card (browser checkout), a Shared Payment Token made from the card, or
  "an agentic network token created from the Issuing card by Visa or
  Mastercard." Source as above.
- **Enforcement is issuer-side.** `spending_controls` sets
  `allowed_categories` and `spending_limits` with intervals such as
  `per_authorization` and `monthly`. Controls apply at the card or
  cardholder level. Source as above.
- Real-time authorization: an `issuing_authorization.request` webhook
  lets you approve or decline each charge. "There is a 2 second default
  timeout to respond before Stripe falls back to the card's default
  rules." Source as above.
- **Disputes.** Issuer-side disputes via `POST /v1/issuing/disputes`. The
  docs suggest an agent compares authorized and settled amounts and files
  disputes. Source as above.
- Stripe MCP can create cardholders and cards with
  `stripe_api_write`. "Issuing a card doesn't give the agent a way to pay
  with it." https://docs.stripe.com/issuing/agents/mcp
- Funding a programmatic purchase: Issuing card, then a PaymentMethod
  made "without exposing the PAN or CVC", then an SPT. "An authorization
  must satisfy both your card controls and the SPT grant." Needs private
  preview and `2026-04-22.preview`.
  https://docs.stripe.com/issuing/agents/programmatic-checkout

### Shared Payment Tokens (SPTs)

- An SPT gives a seller "limited, scoped access to a customer's payment
  method for purchases an agent makes." Public preview.
  https://docs.stripe.com/agentic-commerce/concepts/shared-payment-tokens
- An SPT is scoped to one seller profile and carries `usage_limits`
  (`max_amount`, `currency`, `expires_at`). It "remains usable until it
  expires, you revoke it, or cumulative captured payments reach
  `usage_limits[max_amount]`."
  https://docs.stripe.com/issuing/agents/programmatic-checkout
- Stripe "might use tokens issued through network programs such as
  Mastercard's Agent Pay, Visa's Intelligent Commerce, and American
  Express ACE Kit." Source: shared-payment-tokens page above.
- Agent-side endpoints: create, retrieve, and revoke
  `/v1/shared_payment/issued_tokens`.
  https://docs.stripe.com/changelog/dahlia/2026-04-22/shared-payment-issued-token

### Agentic Commerce Protocol (ACP, with OpenAI)

- Open spec on GitHub. Latest dated version folder is `2026-04-17`.
  https://github.com/agentic-commerce-protocol/agentic-commerce-protocol
- The delegate-payment RFC issues "a delegated vault token" usable "only
  within explicit _Allowance_ constraints." The `allowance` object has
  `reason` (MUST be `one_time`), `max_amount`, `currency`,
  `checkout_session_id`, `merchant_id`, and `expires_at`. Refund
  semantics are out of scope.
  https://github.com/agentic-commerce-protocol/agentic-commerce-protocol/blob/main/rfcs/rfc.delegate_payment.md
- So ACP's "allowance" is a per-checkout token limit. It is not a
  standing budget.

### Machine Payments Protocol (MPP)

- Co-authored by Stripe and Tempo, announced 18 March 2026. A server
  returns HTTP 402 with a challenge. For cards, the client answers with
  an SPT. Stablecoins are also accepted. Minimums: 0.50 USD by card,
  0.01 USDC. https://docs.stripe.com/payments/machine/mpp ,
  https://docs.stripe.com/payments/machine , https://mpp.dev

### Link agent wallet (consumer)

- Announced at Sessions 2026: "grant agents the ability to pay with
  Link's agent wallet, while maintaining control via spending
  approvals." https://stripe.com/blog/everything-we-announced-at-sessions-2026
- Agent payments are for US and Canadian customers. Auth is OAuth to
  `login.link.com`; the token represents one customer.
  https://docs.stripe.com/agentic-commerce/link-cli
- **Flow.** The agent creates a spend request (amount, seller, and a
  context of at least 100 characters). The person approves in the Link
  app within 30 minutes. Link returns a credential.
  https://docs.stripe.com/agentic-commerce/agents/link-agent-wallet/use-link-wallet-pay-online
- **What the agent sees.** One of three credentials: a one-time virtual
  card (full PAN, CVC, billing address), an SPT, or a Link Pay Token for
  Stripe checkouts. The docs warn that `--include card` without
  `--output-file` puts the PAN "in model context, agent transcripts, and
  log aggregation." Source as above.
- Recurring cards: `--recurring-interval` makes `--amount` a per-period
  limit at one merchant. Source as above.
- **Standing approval.** `link-cli approval-policy retrieve` shows "the
  rules that grant the current app authority to create spend requests
  without manual approval." Each rule has an action and a per-purchase
  limit. https://github.com/stripe/link-cli (README)
- **Hard caps.** $500 per spend request, $500 daily, $20,000 per 30
  days, 10 concurrent approved requests. `user-info retrieve` reports
  `agent_wallet_spend_limits` (per transaction, daily, 30-day). Source as
  above.
- The marketing page offers "approve a budget and let your agent handle
  the rest." The docs do not describe a budget object beyond the approval
  policy and recurring cards. **[unverified]** how the budget mode works.
  https://link.com/agents
- Raising an amount needs the person to approve again ("`re_authorize`").
  Source: use-link-wallet-pay-online page above.

### Order Intents

- Demoed at Sessions 2025 as a beta API where Stripe's agent browses a
  site and buys. https://stripe.com/sessions/2025/product-keynote
- Not mentioned in the Sessions 2026 recap. I found no current docs.
  **[unverified]** whether it still exists.

### Privy (Stripe) agent wallets

- "Provision wallets for agents directly from the command line" and
  track spend in a dashboard. A preconfigured policy set is on the
  roadmap. https://stripe.com/blog/everything-we-announced-at-sessions-2026

## 2. Card networks

### Visa Intelligent Commerce (VIC) and Visa rules

- VIC gives agents "network-level controls, consumer-approved purchase
  instructions, agent validation, and tokenized payment credentials."
  Agents get a token from Visa Token Service, not the PAN. Provisioning
  includes step-up verification and a passkey.
  https://developer.visa.com/capabilities/visa-intelligent-commerce ,
  https://github.com/visa/mcp
- Spending limits, categories, and time windows are said to live in the
  consumer's instruction and with the issuer. **[unverified]**: from
  secondary sources only; the VIC docs now need sign-in.
- **Visa Core Rules, edition 24 October 2026** (read from the PDF):
  - 4.1.23.2: an Agentic Payment Provider must enroll in VIC and
    register with Visa. It may not aggregate several agentic
    transactions into one. With a stored credential it must use a token
    from Visa.
  - 4.1.23.3: before transacting, it must get consent, "clearly state
    the expiration date of the Cardholder-defined payment instruction",
    and get "Cardholder acknowledgement that they are responsible for
    actions taken by the Agentic Payment Provider."
  - 4.1.23.10: "A Cardholder is responsible for any actions taken by an
    Agentic Payment Provider as part of an Agentic Transaction as if the
    Cardholder initiated the Transaction."
  - A merchant "must not require a Cardholder to waive the right to
    dispute a Transaction, including an Agentic Transaction."
  - Compelling evidence for fraud disputes now includes "login IDs for an
    Agentic Payment Provider."
  https://cis.visa.com/content/dam/VCOM/download/about-visa/visa-rules-public.pdf
- Secondary sources number the responsibility rule 4.1.24.10. The
  current PDF numbers it 4.1.23.10. **[docs conflict]** (edition drift).

### Visa Trusted Agent Protocol (TAP)

- Announced 14 October 2025 with Cloudflare. Agents sign requests with
  RFC 9421 HTTP Message Signatures, aligned with Web Bot Auth. Tags are
  `agent-browser-auth` and `agent-payer-auth`. Merchants check keys
  against a Visa-hosted directory.
  https://usa.visa.com/about-visa/newsroom/press-releases.releaseId.21716.html ,
  https://github.com/visa/trusted-agent-protocol
- TAP is identity only. It does not issue a payment token or enforce
  limits. **[unverified]** (secondary sources).

### Mastercard Agent Pay

- Announced 29 April 2025. Agentic Tokens extend MDES tokenization. Only
  registered agents can transact. **[unverified]**: the Mastercard
  framework page returned 403; claims are from secondary coverage.
  https://www.mastercard.com/global/en/news-and-trends/stories/2025/agentic-commerce-framework.html
- Mastercard's merchant guide: Verifiable Intent (open-sourced March
  2026, with Google) "serves as cryptographic evidence of what was
  authorized versus what was executed" in disputes. It uses SD-JWT
  delegation chains and is at v0.1. Agent identity uses a
  `mastercardIdToken` (OIDC-compatible JWT) bound to the request
  signature. "Never require consumers to waive dispute rights for
  agentic transactions." Refunds go to the original token.
  https://developer.mastercard.com/merchant-cloud/documentation/tutorials-and-guides/agentic-commerce-guide/22/
- The same guide does not describe consumer controls on Mastercard or
  Visa agentic tokens. Third parties say tokens can be limited by agent,
  merchant, amount, and time. **[unverified]**
- Agent Pay limits are per token and per authorization; nothing tracks
  cumulative spend across transactions. **[unverified]** (secondary).

### OpenAI ChatGPT Wallet (13 November 2026)

- Built with American Express, Mastercard, and Visa. US ChatGPT Pro
  users over 18, inside dots, at launch. The agent gets a network payment
  token with "a scoped card number, security code and expiration date
  that is specific to a given transaction." Each purchase is approved
  with a security check. Existing card protections apply.
  https://www.cnbc.com/2026/11/13/openai-chatgpt-wallet-dot-purchases.html
- Spending caps, categories, and time windows for pre-approved purchases
  are planned, not shipped. Approval uses Face ID or fingerprint. Agents
  identify to sites with Web Bot Auth. **[unverified]**: openai.com
  returned 403; from secondary coverage.
  https://openai.com/index/introducing-chatgpt-wallet/ ,
  https://ppc.land/openai-hands-its-ai-agents-a-wallet-without-the-real-card-number-in-it/

## 3. Google Agent Payments Protocol (AP2)

- v0.1 released 16 September 2025. v0.2 released 28 April 2026.
  https://github.com/google-agentic-commerce/AP2/releases
- **v0.2 renamed the mandates.** v0.1 had Intent, Cart, and Payment
  Mandates. v0.2 has a Checkout Mandate (what is bought) and a Payment
  Mandate (how it is paid), each in an open or closed form, secured as
  SD-JWTs. https://github.com/google-agentic-commerce/AP2/blob/main/docs/ap2/specification.md
- Roles: Shopping Agent, Credential Provider, Merchant, Merchant Payment
  Processor, Trusted Surface. The Credential Provider verifies the agent
  "is authorized to access this Payment Credential" and scopes it.
  Source as above.
- **Human present:** the person approves the closed mandates on a
  Trusted Surface. **Human not present:** the person signs open mandates
  with constraints. They "MUST include the agent's public key as a `cnf`
  claim." The agent then signs closed mandates with its own key. `exp`
  should be as short as the task allows. Source as above.
- **Budget is a first-class constraint.** Open Payment Mandate
  constraints include `payment.amount_range`, `payment.budget`,
  `payment.agent_recurrence` (frequency, max occurrences), allowed payee,
  allowed instrument, and execution date. For `payment.budget`, "the
  requested amount plus the total sum of amounts from previously closed
  Payment Mandates MUST be less than or equal to `max`."
  https://github.com/google-agentic-commerce/AP2/blob/main/docs/ap2/payment_mandate.md
- An agent "MUST NOT present any subsequent open Payment or Checkout
  Mandates without receiving a rejection receipt from the previous one."
  Source: specification.md.
- Disputes: mandates and receipts give "a non-repudiable picture of the
  transaction." How they are used in dispute resolution is out of scope.
  Source: specification.md.
- Google donated AP2 and Mastercard donated Verifiable Intent to the
  FIDO Alliance on 28 April 2026. FIDO plans one harmonized spec.
  **[unverified]** (press coverage).
  https://www.biometricupdate.com/202604/fido-alliance-to-start-work-on-interoperable-standards-for-agentic-commerce

## 4. Coinbase: x402 and Agentic Wallet

### x402

- HTTP 402 payments. v2: the server sends `PAYMENT-REQUIRED`, the
  client retries with `PAYMENT-SIGNATURE`, and the server returns
  `PAYMENT-RESPONSE`. A facilitator can verify and settle.
  https://github.com/coinbase/x402/blob/main/specs/x402-specification-v2.md
- Out of scope: "Client-side budget management". Budget controls for
  agents are "implementation-specific." Source as above.
- The `exact` EVM scheme uses EIP-3009 signed transfers with nonces and
  validity windows. Source as above.
- **Refunds.** The `auth-capture` scheme adds hold, capture, void,
  refund, and a client-only `reclaim` after the capture deadline. It
  notes that `exact` "offers no way to give it back."
  https://github.com/coinbase/x402/blob/main/specs/schemes/auth-capture/scheme_auth_capture.md
- Stripe accepts x402 payments (USDC on Base at launch, February 2026).
  https://docs.stripe.com/payments/machine

### Coinbase Agentic Wallet

- "You fund the wallet and set spending limits; the agent can only pay
  within those limits." https://docs.cdp.coinbase.com/x402/agentic-accounts/agentic-wallet
- Limits are "Max per call" and "Max per session", set by hand in the
  wallet UI. "Agents respect these limits but can't change them." Only
  the person can start onramp or fund transfers.
  https://docs.cdp.coinbase.com/agentic-wallet/mcp/faq ,
  https://docs.cdp.coinbase.com/agentic-wallet/mcp/mcp-tools/overview
- The CLI "enforce[s] limits before any transaction", signs in by email
  OTP, keeps keys away from the agent, and screens with KYT and OFAC.
  Networks: Base, Polygon, Solana.
  https://docs.cdp.coinbase.com/agentic-wallet/cli/welcome
- Payments "can't be reversed."
  https://docs.cdp.coinbase.com/agentic-wallet/mcp/mcp-tools/make-x402-request
- These CDP pages returned 404 to my fetcher; the quotes come from
  search extracts of the same URLs. **[unverified]** wording. What
  "session" means is not documented.
- AgentKit (the open-source SDK) has no built-in spending limits.
  **[unverified]** (https://www.openfort.io/blog/best-agent-wallets-for-developers).

## 5. Others

### Privacy.com (consumer, issuer-enforced)

- "Privacy for Agents": "set a spend limit on any Privacy Card so you
  won't be on the hook for more than you authorize your AI Agents to
  spend." Cards are issued by Patriot Bank; Lithic is the processor.
  https://agents.privacy.com/
- MCP at `https://mcp.privacy.com`, OAuth or API key, paid plan
  required. `create_card` takes `type` (`SINGLE_USE`, `MERCHANT_LOCKED`,
  `UNLOCKED`), `spend_limit`, and `spend_limit_duration` (`TRANSACTION`,
  `MONTHLY`, `ANNUALLY`, `FOREVER`). Other tools: `get_pan` ("Retrieve
  full card number, CVV, and expiration"), `pause_card`, `close_card`,
  `update_card_spend_limit`, `list_transactions`.
  https://developers.privacy.com/docs/mcp-server
- **Funding is not separate.** "Privacy directly debits your funding
  source for transactions as you make them." Funding must be a US
  checking account or debit card.
  https://support.privacy.com/hc/en-us/articles/360012148073-How-does-Privacy-work ,
  https://support.privacy.com/hc/en-us/articles/360015788853-What-can-I-use-to-fund-my-Privacy-Account
- Declines exist at card level ("exceeded your card's spend limit") and
  account level (daily, weekly, monthly).
  https://support.privacy.com/hc/en-us/articles/360041447214-Why-was-a-transaction-declined-when-I-tried-to-use-a-Privacy-Card
- **[unverified]** whether refunds restore a `FOREVER` limit.

### Lithic

- Card issuing platform. Cards "created and funded in real time" with
  per-card spend and authorization controls. Lithic began as
  Privacy.com. https://www.lithic.com/solutions/agentic-commerce
- Supports automated dispute submission for agent purchases.
  https://www.lithic.com/blog/agentic-media-buying

### Rain Agent Vaults (10 November 2026)

- Closest match to our Allowance. A vault is prefunded (bank, card, or
  stablecoin). "The card pays only with what is in the vault, and the
  vault is funded before the card pays." Rules are set "by you or by your
  user, and Rain enforces them at authorization." Card lifetimes: "An
  hour, a trip, a month, or a single purchase." Approval rules like
  "Anything over $500 needs my approval."
  https://www.rain.xyz/product/agent-vaults
- Sold to platforms, not directly to people. Cards are Visa debit from
  Cross River Bank. Rain "can generate network agentic tokens." Source as
  above.
- The agent cannot raise its own limit. **[unverified]** (press release:
  https://www.prnewswire.com/news-releases/rain-agent-vaults-bring-embedded-payments-to-ai-agents-302935044.html).

### Skyfire (KYAPay)

- Prefunded custodial agent wallet. The agent creates a `pay` or
  `kya-pay` JWT with `tokenAmount` and a seller service ID. The amount
  must be at most the wallet balance. The seller charges up to that
  amount, partially if it wants. Default expiry is 24 hours with a seller
  service ID. https://docs.skyfire.xyz/reference/create-token ,
  https://docs.skyfire.xyz/docs/token-schemas
- `kya` tokens carry agent identity. https://kyapay.org/whitepaper

### Payman

- Each agent gets its own wallet. Policies set per-transaction caps,
  daily limits, approval thresholds, and payee allowlists. USD custody is
  with Fifth Third Bank. The human operator must be verified.
  **[unverified]**: docs.paymanai.com did not resolve; from reviews.

### Amazon Buy for Me

- In-app beta from April 2025. Amazon's agent buys from brand sites
  after the person confirms each checkout. Amazon passes encrypted name,
  address, and payment details. The brand handles returns and service.
  https://www.aboutamazon.com/news/retail/amazon-shopping-app-buy-for-me-brands
- No allowance or developer API. Amazon also blocks outside agents from
  buying on Amazon. https://www.cnbc.com/2026/11/13/openai-chatgpt-wallet-dot-purchases.html

### PayPal

- Agent toolkit (`@paypal/agent-toolkit`) wraps PayPal APIs for agent
  frameworks. It is for merchants, not for spending.
  https://developer.paypal.com/ai-tools/toolkit
- Agent Ready lets Braintree merchants accept ACP and UCP payments. ACP
  uses "a secure, one-time-use payment token."
  https://docs.paypal.ai/growth/agentic-commerce/overview
- I found no PayPal consumer allowance product.

### Business agent cards (Ramp, Mercury, Brex)

- Ramp Agent Cards: the agent "uses the cards, funds, and permissions
  already available to the Ramp user who connected it" and follows each
  fund's spend limit, merchant rules, and approvals. "No separate agent
  identity is required." https://agents.ramp.com/cards
- Mercury Agent Cards (11 August 2026): made only by a human; the agent
  "cannot bypass or change" limits; over-budget charges are declined.
  Business customers only. **[unverified]** (press release:
  https://www.morningstar.com/news/business-wire/20260811063202/mercury-launches-spend-with-agent-cards-and-intelligent-budgets-for-the-ai-era).
- Brex: no first-party agent card product found. **[unverified]**
- None of these are open to an individual.

### Banks offering accounts for agents

- Stripe lists "Agent-ready Treasury accounts" with "human-in-the-loop
  confirmation for key actions."
  https://stripe.com/blog/everything-we-announced-at-sessions-2026
- I found no consumer bank offering an account in an agent's name. All
  products above put the account in the person's or business's name.

## 6. Regulation, liability, and disputes

- **Card networks put liability on the cardholder.** Visa 4.1.23.10, quoted
  above. The cardholder must acknowledge this before the first agentic
  transaction (4.1.23.3).
  https://cis.visa.com/content/dam/VCOM/download/about-visa/visa-rules-public.pdf
- **No agent dispute codes.** Agent purchases use ordinary chargeback
  reasons. Visa's dispute chapter mentions agents only as compelling
  evidence (login IDs). Same PDF. Mastercard's guide says existing
  dispute rights apply and cannot be waived.
  https://developer.mastercard.com/merchant-cloud/documentation/tutorials-and-guides/agentic-commerce-guide/22/
- **Regulation E may not help.** Reg E caps consumer loss for
  unauthorized transfers ($50 or $500). But it excludes transfers by a
  person "to whom the consumer furnished the access device", unless they
  exceed the authority given. The CFPB has issued no agent-specific
  guidance. **[unverified]**: from law-firm commentary, not CFPB text.
  https://fbtgibbons.com/the-payment-infrastructure-layer-how-network-rules-already-govern-agentic-commerce/
- **KYC sits with the person.** Link may require `ssn_verification` or
  `identity_verification` before a spend request.
  https://docs.stripe.com/agentic-commerce/agents/link-agent-wallet/use-link-wallet-pay-online
  Visa requires the provider to verify the cardholder's identity before
  storing a credential (4.1.23.3). Coinbase screens wallets with KYT and
  OFAC. No product does KYC on the agent itself; "Know Your Agent" means
  registering the agent's operator and keys.
- **Merchants may still refuse.** OpenAI warns risk systems may decline
  or cancel agent purchases. United and Amazon block agents.
  https://www.cnbc.com/2026/11/13/openai-chatgpt-wallet-dot-purchases.html
- **Crypto is final.** x402 `exact` and Coinbase Agentic Wallet payments
  cannot be reversed. Only `auth-capture` adds refund and reclaim.
