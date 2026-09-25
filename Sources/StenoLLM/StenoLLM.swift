/// `StenoLLM`: the OpenAI-compatible client, structured output, token
/// budgeting and chunking, the cleanup pass (`LLMTranscriptCleaner`) and the
/// summary pass (`LLMMeetingSummarizer`). Text only ever crosses the network
/// from here: no audio, no file paths, no scratchpad.
public enum StenoLLM {
  /// The header every completion carries so a proxy or the stub server can
  /// tell the passes apart.
  public static let purposeHeader = "X-Steno-Purpose"
}
