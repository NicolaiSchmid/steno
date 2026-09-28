import Foundation
import StenoCore

/// The keyboard model of one open `SpeakerPicker`: the typed query, the
/// options built for it and which one Return would take. Pure, so the rules
/// are tested without a view. Nothing is highlighted until the user types,
/// presses Down, or the field opened pre-filled with a suggested name.
struct SpeakerPickerState: Equatable {
  enum Move {
    case up
    case down
  }

  private(set) var query = ""
  private(set) var options: [SpeakerOptions.Option] = []
  private(set) var highlighted: Int?
  /// True while the query is the pre-filled suggestion, untouched.
  private(set) var isPrefilled = false

  /// Opens the picker: with `prefill`, the field shows the suggested name
  /// and Return takes the first matching option; without, it is empty and
  /// Return does nothing.
  mutating func reset(prefill: String?) {
    query = prefill ?? ""
    isPrefilled = prefill != nil
    highlighted = nil
    options = []
  }

  /// New options for the current query. A pre-filled or typed query
  /// highlights the first option, an empty query nothing; an index past the
  /// end clamps to the last option.
  mutating func setOptions(_ new: [SpeakerOptions.Option]) {
    options = new
    guard !new.isEmpty else {
      highlighted = nil
      return
    }
    if let index = highlighted {
      highlighted = min(index, new.count - 1)
    } else if isPrefilled || !query.trimmingCharacters(in: .whitespaces).isEmpty {
      highlighted = 0
    }
  }

  /// The user typed: the pre-fill is gone and the first result will be
  /// highlighted (nothing when the field is empty again).
  mutating func queryChanged(_ text: String) {
    query = text
    isPrefilled = false
    highlighted = text.trimmingCharacters(in: .whitespaces).isEmpty ? nil : 0
  }

  mutating func move(_ direction: Move) {
    guard !options.isEmpty else {
      highlighted = nil
      return
    }
    switch direction {
    case .down:
      highlighted = min((highlighted ?? -1) + 1, options.count - 1)
    case .up:
      guard let current = highlighted else { return }
      highlighted = max(current - 1, 0)
    }
  }

  /// What Return commits: the highlighted option, or nothing.
  func commit() -> SpeakerOptions.Option? {
    guard let index = highlighted, options.indices.contains(index) else { return nil }
    return options[index]
  }
}
