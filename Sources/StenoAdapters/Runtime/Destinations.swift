import Foundation
import StenoCore

/// Every destination the settings configure, in delivery order. Today the
/// Obsidian folder when `settings.obsidian` is set; a second destination adds
/// one line here and one typed optional to `Settings`.
public func destinations(for settings: Settings) -> [any Destination] {
  guard let obsidian = settings.obsidian else { return [] }
  return [ObsidianFolderDestination(settings: obsidian)]
}
