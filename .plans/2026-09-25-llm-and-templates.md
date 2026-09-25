# Steno v1: LLM and templates (`StenoLLM`)

Status: implementation plan, 2026-09-25, reconciled and then revised the same day after the three
reviews (program review application log). Workstream plan under
[`2026-09-25-v1-program.md`](2026-09-25-v1-program.md); scope authority
[`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md). Uses the program's types
(`LanguageModel`, `TranscriptCleaner`, `MeetingSummarizer`, `CleanupInput`, `CleanupOutput`,
`SummaryInput`, `SummaryOutput`, `SummaryDocument`, `LLMUsage`, `SummaryTemplate`) and reads template
data from StenoCore's `SummaryTemplate.bundled`.

## Goal

`StenoLLM` turns a merged, diarized transcript into what the user reads: a cleaned transcript, a
title, a template-driven structured summary in Jamie's style, decisions, tasks with assignee, priority
and due date, and speaker name suggestions. It talks to any OpenAI-compatible chat completions endpoint
configured with base URL, API key and model name, handles hour-long German/English/Denglish meetings
within a bounded context window, degrades predictably when the server ignores or rejects structured
output, reports token usage per meeting, and is fully testable against a local stub server with an
injected clock.

## Non-goals

- Streaming; the UI shows a processing state, not partial text.
- Provider-specific APIs (Anthropic Messages, Gemini, OpenAI Responses API), tool calling,
  embeddings, vision. Only `POST {baseURL}/chat/completions` and `GET {baseURL}/models`.
- Custom templates, template editing, template parsing (data is StenoCore's JSON), auto-picking a
  template from content. Four fixed templates.
- Cross-meeting context, Ask AI, translating the transcript, a per-meeting output-language override,
  on-device Apple Foundation Models.
- Exact tokenisation, pricing and cost estimation. Budgets use a byte heuristic; `LLMUsage` records
  tokens only.
- Rendering Markdown. The summary is a `SummaryDocument`; StenoCore's `SummaryMarkdown` renders it
  at display and export time with the current speaker names.
- Sending anything but text. No audio, file paths or scratchpad in v1.

## Research findings the design rests on

Verified 2026-09-25 against vendor docs unless marked otherwise.

| Server | `response_format: json_schema` | Notes |
|---|---|---|
| OpenAI, Azure OpenAI | honoured; `strict: true` is constrained decoding | strict subset: every property in `required`, `additionalProperties: false` on every object, root is an object and not `anyOf`, optional via `["string","null"]`, no `format`, `pattern`, `minLength`, `minimum`, `minItems`; max 5 nesting levels; `$defs` and recursion allowed |
| Groq | honoured on a short list (`openai/gpt-oss-20b`, `-120b`, `qwen/qwen3.8-27b` for strict) | best-effort mode may answer HTTP 400 "Generated JSON does not match the expected schema"; `json_object` works on all models; no streaming with structured output |
| OpenRouter | passed through per model and provider | fails with an error when the routed provider lacks support; `require_parameters: true` narrows routing; enforcement quality varies |
| LM Studio | honoured for any loaded model via grammar-constrained decoding | models under 7B struggle; their example sends `"strict": "true"` as a string, so `strict` is likely not read |
| Ollama (`/v1`) | docs state it works via `response_format`; request shape not shown, **unverified in practice** | Ollama recommends repeating the schema in the prompt; Ollama Cloud lacks structured output; the loaded model's default context length applies, not ours |
| Anthropic OpenAI compatibility | **ignored** (documented "Ignored") | JSON must come from the prompt; the layer is declared not production-ready |

Tokenizer-free budgeting: English prose runs about 4 bytes per token on current tokenizers; German
3.3 down to 2.8 depending on the tokenizer. Steno uses `tokens = ceil(utf8Bytes / 3.0)` for German
and mixed text, `/ 3.6` for English, a deliberate 10 to 30 percent overestimate; `usage` comes from
the server.

Jamie's shape (2026-09-24 screenshots): title `Topic: Subtopic`; summary starts with `Executive
Summary`; every bullet is `**Lead phrase**: text` with speaker names in bold; language chip `Auto
(German)`; template menu Default, Customer Discovery, Daily Standup, Interview; tasks show text,
assignee, priority filter and `Set Date`. Jamie's docs confirm `Executive Summary` plus `Full Summary`
by topic as the default and a template as context plus sections. Per-template sections are not
public; ours below.

## Design decisions

1. Two passes. Pass 1 cleanup runs per chunk, always. Pass 2 analysis is one call when the transcript
   fits the budget, else map (per-chunk notes) then reduce (one call). Two levels only; beyond that
   `transcriptTooLong`.
2. The schema is always also written into the prompt. `response_format` is a bonus for servers that
   honour it. Output is validated locally always, by decoding into the `Codable` draft types; there is
   no separate schema validator and no JSON repair heuristics. A decode failure goes once through
   `buildRepair`, then fails the call.
3. Structured output mode resolves per endpoint and is remembered: `.jsonSchema`, on HTTP 400
   mentioning `response_format` or `json_schema` `.jsonObject`, on 400 again `.promptOnly`. Silent
   ignoring (Anthropic) is caught by decoding, not mode detection.
4. The summary is structured: sections of bullets with `lead` and `text`, returned as core's
   `SummaryDocument`. People are named by known name or by the verbatim speaker label; StenoCore's
   `SummaryMarkdown` substitutes labels with current names and bolds them when rendering, so a
   renamed speaker is a re-export, never a re-run.
5. Templates are StenoCore data (`SummaryTemplate.bundled`, per template `description`, `context`; per
   section `id`, `heading`, `instructions`, `required`). Section ids and headings are core's; this
   workstream owns the `instructions` and `context` strings (edited in core's JSON) and the prompt
   wording that turns a `SummaryTemplate` into system-prompt blocks.
6. Output language is `Meeting.language` (elected by the core pipeline from tagged segments; fallback
   English); the model writes headings in that language from the English template heading.
7. Speaker names are suggestions with confidence and evidence. StenoLLM never renames a `Speaker`;
   the review sheet prefills from them.
8. `SummaryOutput.usage` and `CleanupOutput.usage` sum `usage` over every call of a meeting; the
   pipeline stores the total on `Meeting.llmUsage`.
9. The API key is never part of a `Codable` value: `LLMEndpoint` carries URL, model and limits only;
   the key goes into `OpenAICompatibleClient.init` and is redacted from every error and log.

## Public API

Core types used as defined in the program: `LanguageModel`, `TranscriptCleaner`, `MeetingSummarizer`,
`CleanupInput`, `CleanupOutput`, `SummaryInput`, `SummaryOutput`, `SummaryDocument`,
`SpeakerNameSuggestion`, `LLMRequest` (`responseFormat`, `purpose`), `LLMResponse` (`finishReason`,
`usage`), `LLMUsage`, `JSONValue`, `SummaryTemplate`, `TemplateSection`, `SecretKey.llmAPIKey`.

```swift
// Endpoint and client
public struct LLMEndpoint: Sendable, Equatable {                      // not Codable: never carries the key
    public var baseURL: URL; public var model: String
    public var contextTokens = 32_000; public var maxOutputTokens = 4_096; public var maxConcurrentRequests = 2
    public var requestTimeout: Duration = .seconds(240)
    public var structuredOutputMode: StructuredOutputMode = .auto
    public init(settings: Settings) throws }                           // baseURL, model, contextTokens from Settings
public enum StructuredOutputMode: String, Sendable, Codable { case auto, jsonSchema, jsonObject, promptOnly }
public struct RetryPolicy: Sendable, Equatable { public var maxAttempts = 3; public var baseDelay: Duration = .seconds(2); public var maxDelay: Duration = .seconds(30); public static let `default`: RetryPolicy }
public enum LLMError: Error, Sendable, Equatable {
    case http(status: Int, body: String), transport(String), timeout, rateLimited(retryAfter: Duration?)
    case unsupportedResponseFormat, invalidJSON(String), truncated, refused(String)
    case transcriptTooLong(estimatedTokens: Int, budget: Int) }        // bodies and descriptions never contain the key
public actor OpenAICompatibleClient: LanguageModel {
    public init(endpoint: LLMEndpoint, apiKey: String?, session: URLSession = .shared, retry: RetryPolicy = .default,
                clock: any Clock<Duration> = ContinuousClock())
    public func complete(_ request: LLMRequest) async throws -> LLMResponse
    public func probe() async throws -> EndpointProbe            // GET /models plus one tiny structured call
    public var resolvedMode: StructuredOutputMode { get } }
public struct EndpointProbe: Sendable, Equatable { public var reachable: Bool; public var modelListed: Bool?; public var resolvedMode: StructuredOutputMode; public var roundTrip: Duration }

// Structured output
public struct JSONSchema: Sendable, Equatable {                     // request-side builder only; the Codable draft types are the validator
    public static func object(_ properties: KeyValuePairs<String, JSONSchema>, description: String? = nil) -> JSONSchema
    public static func array(of item: JSONSchema, description: String? = nil) -> JSONSchema
    public static func string(enum cases: [String]? = nil, description: String? = nil) -> JSONSchema
    public static func integer(description: String? = nil) -> JSONSchema   // also number, boolean
    public var nullable: JSONSchema { get }
    public var jsonValue: JSONValue { get }       // for LLMRequest; emitted with additionalProperties false and every property required
    public var promptText: String { get } }       // compact schema for the prompt
public struct StructuredOutputDecoder: Sendable {  // strip fences and prose prefixes, then JSONDecoder into T; no repair heuristics
    public func decode<T: Decodable & Sendable>(_ type: T.Type, from response: LLMResponse) throws -> T }

// Budgeting and chunking (LLMUsage is StenoCore's; this module adds `+`)
public struct TokenBudget: Sendable, Equatable {
    public static func estimateTokens(_ text: String, language: Locale.Language?) -> Int
    public init(endpoint: LLMEndpoint, reservedOutputTokens: Int, promptOverheadTokens: Int)
    public var inputBudget: Int { get } }
public struct TranscriptChunk: Sendable, Equatable { public var index: Int; public var segments: [TranscriptSegment]; public var leadingContext: [TranscriptSegment]; public var estimatedTokens: Int }
public struct TranscriptChunker: Sendable, Equatable {
    public init(targetTokens: Int = 2_000, maxTokens: Int = 3_000, contextSegments: Int = 3)
    public func chunk(_ segments: [TranscriptSegment], language: Locale.Language?) -> [TranscriptChunk] }

// Pass 1 cleanup
public struct Glossary: Sendable, Equatable { public var people: [String]; public init(input: CleanupInput) }   // participants (incl. attendees) and known people
public struct CleanupPromptBuilder: Sendable {
    public static let outputSchema: JSONSchema
    public func build(chunk: TranscriptChunk, language: Locale.Language, glossary: Glossary) -> LLMRequest }
public struct CleanupDraft: Codable, Sendable, Equatable { public var segments: [Segment]
    public struct Segment: Codable, Sendable, Equatable { public var index: Int; public var text: String } }
public struct LLMTranscriptCleaner: TranscriptCleaner {
    public init(model: any LanguageModel, endpoint: LLMEndpoint, chunker: TranscriptChunker = .init())
    public func clean(_ input: CleanupInput) async throws -> CleanupOutput }

// Pass 2 analysis
public struct SummaryPromptBuilder: Sendable {
    public init(template: SummaryTemplate)                                          // from SummaryTemplate.bundled
    public func templateBlocks() -> String                                          // one block per section: heading, instructions
    public var draftSchema: JSONSchema { get }     // section ids as enum, priority enum
    public var notesSchema: JSONSchema { get }
    public func buildSingleShot(_ input: SummaryInput, segments: [TranscriptSegment]) -> LLMRequest
    public func buildMap(_ input: SummaryInput, chunk: TranscriptChunk) -> LLMRequest
    public func buildReduce(_ input: SummaryInput, notes: [ChunkNotes]) -> LLMRequest
    public func buildRepair(invalid: String, error: String) -> LLMRequest }
public struct AnalysisDraft: Codable, Sendable, Equatable {       // raw model output before post-processing
    public var title: String; public var language: String; public var sections: [Section]
    public var decisions: [String]; public var tasks: [DraftTask]; public var speakerNames: [DraftSpeakerName]
    public struct Section: Codable, Sendable, Equatable { public var id: String; public var heading: String; public var bullets: [Bullet] }
    public struct Bullet: Codable, Sendable, Equatable { public var lead: String; public var text: String } }
public struct DraftTask: Codable, Sendable, Equatable { public var text: String; public var assignee: String?
    public var priority: Priority; public var dueDate: String?             // "YYYY-MM-DD" or null, validated by Steno
    public enum Priority: String, Codable, Sendable { case low, normal, high } }
public struct DraftSpeakerName: Codable, Sendable, Equatable { public var speakerLabel: String; public var name: String?; public var confidence: Double; public var evidence: String }
public struct ChunkNotes: Codable, Sendable, Equatable { public var chunkIndex: Int; public var topics: [Topic]
    public var decisions: [String]; public var taskCandidates: [DraftTask]; public var speakerCues: [DraftSpeakerName]
    public struct Topic: Codable, Sendable, Equatable { public var topic: String; public var points: [String] } }
public struct LLMMeetingSummarizer: MeetingSummarizer {
    public init(model: any LanguageModel, endpoint: LLMEndpoint)
    public func summarize(_ input: SummaryInput) async throws -> SummaryOutput }   // AnalysisDraft -> SummaryDocument + tasks + names

// Language
public enum OutputLanguage {
    public static func resolve(meeting: Locale.Language?) -> Locale.Language   // fallback "en"
    public static func promptName(_ language: Locale.Language) -> String }     // "German", via Locale(identifier: "en_US") so goldens are machine-independent

// Testing/: StubChatServer (NWListener HTTP/1.1 on 127.0.0.1, ephemeral port, scripted responses, records requests and their timing), Scripts
```

## Prompt contracts

Pass 1 cleanup, one request per chunk, temperature 0, `maxConcurrentRequests` in flight. System:
role, meeting language, glossary (participants including calendar attendees, known people), schema,
rules: keep the exact number and order of segments; never merge, split, drop, add, shorten or
summarise; keep code-switching as spoken, never translate; fix STT misspellings of anglicisms and
product names ("Git Hub" to "GitHub"); German noun casing; glossary spelling of names; punctuation;
return text unchanged when nothing needs fixing. User: `leadingContext` segments marked read-only,
then the chunk as `[n] <speaker label>: <text>`. Validation: same count, indices `0..<n` each once,
per-segment word ratio 0.7 to 1.3. A failing chunk is retried once with the error appended, then kept
raw and listed in `failedChunks`. `rawText` always keeps the STT output.

Pass 2 analysis, temperature 0.2. System: role, output language by name, meeting date (to resolve
"next Friday"), participants with roles, speaker labels with known names, template context, one
block per template section (heading, instructions, omit when empty), schema, rules: bullets are a
short lead phrase plus one to three sentences; name people by known name, else by the speaker label
verbatim so Steno can resolve them later; decisions are things agreed, not proposals; tasks are
explicit commitments with an owner; priority `high` only when urgency was said; `dueDate` absolute or
null; suggest a speaker name only with transcript evidence (addressed by name, self-introduction) or
by elimination against the participant list, with confidence 0 to 1 and a one-line quote; title under
80 characters, `Topic: Subtopic` when natural. User: the cleaned transcript (single shot), one chunk
(map), or the `ChunkNotes` array (reduce). Post-processing in `LLMMeetingSummarizer`: drop sections
not in the template and empty non-required ones; validate `dueDate` with a strict `yyyy-MM-dd`
formatter, null on failure; match `assignee` to a participant case-insensitively, else keep
`assigneeName`; map `speakerLabel` to `Speaker.id`; build `SummaryDocument(templateID, language,
sections)`. No Markdown is produced here.

## Files

- `Package.swift`: add `StenoLLM` (depends on `StenoCore`), `StenoLLMTests`.
- `Sources/StenoLLM/LLMEndpoint.swift`, `RetryPolicy.swift`, `OpenAICompatibleClient.swift`
  (client, mode fallback, probe, key redaction, retries on the injected clock),
  `Wire/ChatCompletion.swift` (Codable request, response, error).
- `Sources/StenoLLM/StructuredOutput/JSONSchema.swift` (builder, prompt rendering),
  `StructuredOutputDecoder.swift` (fence and prefix stripping, decode).
- `Sources/StenoLLM/Budget/TokenBudget.swift`, `TranscriptChunker.swift`.
- `Sources/StenoLLM/Cleanup/CleanupPromptBuilder.swift`, `CleanupDraft.swift`,
  `LLMTranscriptCleaner.swift`, `Glossary.swift`.
- Template instruction text, written by this workstream into StenoCore's `Resources/Templates/*.json`
  in a PR against core (only `instructions` and `context` change; ids and headings are core's from
  its step 6): `default` Executive Summary (required), Full Summary by topic, Open Questions;
  `customer-discovery` Customer Context, Problems and Pain Points, Current Workflow and Tools,
  Reactions and Buying Signals, Objections and Risks, Next Steps; `daily-standup` Progress Since Last
  Standup, Plans Until Next Standup, Blockers and Help Needed, Announcements (bullet leads are person
  names); `interview` Candidate Background, Role Fit and Experience, Skills Assessment, Motivation and
  Culture, Candidate Questions, Assessment and Recommendation, Next Steps.
- `Sources/StenoLLM/Summary/SummaryPromptBuilder.swift`, `AnalysisDraft.swift`,
  `LLMMeetingSummarizer.swift` (single shot or map and reduce, post-processing to `SummaryOutput`).
- `Sources/StenoLLM/Language/OutputLanguage.swift`.
- `Sources/StenoLLM/Testing/StubChatServer.swift`, `Scripts.swift`: ok, 429 with `Retry-After`, 500
  then ok, 400 on `response_format`, fenced JSON, invalid JSON, `finish_reason: length`, slow.
- `Sources/steno/Commands/DevLLMCommands.swift`: `steno dev llm probe|cleanup <transcript.json>|summarize
  <transcript.json> --template <id>`; prints usage; key from `FileSecretStore` or `STENO_LLM_API_KEY`.
- `Tests/StenoLLMTests/{Client,Retry,StructuredDecoder,JSONSchemaStrict,TokenBudget,TranscriptChunker,
  Cleanup,Summary,PromptSnapshot,LiveEndpoint}Tests.swift`; goldens through StenoCore's `Snapshot`.
- `Tests/Fixtures/llm/transcripts/denglish-standup.json` (24 synthetic segments, three speakers,
  deliberate STT errors, fixed ids and dates), `customer-call-60min.json` (about 900 segments,
  generated once by `steno dev fixtures generate --llm` with a fixed seed, committed, in
  `MANIFEST.sha256`; forces map and reduce at an 8k budget); `Tests/Fixtures/llm/text/de-1000-words.txt`;
  `Tests/Fixtures/llm/prompts/*.txt` (golden prompts); `Tests/Fixtures/llm/responses/*.json` (canned
  server bodies, also used by `StenoEndToEndTests`).

## Steps

Each step is at most one day and ends with a tagged check a reviewer can run.

1. Target, wire types, stub server in `Testing/`. Check `[ci]`: `StubChatServerTests` accept a POST,
   return a scripted body and record the parsed request.
2. Client, retry, timeout, errors. Bearer auth, per-attempt timeout, retry on 408, 429, 5xx,
   transport errors and timeouts with `Retry-After`, no retry on other 4xx, cancellation, key
   redaction. Check `[ci]`: `ClientTests` and `RetryTests` on `ManualClock` cover "429 then 200
   honours Retry-After" (clock advanced, no real sleep), "three 500s throw `http(500)`", "401 not
   retried", "cancel throws `CancellationError` and the stub server recorded exactly one request";
   `testRedaction`: `String(describing:)` of every `LLMError` and the recorded request exclude the key.
3. Structured output. Fence and prefix stripping, mode fallback on 400, `truncated` on
   `finish_reason: length`, `refused`. Check `[ci]`: `JSONSchemaStrictTests` walks the literal
   `draftSchema`, `notesSchema` and `outputSchema` JSON and asserts every object has
   `additionalProperties: false`, all properties required, depth <= 5, no banned keywords; decoder
   tests accept fenced and prefixed JSON and throw `invalidJSON` on truncated JSON; a scripted 400
   flips `resolvedMode`.
4. Budget and chunker. Check `[ci]`: `llm/text/de-1000-words.txt` estimates between 330 and 500
   tokens; the 60-minute fixture yields 6 to 12 chunks that concatenate back to the input in order;
   no chunk exceeds `maxTokens` unless a single segment does.
5. Template instruction text and prompt blocks. PR against core editing only `instructions` and
   `context` in the four JSON files, plus `templateBlocks()` here. Check `[ci]`:
   `SummaryTemplate.bundled` still yields the section ids of core's step 6; core's template snapshot
   is updated in the same PR with a sentence why; `templateBlocks()` snapshots for all four match.
6. Cleanup pass. Check `[ci]`: `CleanupTests` preserve count and order, keep `rawText`, mark a
   wrong-count chunk failed after one retry, never exceed `maxConcurrentRequests` (server records
   overlap); `PromptSnapshotTests` match `prompts/cleanup-*.txt` (fixed meeting date, `en_US` names).
7. Summary pass, single shot. Check `[ci]`: `SummaryOutput.summary.sections.first?.id == "executive-summary"`,
   every bullet has a non-empty `lead`, a bullet text contains the verbatim label `Speaker 2` and
   core's `SummaryMarkdown.render` turns it into `**Nicolai**` once the speaker is confirmed; an
   unknown section id is dropped; `dueDate` "next Friday" becomes nil; snapshots for all four templates.
8. Map and reduce. Check `[ci]`: at `contextTokens: 8_000` the 60-minute fixture issues N map calls
   then one reduce call (server records `purpose` order); at 32k one call; at 4k `transcriptTooLong`
   with the transcript untouched.
9. Names, tasks, language, usage. Check `[ci]`: `Speaker 2` maps to the right UUID, a suggestion under
   0.3 confidence is dropped; meeting `de` puts `German` in the prompt and a nil language puts
   `English`; `SummaryOutput.usage` equals the sum of the scripted server `usage` bodies.
10. CLI, live test, probe matrix, end-to-end. `steno dev llm probe|cleanup|summarize`,
    `LiveEndpointTests`, run spike 1 and replace "unverified" in the table above;
    `Tests/StenoEndToEndTests` swaps `PassthroughCleaner` and `FakeSummarizer` for
    `LLMTranscriptCleaner` and `LLMMeetingSummarizer` on `StubChatServer` fed from `llm/responses/`.
    Check `[ci]`: the end-to-end test passes and `Meeting.llmUsage` equals the summed scripted usage.
    `[opt-in: STENO_LLM_TESTS]`: `swift test --filter LiveEndpointTests` passes on one real endpoint.
    `[manual]`: `steno dev llm probe` against a local server prints reachability, mode and round trip.

## Tests

Unit `[ci]`, no network, stub server on loopback only: client (auth, body per mode, retry matrix on
`ManualClock`, timeout, cancel, usage, redaction); structured output (literal-schema walk, fence and
prefix stripping, truncation, refusal, mode fallback); budget and chunker (bounds per language,
packing, order, oversized segment); cleanup (count and order, word ratio, raw fallback, concurrency,
glossary present); summary (section filtering, date and assignee validation, speaker mapping, path
selection, usage sum). Golden prompt snapshots for every builder, four templates, two languages,
through StenoCore's `Snapshot`; suites that touch `ModelHub`-like globals or the environment are
`.serialized`; a snapshot diff in a PR is a reviewed prompt change.

Integration `[opt-in: STENO_LLM_TESTS]`: `LiveEndpointTests` runs with `STENO_LLM_BASE_URL`,
`STENO_LLM_MODEL` and optional `STENO_LLM_API_KEY`: Denglish fixture through cleanup and the Default
template; asserts segment count preserved, output decodes, a bullet, a title, `usage`; else skipped
with a message naming the switch.

Manual `[manual]`, a human on a Mac: `steno dev llm summarize` on a real personal Denglish transcript
against LM Studio or Ollama on the same Mac and one hosted endpoint; render with `steno export` and
open the Markdown in Obsidian; confirm bullets read like Jamie's, names are bold, no cleaned sentence
was shortened, and reported token usage matches the provider dashboard within 10 percent.

Reviewer trap: a `prompts/*.txt` golden diff without a sentence in the PR; the 400-on-`response_format`
script removed; any `Codable` type gaining an `apiKey` property.

## Spikes

1. Structured output capability matrix (informs step 3, recorded in step 10). Run the real
   `draftSchema` against Ollama, LM Studio, OpenRouter, Groq and the Anthropic compatibility endpoint;
   record resolved mode, `strict` acceptance, decode rate. Success: every server decodes without the
   repair call in 9 of 10 runs.
2. Cleanup fidelity (before step 6 is done). Denglish fixture plus one real transcript, one local 7 to
   9B model and one hosted model. Success: count preserved in every chunk, word ratio 0.9 to 1.1 in
   95 percent of segments. This is the cleanup half of scope spike 2.
3. Map and reduce quality (before step 8). Same meeting at 32k single shot and 8k map-reduce; success:
   no single-shot decision or task missing after reduce.

## Needs from other workstreams

- StenoCore: the program's types as listed under Public API; `SummaryTemplate.bundled`;
  `SummaryMarkdown.render`; `Settings.llmBaseURL`, `llmModel`, `llmContextTokens`;
  `SecretKey.llmAPIKey`; `ManualClock`; `Tests/StenoEndToEndTests`; a PR review slot for the
  template instruction text.
- Speech and speakers (`SpeechEngine`, `Diarizer`, `SpeakerMemory`): `speakerID` on every segment,
  stable `Speaker.clusterLabel` ("Speaker 1"); `RawSegment.language` tagged so core can elect
  `Meeting.language`.
- Adapters (`Destination`): render the summary through core's `SummaryMarkdown` (headings start at
  level 2); use `MeetingTask.assigneeName` when `assigneePersonID` is nil.
- macOS app (`SecretStore`): API key from Keychain into `OpenAICompatibleClient.init`; settings UI for
  base URL, model, context tokens; review sheet prefills from `SpeakerNameSuggestion`; renaming a
  speaker re-exports (no LLM re-run).

## Deferred

- Pricing and cost estimation (`LLMPricing`, `CostEstimator`, per-million prices in settings); token
  usage is recorded, money is not.
- Per-meeting output-language override (`Meeting.summaryLanguage`).
- User product glossary for the cleanup pass; names come from participants and known people only.
- Markdown template parser and catalog; templates are StenoCore JSON.
- JSON repair heuristics (trailing commas, truncated tails) if a server family turns out to need them.

## Deviations (implementation)

Recorded while implementing this plan in PR #5 (`feat/llm-templates`), 2026-09-25.

- `StubChatServer` uses POSIX sockets and one thread per connection instead of `NWListener`, so
  the same server runs on the Linux container used for iteration and on macOS CI. Accepted sockets
  opt out of SIGPIPE (`SO_NOSIGPIPE` / `MSG_NOSIGNAL`); Darwin otherwise kills the test process on
  the first write to a peer that closed early, which is how the first macOS run died.
- `OpenAICompatibleClient.init` takes an optional `observer: (LLMClientEvent) -> Void`. Tests on
  `ManualClock` need to know when the backoff sleep begins (the per-attempt timeout is also a
  sleeper on the same clock), and the CLI prints retries and mode changes with `--verbose`.
- Every request carries an `X-Steno-Purpose` header with `LLMRequest.purpose`, so the stub server
  (and any proxy) can tell `cleanup`, `cleanup-retry`, `summary`, `summary-map`, `summary-reduce`,
  `*-repair` and `probe` apart. The wire format has no field for it.
- `LLMError` gained `notConfigured(String)` for `LLMEndpoint(settings:)`. `rateLimited` is thrown
  for 429; `Retry-After` in seconds is honoured and clamped to `maxDelay`, the HTTP-date form falls
  back to the backoff. Retries are deterministic (no jitter).
- The per-attempt timeout is raced on the injected clock; `URLRequest.timeoutInterval` is a
  wall-clock backstop at twice the value. A `URLResponse` never crosses a task boundary (not
  `Sendable` on Darwin).
- `JSONSchema.promptText` is a compact typed shape (`"id": "a" | "b"`, `string | null`, trailing
  comments), not JSON Schema, which is shorter and easier for small models; `jsonValue` is the
  strict schema for `response_format`. The strict walker counts nesting for containers only, as
  OpenAI's five-level limit does.
- `CleanupPromptBuilder.build` takes `labels: SpeakerLabels` in addition to the plan's
  parameters; the chunk carries speaker ids, not labels. The word-count check also accepts a
  difference of one word, so a misheard two-word product name may become one word.
- Answer-quality failures (`invalidJSON`, `truncated`, `refused`, validation) fall back to raw text
  after one retry; HTTP and transport errors propagate so the pipeline marks the stage failed and
  keeps the raw transcript instead of silently shipping an uncleaned one after burning retries.
- `de-1000-words.txt` holds 1000 words and the check asserts 1.2 to 2.5 tokens per word. The
  plan's "330 to 500 tokens" corresponds to about 1000 bytes, not 1000 words.
- The 60-minute fixture is produced by `SyntheticTranscript` in `Sources/StenoLLM/Testing/`
  (seeded SplitMix64) and pinned by `LLMFixturesTests`, not by core's `steno dev fixtures
  generate --llm` and `MANIFEST.sha256`; the transcript JSON files are compared as decoded values
  because Darwin and Linux Foundation may print the same Double differently. Both transcript
  fixtures are `MeetingExport` files (`meeting.json`), which is also what `steno dev llm
  cleanup|summarize` read, so `steno export` output feeds the CLI directly.
- `SummaryOutput.language` is the resolved output language (meeting language, else `en`), which
  the pipeline then writes to `Meeting.language`; the model's own `language` field is ignored.
- Map-reduce reserves a quarter of the context (at most `maxOutputTokens`) for the answer and
  refuses before the first call when `chunks * 200` estimated notes tokens exceed the input budget.
- `steno dev llm` is one command group with `probe`, `cleanup <meeting.json>` and `summarize
  <meeting.json> --template <id>` (the workstream brief spelled it `llm-probe`). Keys come from
  `STENO_LLM_API_KEY` or `<support directory>/secrets.json` through `FileSecretStore`, never from a
  flag. `Wiring.llmComponents(settings:)` swaps the fakes for the real passes when
  `Settings.llmBaseURL` and `llmModel` are set; `stenoTests` and `StenoEndToEndTests` depend on
  `StenoLLM` for the stub server.
- Spikes 1 to 3 (structured output matrix, cleanup fidelity, map-reduce quality) need a real
  endpoint and were not run in this PR; `LiveEndpointTests` behind `STENO_LLM_TESTS=1` is the
  harness for them and the capability table above keeps its "unverified" marker for Ollama.
- Simplify pass on the same PR, against the Public API above: `StructuredOutputMode.auto` is
  gone (it behaved exactly like `.jsonSchema`, the fallback runs from any starting mode);
  `LLMEndpoint.init?(settings:)` is failable instead of throwing, so `LLMError.notConfigured` and
  `isConfigured` are gone; `LLMError.unsupportedResponseFormat` was never thrown and is gone;
  `Glossary` is `CleanupInput.glossary: [String]`; `CleanupValidator` is
  `CleanupDraft.problems(against:)` plus `orderedTexts`, so the module has one error type;
  `StructuredOutputDecoder` is a namespace with a static `decode`; `buildSingleShot(_:)` takes
  the input alone and `buildRepair(for:schema:invalid:error:)` is static and derives response
  format, token ceiling and purpose from the request it repairs; the wire types are internal.
- Testing pass on the same PR. `OpenAICompatibleClient.wallClockBackstop(for:)` is the one
  place `URLRequest.timeoutInterval` is computed (twice the clock timeout, at least 30 s), so a
  test can pin it without waiting 30 s; a `URLProtocol` failing with `URLError.timedOut` proves
  the transport timeout is reported and retried like the clock one. `LLMTranscriptCleaner` now
  treats a refusal (`LLMError.refused`, thrown by the client for OpenAI's `refusal` field) like
  the other answer-quality failures: one retry with the reason, then raw text and
  `failedChunks`, one request counted; before, a refusal propagated and failed the stage, against
  the deviation above. `StubChatServer.stop()` is idempotent and returns once the accept thread
  has closed the listening descriptor: the `deinit` after an explicit `stop()` used to `shutdown`
  the descriptor number again, which by then could be another socket of the same test process
  (a live URLSession connection or another server), the source of the intermittent "Empty reply
  from server" transport errors under `--parallel`. Still untestable on CI: the wall-clock backstop actually firing (30 s of
  wall time), `Retry-After` in HTTP-date form (parsed as nil by design), and anything a real model
  does (spikes 1 to 3, `LiveEndpointTests`). Known and reproduced on the Linux container only:
  `swift test --parallel` stalls when a task group cancels a sibling that is entering
  `URLSession.data(for:)` (swift-corelibs-foundation deadlocks between `CancelState.cancel()` ->
  `URLSessionTask.cancel()` -> `workQueue.sync` and the `data(for:)` continuation body's
  `DispatchQueue.sync`); `CleanupTests.transportFailuresPropagateInsteadOfFallingBackToRaw` is
  the usual trigger and every later network test then hangs on `URLSession.shared`. Darwin's
  URLSession does not have the bug, so CI on the Mac is unaffected; the loop is `--filter` per
  suite on Linux.

### Review application (PR #5, after the correctness, elegance and testing passes)

Applied in three commits (`ee927ad` correctness majors, `1aa1afb` correctness minors, `5904088`
elegance), each behaviour change with a test that failed before it, iterated on the Linux
container (`steno-swift:6.1`, one suite per `--filter`) and confirmed on `macos-15`. Departures
from the text above:

- `Retry-After` is honoured only as a finite, non-negative number of seconds, capped at an hour
  before it becomes a `Duration` (`Duration.seconds(Double)` traps past about 1.7e20 s and
  `Double("inf")` parses); anything else, the HTTP-date form included, falls back to the backoff.
- `probe()` throws whatever the probe completion throws, not only 401/403: a base URL without
  `/v1`, a model the server does not know, a non-JSON 200 all fail the probe. A returned
  `EndpointProbe` means both passes can run, so it has no `reachable` field; `GET /models` failing
  is still tolerated (`modelListed` nil). `steno dev llm probe --json` prints
  `{modelListed, structuredOutput, roundTripMilliseconds}`, and with `--base-url` and `--model`
  the settings are not read and no database is opened.
- Map-reduce: the map ceiling is each chunk's share of the input budget
  (`TokenBudget.mapNotesOutputTokens(chunkCount:)`, at most 1 500, at least 256), sent as
  `max_tokens` and used by the up-front check, so both checks agree; the map prompt carries a
  matching length rule ("at most N points in total", one point per 60 tokens, at least three).
  The 200-tokens-per-chunk estimate above is superseded. At 8k the ten chunks get about 480
  tokens each.
- The client tolerates OpenAI's reasoning models without model-name sniffing: a 400 whose
  `error.param` is `max_tokens` is resent with `max_completion_tokens`, one naming `temperature`
  is resent without it, both remembered per client like the structured output mode and reported
  as `LLMClientEvent.parameterRejected`. A repeated rejection of the same parameter, or any other
  `param`, is a plain HTTP 400.
- `usage.prompt_tokens` and `completion_tokens` are optional on the wire; a null or partial usage
  block counts as one request with zero tokens instead of failing an intact answer three times.
- A ``` inside the JSON is content: the decoder treats a fence as Markdown only when it opens
  before the first `{` or `[`, and the last fence closes the block.
- `SummaryOutput.language` is the meeting's tag as elected, nil included, so an untagged meeting
  is not persisted as English (the deviation above that made it the resolved language is
  superseded); `SummaryDocument.language` stays resolved for the renderer.
- A section the model split into two blocks with one id is merged in order under the first
  non-blank heading.
- Budget policy lives in `LLMBudgetPolicy` (`Budget/BudgetPolicy.swift`), and `LLMEndpoint`
  derives `cleanupChunkBudgetTokens` and `summaryReservedOutputTokens` from it, so the app can
  show the derived budgets next to `llmContextTokens`. `LLMMeetingSummarizer.reservedOutputTokens`
  and `notesTokensPerChunk` are gone.
- The heading-translation sentence is `SummaryPromptBuilder.headingsRule` and follows the
  language line only in the single-shot and reduce prompts, where the template sections follow;
  the map prompt no longer carries an instruction it cannot satisfy. Goldens: the map prompt only.
- Post-processing is `AnalysisDraft.summaryOutput(for:usage:minimumConfidence:)`
  (`Summary/AnalysisDraft+Output.swift`), pure over the draft as `CleanupDraft.problems(against:)`
  is for pass 1. `DraftTask.priority` is core's `TaskPriority`. `AnalysisDraft.language` is gone
  from the type, the schema and the fixtures (it was requested and never read); the single-shot,
  reduce and repair goldens lose that one schema line.
- "Omit when empty" is said once, by the builder; the sentence left `default.json` and
  `daily-standup.json`, so core's template goldens and the prompts embedding them changed.
- Public surface nobody outside the module used is internal or gone (`SpeakerLabels.ordered`
  and its `==`, `SpeakerLabels.unknown`, `OutputLanguage.fallback`,
  `TranscriptLines.render(startIndex:)`, `JSONSchema.Property`/`properties`,
  `TokenBudget.bytesPerToken`, `RecordedRequest.inFlightOnArrival`); the wall-clock backstop is
  set in `perform`, and the probe schema goes through `JSONSchema` so the strict walker covers it.
- The Linux-only `swift test --parallel` stall is a known environment issue, not a product
  defect: a lock-order deadlock inside FoundationNetworking between the Swift task status lock
  (held while a task group cancels its children) and `workQueue.sync` in
  `URLSessionTask.cancel()`. The two deadlocked threads of the lldb dump are in
  [`reviews/2026-09-25-pr5-linux-stall-backtrace.txt`](reviews/2026-09-25-pr5-linux-stall-backtrace.txt);
  Darwin's `cancel()` is asynchronous and the same run passed 14 of 14 times on a Mac.

Follow-ups recorded, not applied: neighbour-merge detection in the cleanup word check (a cleaned
segment containing an adjacent original verbatim) and a tighter ratio; on a `finish_reason:
length` cleanup retry, a larger `maxTokens` without the truncated echo, or a split chunk; the
cleanup request size at 4k contexts (`max_tokens` capped by what the context still holds); one
retry at the endpoint ceiling for a truncated summary; keep-alive in the stub server (it answers
`Connection: close`, so URLSession's pooled-connection path is never exercised);
`CleanupPromptBuilder.init(input:maxOutputTokens:)` mirroring the summary builder; `maxTokens` out
of the prompt golden header; a `systemPrompt(for:)` so the budget is measured without rendering
the transcript; splitting `SummaryTests.singleShotBuildsTheSummaryOutputFromTheDraft` and the
first CLI test; `OutputLanguage.resolve(meeting:)` renamed for its `LanguageTag?` argument.
