import Foundation
import NIOCore
import NIOHTTP1

#if canImport(Network)
  import Network
  import NIOTransportServices
#else
  import NIOPosix
#endif

/// The listener. On Apple platforms it is `NIOTSListenerBootstrap` around an
/// `NWListener` whose parameters terminate TLS 1.3 with the Mac's identity
/// and, when advertising, publish `_steno._tcp` with the TXT record
/// (`v=1`, `id=<macID>`). Loopback only when `advertise` is false.
///
/// On Linux, where neither Network.framework nor Security exists, the same
/// HTTP pipeline listens in plaintext on 127.0.0.1 so the protocol core can
/// be built and tested in a container. That branch never compiles into the
/// Mac product.
final class HandoverServer: @unchecked Sendable {
  let port: UInt16
  /// The scheme a loopback client uses against this listener.
  let scheme: String
  private let group: any EventLoopGroup
  private let channel: any Channel
  #if canImport(Network)
    private let listener: NWListener
  #endif

  static func start(
    configuration: HandoverConfiguration,
    identity: HandoverIdentity,
    engine: any RequestHandling,
    metrics: ServerMetrics
  ) async throws -> HandoverServer {
    let childInitializer: @Sendable (any Channel) -> EventLoopFuture<Void> = { channel in
      channel.eventLoop.makeCompletedFuture {
        try channel.pipeline.syncOperations.configureHTTPServerPipeline(withErrorHandling: true)
        try channel.pipeline.syncOperations.addHandler(
          HTTPHandler(engine: engine, configuration: configuration, metrics: metrics))
      }
    }

    #if canImport(Network)
      let tls = NWProtocolTLS.Options()
      guard let secIdentity = sec_identity_create(identity.secIdentity) else {
        throw ServerError.identityRejected
      }
      sec_protocol_options_set_local_identity(tls.securityProtocolOptions, secIdentity)
      sec_protocol_options_set_min_tls_protocol_version(tls.securityProtocolOptions, .TLSv13)
      let parameters = NWParameters(tls: tls)
      parameters.allowLocalEndpointReuse = true
      let port = NWEndpoint.Port(rawValue: configuration.port) ?? .any
      if !configuration.advertise {
        parameters.requiredLocalEndpoint = .hostPort(host: .ipv4(.loopback), port: port)
      }
      let listener =
        configuration.advertise
        ? try NWListener(using: parameters, on: port) : try NWListener(using: parameters)
      if configuration.advertise {
        var txt = NWTXTRecord()
        txt["v"] = String(Wire.protocolVersion)
        txt["id"] = identity.macID.uuidString.lowercased()
        listener.service = NWListener.Service(
          name: configuration.serviceName, type: Wire.serviceType, domain: nil, txtRecord: txt)
      }
      let group = NIOTSEventLoopGroup(loopCount: 1)
      do {
        let channel = try await NIOTSListenerBootstrap(group: group)
          .childChannelInitializer(childInitializer)
          .withNWListener(listener).get()
        guard let bound = listener.port?.rawValue, bound != 0 else {
          try await channel.close()
          throw ServerError.noPort
        }
        return HandoverServer(
          port: bound, scheme: "https", group: group, channel: channel, listener: listener)
      } catch {
        try? await group.shutdownGracefully()
        throw error
      }
    #else
      let group = MultiThreadedEventLoopGroup(numberOfThreads: 1)
      do {
        let channel = try await ServerBootstrap(group: group)
          .serverChannelOption(.socketOption(.so_reuseaddr), value: 1)
          .childChannelInitializer(childInitializer)
          .bind(host: "127.0.0.1", port: Int(configuration.port)).get()
        guard let bound = channel.localAddress?.port, bound != 0 else {
          try await channel.close()
          throw ServerError.noPort
        }
        return HandoverServer(port: UInt16(bound), scheme: "http", group: group, channel: channel)
      } catch {
        try? await group.shutdownGracefully()
        throw error
      }
    #endif
  }

  #if canImport(Network)
    private init(
      port: UInt16, scheme: String, group: any EventLoopGroup, channel: any Channel,
      listener: NWListener
    ) {
      self.port = port
      self.scheme = scheme
      self.group = group
      self.channel = channel
      self.listener = listener
    }
  #else
    private init(port: UInt16, scheme: String, group: any EventLoopGroup, channel: any Channel) {
      self.port = port
      self.scheme = scheme
      self.group = group
      self.channel = channel
    }
  #endif

  func stop() async {
    try? await channel.close()
    try? await group.shutdownGracefully()
  }
}

enum ServerError: Error, CustomStringConvertible, Sendable {
  case identityRejected
  case noPort
  case notListening

  var description: String {
    switch self {
    case .identityRejected: "Network.framework rejected the TLS identity"
    case .noPort: "the listener reported no port"
    case .notListening: "the handover service is not listening"
    }
  }
}
