import Testing
import speex

@testable import StenoAudio

@Suite struct PackageWiringTests {
  @Test func moduleConstantsMatchThePlan() {
    #expect(StenoAudio.sampleRate == 48_000)
    #expect(StenoAudio.frameSize == 480)
    #expect(StenoAudio.echoTailLength == 9_600)
  }

  /// The CSpeex product links and its DSP entry points resolve.
  @Test func speexEchoStateInitialisesAndDestroys() {
    let state = speex_echo_state_init(Int32(StenoAudio.frameSize), Int32(StenoAudio.echoTailLength))
    #expect(state != nil)
    var frameSize: Int32 = 0
    #expect(speex_echo_ctl(state, SPEEX_ECHO_GET_FRAME_SIZE, &frameSize) == 0)
    #expect(frameSize == 480)
    speex_echo_state_destroy(state)
  }
}
