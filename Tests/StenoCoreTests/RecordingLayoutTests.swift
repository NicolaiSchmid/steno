import Foundation
import Testing

@testable import StenoCore

@Suite struct RecordingLayoutTests {
  @Test func spellsEveryFileOfTheMeetingFolderOnce() {
    let folder = URL(fileURLWithPath: "/tmp/steno/audio", isDirectory: true)
    let layout = RecordingLayout(audioFolder: folder, meetingID: SampleData.meetingID)
    let base = "/tmp/steno/audio/\(SampleData.meetingID.uuidString)"
    #expect(layout.directory.path == base)
    #expect(layout.master(.caf48kFloat32).path == "\(base)/recording.caf")
    #expect(layout.master(.m4aAAC).path == "\(base)/recording.m4a")
    #expect(layout.master(.wav16kInt16).path == "\(base)/recording.wav")
    #expect(layout.sidecar(.mic).path == "\(base)/mic.wav")
    #expect(layout.sidecar(.system).path == "\(base)/system.wav")
    #expect(layout.mixdown(.m4aAAC).path == "\(base)/audio.m4a")
    #expect(layout.speakersDirectory.path == "\(base)/speakers")
    #expect(
      layout.sampleClip(speakerID: SampleData.speakerTwoID).path
        == "\(base)/speakers/\(SampleData.speakerTwoID.uuidString).wav")
  }

  @Test func isDerivedFromTheAssetsMasterSoThePipelineNeverReadsSettings() {
    let asset = SampleData.audioAsset()
    let layout = RecordingLayout(asset: asset)
    #expect(layout == RecordingLayout(directory: asset.url.deletingLastPathComponent()))
    #expect(layout.sidecar(.mic) == asset.sidecars16k[.mic])
    #expect(layout.mixdown(.m4aAAC) == asset.mixdownURL)
    #expect(
      layout.sampleClip(speakerID: SampleData.speakerTwoID)
        == SampleData.speakers()[1].sampleClipURL)
  }
}
