import Foundation
import NIOCore
import NIOHTTP1

/// The one channel handler behind `configureHTTPServerPipeline()`. Per
/// request it matches the route, runs the auth gate on the head (reads are
/// paused while the gate is pending, so an unauthenticated body is never
/// buffered), enforces the body limit (413 and close), collects the body and
/// hands the complete request to the engine. Rejections are answered at
/// once with `Connection: close`; what the client still sends is discarded
/// up to the limit, then the connection closes.
final class HTTPHandler: ChannelInboundHandler, RemovableChannelHandler {
  typealias InboundIn = HTTPServerRequestPart
  typealias OutboundOut = HTTPServerResponsePart

  private struct Pending {
    var head: HTTPRequestHead
    var route: Route
    var limit: Int
    var body: ByteBuffer
    /// Nil while the auth gate is still deciding.
    var principal: Principal?
    var endReceived = false
  }

  private enum State {
    case idle
    case pending(Pending)
    /// Rejected or over limit: eat what is left, then close.
    case discarding(remaining: Int)
    case closed

    var isTerminal: Bool {
      switch self {
      case .discarding, .closed: true
      default: false
      }
    }

    var isClosed: Bool {
      if case .closed = self { return true }
      return false
    }
  }

  private let engine: any RequestHandling
  private let configuration: HandoverConfiguration
  private let metrics: ServerMetrics
  private var state: State = .idle
  /// The flush of the last response end; a close waits for it, because
  /// Network.framework's cancel drops queued sends.
  private var lastWrite: EventLoopFuture<Void>?
  private var closeTimer: Scheduled<Void>?
  /// How long a half-closed connection may linger before it is torn down.
  static let closeGrace = TimeAmount.seconds(2)

  init(engine: any RequestHandling, configuration: HandoverConfiguration, metrics: ServerMetrics) {
    self.engine = engine
    self.configuration = configuration
    self.metrics = metrics
  }

  func channelRead(context: ChannelHandlerContext, data: NIOAny) {
    switch unwrapInboundIn(data) {
    case .head(let head):
      receive(head: head, context: context)
    case .body(var buffer):
      receive(body: &buffer, context: context)
    case .end:
      receiveEnd(context: context)
    }
  }

  func channelInactive(context: ChannelHandlerContext) {
    closeTimer?.cancel()
    closeTimer = nil
    state = .closed
    context.fireChannelInactive()
  }

  func errorCaught(context: ChannelHandlerContext, error: any Error) {
    // A parser error or a broken connection: nothing to answer, drop it.
    metrics.update { $0.closedByServer += 1 }
    state = .closed
    context.close(promise: nil)
  }

  // MARK: - Request parts

  private func receive(head: HTTPRequestHead, context: ChannelHandlerContext) {
    metrics.update { $0.requestHeads += 1 }
    guard let route = Route.match(method: head.method, uri: head.uri) else {
      reject(.problem(.notFound, "no such route"), head: head, context: context)
      return
    }
    let limit = route.bodyLimit(configuration)
    if let declared = head.headers.first(name: "content-length").flatMap(Int.init), declared > limit
    {
      respondAndClose(
        .problem(.payloadTooLarge, "body limit is \(limit) bytes"), head: head, context: context)
      return
    }
    state = .pending(
      Pending(
        head: head, route: route, limit: limit, body: context.channel.allocator.buffer(capacity: 0))
    )
    // Stop reading until the gate has decided; bytes already decoded from
    // the first read are buffered below and dropped on rejection.
    context.channel.setOption(ChannelOptions.autoRead, value: false).whenFailure { _ in }
    let authorization = head.headers.first(name: "authorization")
    let bound = NIOLoopBound((self, context), eventLoop: context.eventLoop)
    let engine = self.engine
    context.eventLoop.makeFutureWithTask {
      await engine.authenticate(route, authorization: authorization)
    }.whenComplete { result in
      let (handler, context) = bound.value
      handler.authenticationFinished(result, context: context)
    }
  }

  private func authenticationFinished(
    _ result: Result<AuthOutcome, any Error>, context: ChannelHandlerContext
  ) {
    guard case .pending(var pending) = state, pending.principal == nil else { return }
    context.channel.setOption(ChannelOptions.autoRead, value: true).whenFailure { _ in }
    switch result {
    case .success(.allowed(let principal)):
      pending.principal = principal
      state = .pending(pending)
      if pending.endReceived { dispatch(pending, principal: principal, context: context) }
    case .success(.unauthorized):
      reject(
        .problem(.unauthorized, "unknown or revoked token"), head: pending.head, context: context)
    case .success(.forbidden):
      reject(.problem(.forbidden, "pairing secret rejected"), head: pending.head, context: context)
    case .failure(let error):
      reject(
        .problem(.internalServerError, "authentication failed: \(error)"), head: pending.head,
        context: context)
    }
  }

  private func receive(body buffer: inout ByteBuffer, context: ChannelHandlerContext) {
    switch state {
    case .pending(var pending):
      pending.body.writeBuffer(&buffer)
      state = .pending(pending)
      if pending.body.readableBytes > pending.limit {
        respondAndClose(
          .problem(.payloadTooLarge, "body limit is \(pending.limit) bytes"), head: pending.head,
          context: context)
      }
    case .discarding(let remaining):
      let count = buffer.readableBytes
      metrics.update { $0.discardedBodyBytes += count }
      if count > remaining {
        close(context: context)
      } else {
        state = .discarding(remaining: remaining - count)
      }
    case .idle, .closed:
      break
    }
  }

  private func receiveEnd(context: ChannelHandlerContext) {
    switch state {
    case .pending(var pending):
      if let principal = pending.principal {
        dispatch(pending, principal: principal, context: context)
      } else {
        pending.endReceived = true
        state = .pending(pending)
      }
    case .discarding:
      close(context: context)
    case .idle, .closed:
      break
    }
  }

  // MARK: - Dispatch and responses

  private func dispatch(_ pending: Pending, principal: Principal, context: ChannelHandlerContext) {
    state = .idle
    metrics.update { $0.handledRequests += 1 }
    let request = HandoverRequest(
      route: pending.route, principal: principal, headers: pending.head.headers,
      body: Data(pending.body.readableBytesView))
    let head = pending.head
    let bound = NIOLoopBound((self, context), eventLoop: context.eventLoop)
    let engine = self.engine
    context.eventLoop.makeFutureWithTask {
      await engine.handle(request)
    }.whenComplete { result in
      let (handler, context) = bound.value
      switch result {
      case .success(let response):
        handler.write(response, head: head, close: !head.isKeepAlive, context: context)
      case .failure(let error):
        handler.write(
          .problem(.internalServerError, "\(error)"), head: head, close: true, context: context)
      }
    }
  }

  /// Answers now, then discards whatever body still arrives (up to the
  /// limit) so the client reads the status instead of a reset.
  private func reject(
    _ response: HandoverResponse, head: HTTPRequestHead, context: ChannelHandlerContext
  ) {
    if case .pending(let pending) = state {
      let seen = pending.body.readableBytes
      metrics.update { $0.discardedBodyBytes += seen }
    }
    state = .discarding(remaining: HandoverConfiguration.jsonBodyLimit)
    write(response, head: head, close: false, context: context)
  }

  private func respondAndClose(
    _ response: HandoverResponse, head: HTTPRequestHead, context: ChannelHandlerContext
  ) {
    // Anything that still arrives while the response flushes is dropped;
    // `write` closes once the end has gone out.
    state = .discarding(remaining: 0)
    write(response, head: head, close: true, context: context)
  }

  private func write(
    _ response: HandoverResponse, head: HTTPRequestHead, close: Bool, context: ChannelHandlerContext
  ) {
    var headers = response.headers
    headers.replaceOrAdd(name: "Content-Length", value: String(response.body.count))
    if close || state.isTerminal { headers.replaceOrAdd(name: "Connection", value: "close") }
    metrics.update { $0.statuses.append(response.status.code) }
    let responseHead = HTTPResponseHead(
      version: head.version, status: response.status, headers: headers)
    context.write(wrapOutboundOut(.head(responseHead)), promise: nil)
    if !response.body.isEmpty {
      var buffer = context.channel.allocator.buffer(capacity: response.body.count)
      buffer.writeBytes(response.body)
      context.write(wrapOutboundOut(.body(.byteBuffer(buffer))), promise: nil)
    }
    let promise = context.eventLoop.makePromise(of: Void.self)
    context.writeAndFlush(wrapOutboundOut(.end(nil)), promise: promise)
    lastWrite = promise.futureResult
    if close {
      self.close(context: context)
    }
  }

  /// Orderly close: once the last response has left, half-close the output
  /// (FIN after the data, so the client reads the status instead of a
  /// reset), then tear the connection down after `closeGrace` unless the
  /// client closed first.
  private func close(context: ChannelHandlerContext) {
    guard !state.isClosed else { return }
    state = .closed
    metrics.update { $0.closedByServer += 1 }
    let bound = NIOLoopBound((self, context), eventLoop: context.eventLoop)
    (lastWrite ?? context.eventLoop.makeSucceededVoidFuture()).whenComplete { _ in
      let (handler, context) = bound.value
      context.close(mode: .output, promise: nil)
      handler.closeTimer = context.eventLoop.scheduleTask(in: Self.closeGrace) {
        bound.value.1.close(mode: .all, promise: nil)
      }
    }
  }
}
