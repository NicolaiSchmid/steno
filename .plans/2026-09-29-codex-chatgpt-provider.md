# Steno: summaries through the user's ChatGPT sign-in (Codex credentials)

Status: accepted by the owner on 2026-09-29 after a written risk assessment; implementation in
the same PR.

Binding plans: scope authority [`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md);
LLM plan [`2026-09-25-llm-and-templates.md`](2026-09-25-llm-and-templates.md); app plan
[`2026-09-25-macos-app-and-release.md`](2026-09-25-macos-app-and-release.md); onboarding plan
[`2026-09-28-onboarding-vault-and-llm.md`](2026-09-28-onboarding-vault-and-llm.md). This plan
amends scope decision 4 ("LLM post-processing over any OpenAI-compatible endpoint, base URL +
API key only") by adding a second provider, and strikes "OpenAI Responses API" and "streaming"
from the LLM plan's non-goals for that provider only. Nothing else in those plans changes.
The scope's privacy line "Only transcript text goes to the LLM endpoint" stays true.

## Goal

The owner and friends already pay for ChatGPT and have the Codex CLI signed in. Summaries
should be able to run on that plan instead of a separate API key. The user picks
"ChatGPT (Codex)" as the summaries service during onboarding or in Settings, reads what that
means, and confirms. Steno then uses the OAuth tokens Codex stored on the Mac to call OpenAI's
Codex backend directly with the Responses API.

## Findings

Risk assessment given to the owner before this plan, kept here so the decision is on record:

- OpenAI has publicly endorsed third-party harnesses on a ChatGPT subscription (OpenClaw,
  OpenCode, Pi by name; Sam Altman on 2026-05-02; the Codex for Open Source page). None of this
  is in the Terms of Use. OpenAI blocks "sub2api" style redistribution of one subscription to
  many users. One person, one Mac, their own login is the tolerated case. OpenAI can change its
  mind without notice, as Anthropic did in 2026 for Claude subscriptions.
- The Codex CLI is Apache-2.0 and its wire protocol is readable. Everything below was read from
  `openai/codex` `main` on 2026-09-29 (`codex-rs/login`, `codex-rs/codex-api`,
  `codex-rs/model-provider-info`):
  - Credentials: `$CODEX_HOME/auth.json` (default `~/.codex/auth.json`, mode 0600). Keys:
    `auth_mode` (`"chatgpt"` for a ChatGPT login), `OPENAI_API_KEY` (nullable), `tokens`
    (`access_token`, `refresh_token`, `id_token`, `account_id`), `last_refresh` (RFC 3339).
    `access_token` and `id_token` are JWTs. The account id is `tokens.account_id`, falling back
    to the `id_token` claim `https://api.openai.com/auth`.`chatgpt_account_id`. The plan type is
    `chatgpt_plan_type` in the same claim; the email is the top-level `email` claim.
  - Refresh: `POST https://auth.openai.com/oauth/token`, JSON body
    `{"grant_type":"refresh_token","client_id":"app_EMoamEEZ73f0CkXaXp7hrann","refresh_token":…}`.
    Response `{id_token, access_token, refresh_token}`, all optional. Refresh tokens rotate:
    a reused one fails permanently (`refresh_token_reused`, `refresh_token_expired`,
    `refresh_token_invalidated`, or `invalid_grant` on 400). Codex refreshes when
    `last_refresh` is older than 8 days or the access token expires within 5 minutes.
  - Completions: `POST https://chatgpt.com/backend-api/codex/responses`. Headers
    `Authorization: Bearer <access_token>`, `ChatGPT-Account-ID: <account_id>`, `originator`,
    `User-Agent`, `session-id`. Body is the Responses API: `model`, `instructions`, `input`
    (items `{type:"message", role, content:[{type:"input_text", text}]}`), `tool_choice:"auto"`,
    `parallel_tool_calls:false`, `reasoning:{effort}`, `store:false`, `stream:true`,
    `include:[]`, `text:{format:{type:"json_schema", name, strict, schema}}`. Codex always
    streams; the answer comes as server-sent events `response.output_item.done` (a `message`
    item with `content:[{type:"output_text", text}]`), `response.completed` or
    `response.incomplete` (with `response.usage.input_tokens` and `output_tokens`),
    `response.failed`, and `error`.
  - Models: `GET https://chatgpt.com/backend-api/codex/models?client_version=<semver>` returns
    `{models:[{slug, display_name, visibility ("list"|"hide"|"none"), context_window,
    supported_in_api, minimal_client_version, …}]}`. The server drops every model whose
    `minimal_client_version` is above the query, so a low value returns an empty list.
- Live check on 2026-09-29 from a Linux machine with the owner's own Codex sign-in, headers
  `User-Agent: steno/0.1.0` and `originator: steno`: `GET /models?client_version=99.0.0`
  answered 200 with seven listed models (all `context_window` 272 000); `POST /responses`
  with `max_output_tokens` answered 400 `{"detail":"Unsupported parameter: max_output_tokens"}`;
  the same request without it answered 200 with the event stream above and
  `{"ok":true}`. The backend does not gate on the originator. Both open questions are closed.
- Steno today has one `LanguageModel` implementation, `OpenAICompatibleClient`
  (`Sources/StenoLLM/OpenAICompatibleClient.swift`), one secret (`SecretKey.llmAPIKey`), and
  "configured" means `LLMEndpoint(settings:) != nil` (`Sources/StenoLLM/LLMEndpoint.swift:60`),
  checked in `apps/macos/Steno/Services/Settings+Setup.swift:11`, `LLMWiring.swift`,
  `Sources/steno/Wiring.swift:93` and `DevLLM.swift:54`. There is no SSE parsing anywhere.
- The macOS app is not sandboxed (`apps/macos/Config/Base.xcconfig`), so reading `~/.codex`
  needs no entitlement.
- The Summaries pane (`apps/macos/Steno/Settings/SettingsView.swift:307`) is driven by
  `LLMPreset` (`LLMSettingsViewModel.swift:8`), which is inferred from the base URL alone.
  Onboarding reuses the same view model (`OnboardingViewModel.swift:87`) with URL, model and
  key fields (`OnboardingView.swift:291`).

## Non-goals

- Reading, copying or storing the Claude Code or Claude.ai credentials. Anthropic forbids it
  in writing and enforces it server-side.
- Copying Codex tokens anywhere else: not into the Keychain, not into `Settings`, not into
  logs. The only writer of `auth.json` besides Codex is the refresh write-back below.
- Running the Codex sign-in flow inside Steno. The user signs in with `codex login`; Steno
  only tells them to when the file is missing.
- Streaming partial output to the UI. The client buffers the event stream and returns one
  `LLMResponse`, like the endpoint client.
- Using the Codex backend with an OpenAI API key (`auth_mode` other than `chatgpt`). Those
  users pick the OpenAI preset of the endpoint provider.
- Spawning the `codex` binary. The owner chose the direct API after weighing both.

## Decisions

1. **Two providers, one enum.** `Settings.llmProvider: LLMProvider` with cases `endpoint`
   (default, today's behaviour) and `codex`. The endpoint fields (`llmBaseURL`, `llmModel`,
   `llmContextTokens`) are untouched, so switching back loses nothing. Codex gets its own
   `codexModel: String?` and `codexContextTokens: Int`. New rows load as defaults; no
   migration.
2. **Explicit confirmation gates every credential use.** `Settings.codexConfirmedAt: Date?`
   is nil until the user presses "Use my ChatGPT account" after reading the consent copy. While
   it is nil, the tokens are never sent anywhere and never refreshed: no probe, no model list,
   no pipeline pass. The one thing that happens before confirmation is the consent card's own
   account line ("Signed in as name@example.com (Plus)"), read from the file while the ChatGPT
   choice is on screen, no network. The confirmation is stored in settings, not UserDefaults,
   so it travels with the database and is visible in Settings. "Stop using ChatGPT" clears it;
   switching the service picker to a server does not, so switching back needs no second
   confirmation.
3. **Honest identification.** Requests carry `User-Agent: steno/<version>` and
   `originator: steno`. Steno does not present itself as the Codex CLI. If the backend rejects
   an unknown originator, the feature reports that error verbatim; it does not fall back to
   impersonation. Verified against the live backend during implementation (see Verification).
4. **Refresh like Codex, write back like Codex.** The credential store re-reads `auth.json`
   before every request, refreshes when the access token expires within 5 minutes or
   `last_refresh` is older than 8 days, and on a 401 once. A successful refresh is written back
   to `auth.json` atomically (temp file, 0600, rename) preserving every key Steno does not
   understand, because the old refresh token is dead after rotation and the Codex CLI would
   otherwise be signed out. A permanent refresh failure surfaces as "Sign in again with
   `codex login`". If two writers race, the loser's next read sees the winner's tokens; a
   `refresh_token_reused` error triggers one re-read before giving up.
5. **Reasoning effort follows purpose.** `low` for the cleanup passes and the probe (many
   chunks, simple task), `medium` for everything else. Neither `temperature` nor
   `max_output_tokens` is sent: the backend rejects both, so `LLMRequest.maxTokens` only shapes
   the prompts and budgets, and a cut-off answer arrives as `response.incomplete` with
   `incomplete_details.reason == "max_output_tokens"`, which maps to `.length`.
6. **Structured output modes are the same three.** `.jsonSchema` becomes
   `text.format {type: json_schema, strict}`; `.jsonObject` becomes `text.format {type:
   json_object}`; `.promptOnly` sends no `text`. The 400-driven downgrade from the endpoint
   client is reused: a 400 naming `text` or `format` downgrades the mode.
7. **The model list comes from the backend.** The Summaries pane shows a picker of models with
   `visibility == "list"` from `/models`, plus the stored value if it is not in the list. The
   default when nothing is stored is the first listed model. `context_window` from the list is
   stored as `codexContextTokens` when the user picks a model; the default is 128 000. The
   `client_version` query is a capability filter (the server hides models newer than the
   Codex version named), not an identity claim, so Steno sends a high sentinel (`99.0.0`) to
   see the whole list; the identity headers stay honest per decision 3.
8. **One `LLMEndpoint` shape for both providers.** For Codex the endpoint's `baseURL` is the
   Codex backend root, so the budgets, chunking and concurrency logic in the cleaner and
   summarizer are untouched. `LLMEndpoint(settings:)` becomes provider-aware and returns nil
   for Codex until `codexModel` is set and `codexConfirmedAt` is non-nil, so every existing
   "configured" check keeps working.
9. **Concurrency stays at 2 for Codex** (the endpoint default). Codex plan limits are shared
   with the user's coding sessions; the consent copy says so.
10. **Errors that name the plan are not retried.** A body whose `error.type` or code is
    `usage_limit_reached` or `usage_not_included`, whether on an HTTP error or inside the
    stream as `response.failed`, becomes a non-retryable `LLMError` prefixed "ChatGPT plan
    limit reached", so the meeting detail shows that rather than hanging through backoff. An
    ordinary 429 keeps the endpoint client's `Retry-After` backoff: it clears on its own.
11. **Tokens are redacted** from every error string with the existing `redact` helper,
    extended to take a list of secrets (access token, refresh token, account id).

## UX spec

### Consent copy (shared by onboarding and Settings)

Title: **Use your ChatGPT plan for summaries**

Body:

> Steno will use the sign-in that the Codex command-line tool saved on this Mac
> (`~/.codex/auth.json`) and send your meeting transcripts to OpenAI under your ChatGPT plan.
> Audio never leaves your Mac.
>
> What this means:
>
> - Summaries count against your ChatGPT plan's Codex limits, shared with your coding
>   sessions.
> - Steno refreshes the saved sign-in when it expires and writes the new one back to the same
>   file, the same way Codex does.
> - OpenAI allows tools like this today but has not promised to keep doing so. If it stops
>   working, switch to an API key or a local model in Settings.
>
> Signed in as **name@example.com** (Plus).

Buttons: **Use my ChatGPT account** (primary), **Not now**.

When the file is missing or has no ChatGPT login, the status line reads "No Codex sign-in
found. Run `codex login` in Terminal, then check again." with a **Check again** button, and
the primary button is disabled.

### Onboarding, Summaries row

A segmented picker above the existing fields: **Server or API key** | **ChatGPT (Codex)**.
The first shows today's URL, model and key fields. The second shows the consent card above.
"Use my ChatGPT account" saves `llmProvider = .codex`, `codexConfirmedAt = now`, fetches the
model list, stores the first listed model, probes, and marks the row
"Saved: <model> via ChatGPT as name@example.com (Plus)". Skip stays available.

### Settings, Summaries pane

`LLMPreset` gains **ChatGPT (Codex)** as the first hosted option. Selecting it hides the
server, model and key fields and shows:

- The consent card when `codexConfirmedAt` is nil.
- After confirmation: status row "Using ChatGPT as name@example.com (Plus)", a **Model**
  picker fed by `/models` with a **Refresh** button, the existing "Test again" status row, and
  a **Stop using ChatGPT** button that clears `codexConfirmedAt` and switches the preset back
  to the inferred endpoint preset.

The sidebar subtitle for Summaries reads "ChatGPT (Codex)" when that provider is active.

### Copy rules

No developer vocabulary in the UI beyond the file path and the `codex login` command, both of
which the user needs verbatim. "OAuth", "JWT", "Responses API" and "originator" do not appear.

## Implementation steps

1. **Settings** (`Sources/StenoCore/Model/Settings.swift`): add `LLMProvider` enum,
   `llmProvider`, `codexModel`, `codexConfirmedAt`. Tests in
   `Tests/StenoCoreTests/SettingsStoreTests.swift` for defaults and round trip.
2. **Endpoint** (`Sources/StenoLLM/LLMEndpoint.swift`): `static let codexBackendURL`,
   `init?(settings:)` provider-aware, `LLMEndpoint.codex(model:contextTokens:)`. Tests in
   `Tests/StenoLLMTests/ClientTests.swift` (settings → endpoint cases).
3. **Credential store** (`Sources/StenoLLM/Codex/CodexCredentialStore.swift`): actor over a
   `CODEX_HOME` directory. `stored()` reads `auth.json` into `CodexCredentials` (access token,
   refresh token, account id, email, plan, expiry from the JWT `exp` claim, `lastRefresh`)
   without the network. `current()` returns credentials fit to send, refreshing through an
   injected `URLSession` when due; `refreshed(ifStillUsing:)` is the 401 path and skips the
   network when the CLI has rotated the file meanwhile. Concurrent callers share one refresh.
   Write-back overlays the new tokens on the file's latest contents and preserves unknown keys. JWT
   payload decoding lives in `Sources/StenoLLM/Codex/JWTClaims.swift`. Tests in
   `Tests/StenoLLMTests/CodexCredentialStoreTests.swift` with a temp directory and the stub
   server as the token endpoint: parse, missing file, API-key-only file, expiry detection,
   refresh and write-back, unknown keys preserved, file mode 0600, permanent failure codes.
4. **Wire types and SSE** (`Sources/StenoLLM/Wire/Responses.swift`,
   `Sources/StenoLLM/Wire/ServerSentEvents.swift`): request encoding, event decoding, a parser
   that turns a buffered `text/event-stream` body into events. Tests in
   `Tests/StenoLLMTests/ServerSentEventsTests.swift`.
5. **Client** (`Sources/StenoLLM/Codex/CodexResponsesClient.swift`): `actor
   CodexResponsesClient: LanguageModel` with the same `RetryPolicy`, clock and observer shape
   as the endpoint client. `complete`, `probe()` returning `EndpointProbe` plus account email
   and plan, `listModels()`. Tests in `Tests/StenoLLMTests/CodexClientTests.swift` through
   `StubChatServer` (new scripts in `Sources/StenoLLM/Testing/Scripts.swift`): headers, body
   shape, SSE happy path, incomplete → `.length`, failed → error, 401 → refresh → retry once,
   usage limit → non-retryable, mode downgrade on 400.
6. **Wiring**: `Sources/steno/Wiring.swift` and `apps/macos/Steno/Services/LLMWiring.swift`
   pick the client by provider. `steno dev llm probe` prints the account line for Codex.
   `AppEnvironment.live()` passes the Codex home (default `~/.codex`, `CODEX_HOME` override).
7. **Settings UI**: `LLMPreset.codex`, `LLMSettingsViewModel` gains `codexStatus`,
   `codexModels`, `confirmCodex()`, `stopUsingCodex()`, `refreshCodexModels()`;
   `SummariesSettingsView` gains the consent card and model picker;
   `SettingsOverviewViewModel` subtitle. Tests in `apps/macos/StenoTests/SettingsViewModelTests.swift`.
8. **Onboarding**: segmented picker and consent card in `SummariesSetupFields`;
   `OnboardingViewModel.canSaveSummaries` and `savedLine` provider-aware. Tests in
   `apps/macos/StenoTests/OnboardingViewModelTests.swift`.
9. **Docs**: amend scope decision 4 and the LLM plan non-goals with a pointer to this plan;
   one paragraph in the macOS README's summaries section.

## Verification

- `swift test` on Linux (the `steno-swift:6.1` image) for StenoCore and StenoLLM.
- macOS CI for the app tests.
- One live check from a machine with a Codex sign-in, using the owner's own account with the
  owner's knowledge: `GET /models` and one probe completion with the honest headers from
  decision 3. Recorded in the PR description with the tokens redacted. If the backend rejects
  the honest originator, stop and report; do not ship an impersonating header.
- Manual: onboarding with and without `~/.codex/auth.json`; Settings switch to ChatGPT, confirm,
  pick a model, test, stop using, switch back; a summary re-run on an existing meeting.

## Open questions

None. Both questions raised in the first draft (whether the backend accepts an `originator`
other than `codex_cli_rs`, and what `client_version` does to `/models`) were answered by the
live check recorded under Findings.
