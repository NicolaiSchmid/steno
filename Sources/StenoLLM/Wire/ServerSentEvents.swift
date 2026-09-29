import Foundation

/// One server-sent event: the `event:` name when the server sent one and
/// the `data:` lines joined with newlines, as the specification reads them.
struct ServerSentEvent: Sendable, Equatable {
  var event: String?
  var data: String
}

/// Parses a complete `text/event-stream` body. Steno buffers the whole
/// answer (nothing shows partial output), so this is a pure function over
/// the bytes: events are separated by a blank line, lines end in `\n` or
/// `\r\n`, a line starting with `:` is a comment, `id:` and `retry:` are
/// ignored, and an event without data is dropped.
enum ServerSentEvents {
  static func parse(_ data: Data) -> [ServerSentEvent] {
    parse(String(decoding: data, as: UTF8.self))
  }

  static func parse(_ text: String) -> [ServerSentEvent] {
    var events: [ServerSentEvent] = []
    var name: String?
    var lines: [String] = []

    func flush() {
      if !lines.isEmpty {
        events.append(ServerSentEvent(event: name, data: lines.joined(separator: "\n")))
      }
      name = nil
      lines = []
    }

    // `\r\n` is one `Character` in Swift, so the line ends are normalised
    // before splitting on newlines.
    let normalised = text.replacingOccurrences(of: "\r\n", with: "\n")
      .replacingOccurrences(of: "\r", with: "\n")
    for line in normalised.split(omittingEmptySubsequences: false, whereSeparator: { $0 == "\n" }) {
      if line.isEmpty {
        flush()
        continue
      }
      if line.hasPrefix(":") { continue }
      let field: Substring
      var value: Substring
      if let colon = line.firstIndex(of: ":") {
        field = line[..<colon]
        value = line[line.index(after: colon)...]
        if value.hasPrefix(" ") { value = value.dropFirst() }
      } else {
        field = line
        value = ""
      }
      switch field {
      case "event": name = String(value)
      case "data": lines.append(String(value))
      default: break
      }
    }
    flush()
    return events
  }

  /// Whether `data` reads as an event stream: a `Content-Type` of
  /// `text/event-stream`, or a body whose first line is a field.
  static func looksLikeEventStream(contentType: String?, body: Data) -> Bool {
    if let contentType, contentType.lowercased().contains("text/event-stream") { return true }
    let head = String(decoding: body.prefix(64), as: UTF8.self)
    let firstLine =
      head.split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: false)
      .first.map(String.init) ?? ""
    return firstLine.hasPrefix("event:") || firstLine.hasPrefix("data:")
      || firstLine.hasPrefix(":")
  }
}
