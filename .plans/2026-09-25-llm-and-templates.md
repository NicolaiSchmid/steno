# Steno v1: LLM and templates (`StenoLLM`)

Status: implementation plan, 2026-09-25, nothing implemented yet. Workstream
plan under [`2026-09-25-v1-program.md`](2026-09-25-v1-program.md); scope
authority [`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md). Uses
the types in [`2026-09-25-core-foundation.md`](2026-09-25-core-foundation.md)
and requests program changes in the last section.

## Goal

`StenoLLM` turns a merged, diarized transcript into what the user reads: a
cleaned transcript, a title, a template-driven summary in Jamie's style,
decisions, tasks with assignee, priority and due date, and speaker name
suggestions. It talks to any OpenAI-compatible chat completions endpoint
configured with base URL, API key and model name, handles hour-long
German/English/Denglish meetings within a bounded context window, degrades
predictably when the server ignores or rejects structured output, reports
token usage and estimated cost per meeting, and is fully testable against a
local stub server.

## Non-goals

- Streaming; the UI shows a processing state, not partial text.
- Provider-specific APIs (Anthropic Messages, Gemini, OpenAI Responses API),
  tool calling, embeddings, vision. Only `POST {baseURL}/chat/completions`
  and `GET {baseURL}/models`.
- Custom templates, template editing, auto-picking a template from content
  (Jamie "auto-apply"). Four fixed templates.
- Cross-meeting context, Ask AI, translating the transcript, on-device Apple
  Foundation Models (a later `LanguageModel` implementation).
- Exact tokenisation. Budgets use a byte heuristic plus server `usage`.
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

Tokenizer-free budgeting: English prose runs about 4 bytes per token on
current tokenizers; German 3.3 down to 2.8 depending on the tokenizer. Steno
uses `tokens = ceil(utf8Bytes / 3.0)` for German and mixed text, `/ 3.6` for
English, a deliberate 10 to 30 percent overestimate; cost uses server `usage`.

Jamie's shape (2026-09-24 screenshots): title `Topic: Subtopic`; summary
starts with `Executive Summary`; every bullet is `**Lead phrase**: text`
with speaker names in bold; language chip `Auto (German)`; template menu
Default, Customer Discovery, Daily Standup, Interview; tasks show text,
assignee, priority filter and `Set Date`. Jamie's docs confirm `Executive
Summary` plus `Full Summary` by topic as the default and a template as
context plus sections. Per-template sections are not public; ours below.

## Design decisions

1. Two passes. Pass 1 cleanup runs per chunk, always. Pass 2 analysis is one
   call when the transcript fits the budget, else map (per-chunk notes) then
   reduce (one call). Two levels only; beyond that `transcriptTooLong`.
2. The schema is always also written into the prompt. `response_format` is
   a bonus for servers that honour it. Output is validated locally always.
3. Structured output mode resolves per endpoint and is remembered:
   `.jsonSchema`, on HTTP 400 mentioning `response_format` or `json_schema`
   `.jsonObject`, on 400 again `.promptOnly`. Silent ignoring (Anthropic) is
   caught by validation, not mode detection.
4. The summary is structured JSON (sections of bullets with `lead` and
   `text`) rendered to Markdown by Steno: Jamie's bold-lead style becomes
   deterministic and tests can assert on structure.
5. Templates are Markdown resources with a restricted frontmatter parsed by
   hand (no YAML dependency). Core foundation puts the same data as JSON in
   `StenoCore`; see requested change 1. Either owner must carry per section
   `id`, `heading`, `instructions`, `required`; per template `description`,
   `context`.
6. Output language is the meeting language unless the meeting carries an
   override; the model writes headings in that language from the English
   template heading.
7. Speaker names are suggestions with confidence and evidence. StenoLLM
   never renames a `Speaker`; the review sheet prefills from them.
8. Cost is `usage` summed over every call of a meeting, priced with
   user-entered per-million-token prices; local servers report tokens only.

## Public API

Core types used as defined in the core foundation plan: `LanguageModel`,
`TranscriptCleaner`, `MeetingSummarizer`, `SummaryInput`, `SummaryOutput`,
`LLMRequest`, `LLMMessage`, `LLMResponse`, `JSONValue`, `SummaryTemplate`,
`TemplateSection`, `SecretKey.llmAPIKey`. Deltas are under "Needs".

```swift
// Endpoint and client
public struct LLMEndpoint: Sendable, Codable, Equatable {
    public var baseURL: URL; public var apiKey: String?; public var model: String
    public var contextTokens = 32_000; public var maxOutputTokens = 4_096; public var maxConcurrentRequests = 2
    public var requestTimeout: Duration = .seconds(240); public var pricing: LLMPricing?
    public var structuredOutputMode: StructuredOutputMode = .auto }
public enum StructuredOutputMode: String, Sendable, Codable { case auto, jsonSchema, jsonObject, promptOnly }
public struct LLMPricing: Sendable, Codable, Equatable { public var inputPerMillion: Decimal; public var outputPerMillion: Decimal; public var currency: String }
public struct RetryPolicy: Sendable, Equatable { public var maxAttempts = 3; public var baseDelay: Duration = .seconds(2); public var maxDelay: Duration = .seconds(30); public static let `default`: RetryPolicy }
public enum LLMError: Error, Sendable, Equatable {
    case http(status: Int, body: String), transport(String), timeout, rateLimited(retryAfter: Duration?)
    case unsupportedResponseFormat, invalidJSON(String), schemaMismatch([String]), truncated, refused(String)
    case transcriptTooLong(estimatedTokens: Int, budget: Int), cancelled }
public actor OpenAICompatibleClient: LanguageModel {
    public init(endpoint: LLMEndpoint, session: URLSession = .shared, retry: RetryPolicy = .default)
    public func complete(_ request: LLMRequest) async throws -> LLMResponse
    public func probe() async throws -> EndpointProbe            // GET /models plus one tiny structured call
    public var resolvedMode: StructuredOutputMode { get } }
public struct EndpointProbe: Sendable, Equatable { public var reachable: Bool; public var modelListed: Bool?; public var resolvedMode: StructuredOutputMode; public var roundTrip: Duration }

// Structured output
public struct JSONSchema: Sendable, Equatable {
    public static func object(_ properties: KeyValuePairs<String, JSONSchema>, description: String? = nil) -> JSONSchema
    public static func array(of item: JSONSchema, description: String? = nil) -> JSONSchema
    public static func string(enum cases: [String]? = nil, description: String? = nil) -> JSONSchema
    public static func integer(description: String? = nil) -> JSONSchema   // also number, boolean
    public var nullable: JSONSchema { get }
    public func validateStrict() throws          // all required, additionalProperties false, depth <= 5, no banned keywords
    public var jsonValue: JSONValue { get }       // for LLMRequest
    public var promptText: String { get } }       // compact schema for the prompt
public struct StructuredOutputDecoder: Sendable {  // extract from fences and prefixes, repair, validate, decode
    public init(schema: JSONSchema)
    public func decode<T: Decodable & Sendable>(_ type: T.Type, from response: LLMResponse) throws -> T }

// Budgeting and chunking
public struct LLMUsage: Sendable, Codable, Equatable { public var promptTokens: Int; public var completionTokens: Int; public var requests: Int; public static func + (lhs: Self, rhs: Self) -> Self }
public struct TokenBudget: Sendable, Equatable {
    public static func estimateTokens(_ text: String, language: Locale.Language?) -> Int
    public init(endpoint: LLMEndpoint, reservedOutputTokens: Int, promptOverheadTokens: Int)
    public var inputBudget: Int { get } }
public struct TranscriptChunk: Sendable, Equatable { public var index: Int; public var segments: [TranscriptSegment]; public var leadingContext: [TranscriptSegment]; public var estimatedTokens: Int }
public struct TranscriptChunker: Sendable, Equatable {
    public init(targetTokens: Int = 2_000, maxTokens: Int = 3_000, contextSegments: Int = 3)
    public func chunk(_ segments: [TranscriptSegment], language: Locale.Language?) -> [TranscriptChunk] }

// Pass 1 cleanup
public struct Glossary: Sendable, Equatable { public var people: [String]; public var products: [String]; public init(context: CleanupContext) }
public struct CleanupPromptBuilder: Sendable {
    public static let outputSchema: JSONSchema
    public func build(chunk: TranscriptChunk, language: Locale.Language, glossary: Glossary) -> LLMRequest }
public struct CleanupDraft: Codable, Sendable, Equatable { public var segments: [Segment]
    public struct Segment: Codable, Sendable, Equatable { public var index: Int; public var text: String } }
public struct LLMTranscriptCleaner: TranscriptCleaner {
    public init(model: any LanguageModel, endpoint: LLMEndpoint, chunker: TranscriptChunker = .init())
    public func clean(_ segments: [TranscriptSegment], context: CleanupContext) async throws -> CleanupResult }

// Templates (resource side; see decision 5)
public enum TemplateParser { public static func parse(_ markdown: String) throws -> SummaryTemplate }
public struct TemplateCatalog: Sendable { public static let builtIn: TemplateCatalog; public var all: [SummaryTemplate]; public func template(id: String) -> SummaryTemplate? }

// Pass 2 analysis
public struct SummaryPromptBuilder: Sendable {
    public init(template: SummaryTemplate)
    public var draftSchema: JSONSchema { get }     // section ids as enum, priority enum
    public var notesSchema: JSONSchema { get }
    public func buildSingleShot(_ input: SummaryInput, segments: [TranscriptSegment]) -> LLMRequest
    public func buildMap(_ input: SummaryInput, chunk: TranscriptChunk) -> LLMRequest
    public func buildReduce(_ input: SummaryInput, notes: [ChunkNotes]) -> LLMRequest
    public func buildRepair(invalid: String, errors: [String]) -> LLMRequest }
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
public struct SummaryRenderer: Sendable { public func render(_ draft: AnalysisDraft, template: SummaryTemplate, boldNames: [String]) -> String }
public struct LLMMeetingSummarizer: MeetingSummarizer {
    public init(model: any LanguageModel, endpoint: LLMEndpoint)
    public func summarize(_ input: SummaryInput) async throws -> SummaryOutput }

// Language and cost
public enum OutputLanguage {
    public static func resolve(meeting: Locale.Language?, override: Locale.Language?) -> Locale.Language   // fallback "en"
    public static func promptName(_ language: Locale.Language) -> String }                                 // "German"
public struct CostEstimate: Sendable, Codable, Equatable { public var amount: Decimal?; public var currency: String?; public var usage: LLMUsage; public var isPreflight: Bool }
public struct CostEstimator: Sendable {
    public init(pricing: LLMPricing?)
    public func cost(of usage: LLMUsage) -> CostEstimate
    public func preflight(_ input: SummaryInput, endpoint: LLMEndpoint) -> CostEstimate }
```

## Prompt contracts

Pass 1 cleanup, one request per chunk, temperature 0, `maxConcurrentRequests`
in flight. System: role, meeting language, glossary (participants, calendar
attendees, known people, user product list), schema, rules: keep the exact
number and order of segments; never merge, split, drop, add, shorten or
summarise; keep code-switching as spoken, never translate; fix STT
misspellings of anglicisms and product names ("Git Hub" to "GitHub");
German noun casing; glossary spelling of names; punctuation; return text
unchanged when nothing needs fixing. User: `leadingContext` segments marked
read-only, then the chunk as `[n] <speaker label>: <text>`. Validation:
same count, indices `0..<n` each once, per-segment word ratio 0.7 to 1.3. A
failing chunk is retried once with the error appended, then kept raw and
listed in `failedChunks`. `rawText` always keeps the STT output.

Pass 2 analysis, temperature 0.2. System: role, output language by name,
meeting date (to resolve "next Friday"), participants with roles, speaker
labels with known names, calendar attendees, template context, one block
per template section (heading, instructions, omit when empty), schema,
rules: bullets are a short bold lead phrase plus one to three sentences;
name people by known name, else label, verbatim so Steno can bold them;
decisions are things agreed, not proposals; tasks are explicit commitments
with an owner; priority `high` only when urgency was said; `dueDate`
absolute or null; suggest a speaker name only with transcript evidence
(addressed by name, self-introduction) or by elimination against the
attendee list, with confidence 0 to 1 and a one-line quote; title under 80
characters, `Topic: Subtopic` when natural. User: the cleaned transcript
(single shot), one chunk (map), or the `ChunkNotes` array (reduce).
Post-processing in `LLMMeetingSummarizer`: drop sections not in the
template and empty non-required ones; validate `dueDate` with a strict
`yyyy-MM-dd` formatter, null on failure; match `assignee` to a participant
or attendee case-insensitively, else keep `assigneeName`; map
`speakerLabel` to `Speaker.id`; bold known names; render Markdown.

## Files

- `Package.swift`: add `StenoLLM` (depends on `StenoCore`), `StenoLLMTests`.
- `Sources/StenoLLM/LLMEndpoint.swift`, `RetryPolicy.swift`,
  `OpenAICompatibleClient.swift` (client, mode fallback, probe, key
  redaction), `Wire/ChatCompletion.swift` (Codable request, response, error).
- `Sources/StenoLLM/StructuredOutput/JSONSchema.swift` (builder, strict
  validator, prompt rendering), `StructuredOutputDecoder.swift` (extract,
  repair fences, prefixes, trailing commas, truncated tail; decode).
- `Sources/StenoLLM/Budget/TokenBudget.swift`, `TranscriptChunker.swift`.
- `Sources/StenoLLM/Cleanup/CleanupPromptBuilder.swift`, `CleanupDraft.swift`,
  `LLMTranscriptCleaner.swift`, `Glossary.swift`.
- `Sources/StenoLLM/Templates/TemplateParser.swift` (frontmatter keys `id`,
  `name`, `description`, `icon`, `context`, `sections`, `required`; body
  `## <section-id>` blocks with a `heading:` line then instructions),
  `TemplateCatalog.swift` (loads from `Bundle.module`, validates at load).
- `Sources/StenoLLM/Resources/Templates/` (sections in order):
  `default.md` Executive Summary (required), Full Summary by topic, Open
  Questions; `customer-discovery.md` Customer Context, Problems and Pain
  Points, Current Workflow and Tools, Reactions and Buying Signals,
  Objections and Risks, Next Steps; `daily-standup.md` Progress Since Last
  Standup, Plans Until Next Standup, Blockers and Help Needed, Announcements
  (bullet leads are person names); `interview.md` Candidate Background, Role
  Fit and Experience, Skills Assessment, Motivation and Culture, Candidate
  Questions, Assessment and Recommendation, Next Steps.
- `Sources/StenoLLM/Summary/SummaryPromptBuilder.swift`, `AnalysisDraft.swift`,
  `SummaryRenderer.swift`, `LLMMeetingSummarizer.swift` (single shot or map
  and reduce, post-processing to `SummaryOutput`).
- `Sources/StenoLLM/Language/OutputLanguage.swift`, `Cost/CostEstimator.swift`.
- `Sources/steno/Commands/LLMCommands.swift`: `steno llm probe|cleanup
  <transcript.json>|summarize <transcript.json> --template <id> [--language
  de]`; prints usage and cost.
- `Tests/StenoLLMTests/Support/StubChatServer.swift`: `NWListener` HTTP/1.1
  server on 127.0.0.1, ephemeral port, scripted responses, records requests;
  `Scripts.swift`: ok, 429 with `Retry-After`, 500 then ok, 400 on
  `response_format`, fenced JSON, invalid JSON, `finish_reason: length`, slow.
- `Tests/StenoLLMTests/{Client,Retry,StructuredDecoder,JSONSchemaStrict,
  TokenBudget,TranscriptChunker,Cleanup,Summary,SummaryRenderer,
  TemplateParser,PromptSnapshot,CostEstimator,LiveEndpoint}Tests.swift`
- `Tests/Fixtures/LLM/transcripts/denglish-standup.json` (24 synthetic
  segments, three speakers, deliberate STT errors),
  `customer-call-60min.json` (generated, about 900 segments, forces map and
  reduce at an 8k budget); `Tests/Fixtures/LLM/prompts/*.txt` (golden
  prompts); `Tests/Fixtures/LLM/responses/*.json` (canned server bodies).

## Steps

Each step is at most one day and ends with a check a reviewer can run.

1. Target, wire types, stub server. Check: `StubChatServerTests` accept a
   POST, return a scripted body and record the parsed request.
2. Client, retry, timeout, errors. Bearer auth, per-attempt timeout, retry on
   408, 429, 5xx, transport errors and timeouts with `Retry-After`, no retry
   on other 4xx, cancellation, key never in logs. Check: `ClientTests` and
   `RetryTests` cover "429 then 200 honours Retry-After", "three 500s throw
   `http(500)`", "401 not retried", "cancel stops within 100 ms".
3. Structured output. `JSONSchema.validateStrict`, decoder repairs, mode
   fallback on 400, `truncated` on `finish_reason: length`, `refused`.
   Check: strict tests reject an optional property, a missing
   `additionalProperties: false` and depth 6; decoder tests accept fenced,
   prefixed and trailing-comma JSON, reject truncated JSON; a scripted 400
   flips `resolvedMode`.
4. Budget and chunker. Check: a 1,000-word German sample estimates between
   330 and 500 tokens; the 60-minute fixture yields 6 to 12 chunks that
   concatenate back to the input in order; no chunk exceeds `maxTokens`
   unless a single segment does.
5. Templates. Parser, catalog, four resources, load-time validation. Check:
   a fixture round-trips, a listed section without a body is rejected,
   `TemplateCatalog.builtIn.all.count == 4`.
6. Cleanup pass. Check: `CleanupTests` preserve count and order, keep
   `rawText`, mark a wrong-count chunk failed after one retry, never exceed
   `maxConcurrentRequests` (server records overlap); `PromptSnapshotTests`
   match `prompts/cleanup-*.txt`.
7. Summary pass, single shot. Check: Markdown starts with `## Executive
   Summary`, bullets match `^- \*\*[^*]+\*\*: ` and contain `**Nicolai**`;
   an unknown section id is dropped; `dueDate` "next Friday" becomes nil;
   snapshots for all four templates match.
8. Map and reduce. Check: at `contextTokens: 8_000` the 60-minute fixture
   issues N map calls then one reduce call (server records `purpose` order);
   at 32k one call; at 4k `transcriptTooLong` with the transcript untouched.
9. Names, tasks, language, cost. Check: `Speaker 2` maps to the right UUID,
   a suggestion under 0.3 confidence is dropped; meeting `de` with override
   `en` puts `English` in the prompt; 10k in, 2k out at 1.00 / 4.00 per
   million prices as 0.018.
10. CLI, live test, probe matrix. `steno llm probe|cleanup|summarize`,
    `LiveEndpointTests`, run spike 1 and replace "unverified" in the table
    above. Check: `steno llm probe` against a local server prints
    reachability, mode and round trip; `STENO_LLM_TESTS=1 swift test
    --filter LiveEndpointTests` passes on one real endpoint; CI URL and test
    count are in the PR.

## Tests

Unit, no network, stub server on loopback only: client (auth, body per
mode, retry matrix, timeout, cancel, usage, redaction); structured output
(strict validator, each repair, truncation, refusal, mode fallback); budget
and chunker (bounds per language, packing, order, oversized segment);
templates (round trip, malformed frontmatter, built-ins load); cleanup
(count and order, word ratio, raw fallback, concurrency, glossary present);
summary (rendering, section filtering, date and assignee validation,
speaker mapping, path selection, usage sum); cost (maths, nil pricing,
preflight overestimates the scripted actual). Golden prompt snapshots for
every builder, four templates, two languages; `STENO_UPDATE_SNAPSHOTS=1`
regenerates; a snapshot diff in a PR is a reviewed prompt change.

Integration, opt-in: `LiveEndpointTests` runs when `STENO_LLM_TESTS=1` with
`STENO_LLM_BASE_URL`, `STENO_LLM_MODEL` and optional `STENO_LLM_API_KEY`:
Denglish fixture through cleanup and the Default template; asserts segment
count preserved, output decodes, a bullet, a title, `usage`; else skipped.

Manual, a human on a Mac: `steno llm summarize` on a real personal Denglish
transcript against LM Studio or Ollama on the same Mac and one hosted
endpoint; open the Markdown in Obsidian; confirm bullets read like Jamie's,
names are bold, no cleaned sentence was shortened, and reported cost matches
the provider dashboard within 10 percent.

## Spikes

1. Structured output capability matrix (informs step 3, recorded in step
   10). Run the real `draftSchema` against Ollama, LM Studio, OpenRouter,
   Groq and the Anthropic compatibility endpoint; record resolved mode,
   `strict` acceptance, validation rate. Success: every server validates
   without the repair call in 9 of 10 runs.
2. Cleanup fidelity (before step 6 is done). Denglish fixture plus one real
   transcript, one local 7 to 9B model and one hosted model. Success: count
   preserved in every chunk, word ratio 0.9 to 1.1 in 95 percent of
   segments. This is the cleanup half of scope spike 2.
3. Map and reduce quality (before step 8). Same meeting at 32k single shot and
   8k map-reduce; success: no single-shot decision or task missing after reduce.

## Needs from other workstreams

- Core foundation (`StenoCore`), deltas on the types in its plan:
  `LLMRequest.responseFormat: LLMResponseFormat` (`.text`, `.jsonObject`,
  `.jsonSchema(name:schema:strict:)`) replacing bare `jsonSchema`, plus
  `purpose: String`; `LLMResponse.finishReason: LLMFinishReason` (`stop`,
  `length`, `contentFilter`, `other`); `TranscriptCleaner.clean` takes a
  `CleanupContext` (language, participants, speakers, calendar attendees,
  known people, product glossary) and returns `CleanupResult` (segments,
  `failedChunks`, usage); `SummaryInput` gains `knownPeople`,
  `productGlossary`, `outputLanguage: Locale.Language?`;
  `SummaryOutput.speakerNames` becomes `[SpeakerNameSuggestion]` (speakerID,
  name?, confidence, evidence) and `SummaryOutput.usage: LLMUsage` (move
  `LLMUsage` to core); `Meeting.summaryLanguage`; `Settings` gains
  `llmContextTokens`, `llmPricing`, `llmProductGlossary`; `TemplateSection`
  gains `id`, `required`; `SummaryTemplate` gains `description`, `context`.
- Speech and speakers (`SpeechEngine`, `Diarizer`, `SpeakerMemory`):
  `speakerID` on every segment, stable `Speaker.clusterLabel` ("Speaker 1"),
  `Meeting.language` detected.
- Adapters (`Destination`): render `summaryMarkdown` as is; use
  `MeetingTask.assigneeName` when `assigneePersonID` is nil.
- macOS app (`SecretStore`): API key from Keychain into `LLMEndpoint.apiKey`;
  settings UI for base URL, model, context tokens, pricing, default summary
  language; review sheet prefills from `SpeakerNameSuggestion`; re-run
  summary after speaker renaming.

## Requested changes to the program document

1. Decide template ownership; both plans claim it. Core foundation proposes
   JSON in `StenoCore/Resources/Templates`; this plan proposes Markdown with
   frontmatter in `StenoLLM/Resources/Templates` per its brief. Either works
   with the fields in decision 5. If StenoCore wins, this plan drops
   `TemplateParser`, `TemplateCatalog` and the four `.md` files and reads
   `TemplateRegistry.bundled`.
2. Supporting types: `LLMRequest` and `LLMResponse` carry a response format
   enum with strictness, a `purpose`, and a `finishReason`; a client cannot
   detect truncation or refusal from `text` alone.
3. Pipeline boundaries: `TranscriptCleaner` and `MeetingSummarizer` as StenoCore
   protocols implemented by StenoLLM (core asks the same), with inputs above.
4. Canonical model: add `Meeting.summaryLanguage: Locale.Language?` and a
   home for per-meeting `LLMUsage` (`Meeting.llmUsage` or a `ProcessingRun`
   row) so the language override and cost have storage.
5. Verification standard: list `STENO_LLM_TESTS=1` next to
   `STENO_MODEL_TESTS=1` as the opt-in switch for the live endpoint test.
