import Foundation
import NIOCore
import NIOHTTP1

@testable import StenoHandover

#if canImport(Network)
  import Network
  import NIOTransportServices
  import Security
#else
  import NIOPosix
#endif

/// A client for the limit tests, where the assertion is about the connection
/// itself: it sends exactly the head and body it is given (a body shorter
/// than its `Content-Length`, a chunked body on a GET), reads one response
/// through NIO's client codec and reports whether the server closed. Over
/// TLS it pins with the same `PinnedTrustEvaluator` as `LoopbackClient`.
struct RawClient {
  struct Exchange: Sendable {
    var status: Int?
    var headers: HTTPHeaders
    var body: Data
    var closedByServer: Bool
  }

  let port: UInt16
  let fingerprint: Data

  /// Returns once a full response has been read and the server either
  /// closed or `closeGrace` passed. A response that never arrives within
  /// `timeout` returns whatever was read.
  func exchange(
    _ method: HTTPMethod, _ path: String, headers: [(String, String)] = [], body: Data = Data(),
    closeGrace: Duration = .seconds(3), timeout: Duration = .seconds(10)
  ) async throws -> Exchange {
    let collector = Collector()
    #if canImport(Network)
      let group = NIOTSEventLoopGroup(loopCount: 1)
      let tls = NWProtocolTLS.Options()
      let pinned = fingerprint
      sec_protocol_options_set_verify_block(
        tls.securityProtocolOptions,
        { _, secTrust, complete in
          let trust = sec_trust_copy_ref(secTrust).takeRetainedValue()
          complete(PinnedTrustEvaluator.evaluate(trust, pinnedFingerprint: pinned))
        }, DispatchQueue(label: "steno.rawclient.verify"))
      let bootstrap = NIOTSConnectionBootstrap(group: group).tlsOptions(tls)
    #else
      let group = MultiThreadedEventLoopGroup(numberOfThreads: 1)
      let bootstrap = ClientBootstrap(group: group)
    #endif
    defer { Task { try? await group.shutdownGracefully() } }
    let channel = try await bootstrap.channelInitializer { channel in
      channel.eventLoop.makeCompletedFuture {
        try channel.pipeline.syncOperations.addHTTPClientHandlers()
        try channel.pipeline.syncOperations.addHandler(CollectingHandler(collector: collector))
      }
    }.connectTimeout(.seconds(8)).connect(host: "127.0.0.1", port: Int(port)).get()

    let head = HTTPRequestHead(
      version: .http1_1, method: method, uri: path,
      headers: HTTPHeaders([("Host", "127.0.0.1")] + headers))
    channel.write(HTTPClientRequestPart.head(head), promise: nil)
    channel.write(HTTPClientRequestPart.body(.byteBuffer(ByteBuffer(bytes: body))), promise: nil)
    try await channel.writeAndFlush(HTTPClientRequestPart.end(nil))

    let clock = ContinuousClock()
    let deadline = clock.now + timeout
    while clock.now < deadline, !collector.closed, collector.response == nil {
      try await Task.sleep(for: .milliseconds(10))
    }
    let graceDeadline = clock.now + closeGrace
    while clock.now < graceDeadline, !collector.closed {
      try await Task.sleep(for: .milliseconds(10))
    }
    let closed = collector.closed
    try? await channel.close()
    let (responseHead, responseBody) = collector.response ?? (nil, Data())
    return Exchange(
      status: responseHead.map { Int($0.status.code) },
      headers: responseHead?.headers ?? HTTPHeaders(), body: responseBody, closedByServer: closed)
  }
}

/// What the connection has delivered so far, shared between the NIO handler
/// (event loop) and the test (cooperative pool).
final class Collector: @unchecked Sendable {
  private let lock = NSLock()
  private var head: HTTPResponseHead?
  private var body = Data()
  private var ended = false
  private var isClosed = false

  var closed: Bool { lock.withLock { isClosed } }

  /// The one response once its end has arrived, else nil.
  var response: (HTTPResponseHead?, Data)? {
    lock.withLock { ended ? (head, body) : nil }
  }

  func receive(_ part: HTTPClientResponsePart) {
    lock.withLock {
      switch part {
      case .head(let received): head = received
      case .body(let buffer): body.append(contentsOf: buffer.readableBytesView)
      case .end: ended = true
      }
    }
  }

  func markClosed() {
    lock.withLock { isClosed = true }
  }
}

final class CollectingHandler: ChannelInboundHandler {
  typealias InboundIn = HTTPClientResponsePart

  private let collector: Collector

  init(collector: Collector) {
    self.collector = collector
  }

  func channelRead(context: ChannelHandlerContext, data: NIOAny) {
    collector.receive(unwrapInboundIn(data))
  }

  func channelInactive(context: ChannelHandlerContext) {
    collector.markClosed()
    context.fireChannelInactive()
  }

  func errorCaught(context: ChannelHandlerContext, error: any Error) {
    collector.markClosed()
    context.close(promise: nil)
  }
}
