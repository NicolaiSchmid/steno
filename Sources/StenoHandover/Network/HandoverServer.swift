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
  private let group: any EventLoopGroup
  private let channel: any Channel

  static func start(
    configuration: HandoverConfiguration,
    identity: HandoverIdentity,
    engine: any RequestHandling,
    metrics: ServerMetrics
  ) async throws -> HandoverServer {
    let readTimeout = TimeAmount(configuration.readTimeout)
    let childInitializer: @Sendable (any Channel) -> EventLoopFuture<Void> = { channel in
      channel.eventLoop.makeCompletedFuture {
        // The idle handler sits ahead of the HTTP codec so a client that
        // never finishes its request line is timed out too.
        try channel.pipeline.syncOperations.addHandler(IdleStateHandler(readTimeout: readTimeout))
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
      if configuration.advertise {
        // The service is advertised on the LAN and nowhere else: not over a
        // VPN tunnel or another virtual interface, not over cellular.
        parameters.prohibitedInterfaceTypes = [.cellular, .other]
      } else {
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
      let group: any EventLoopGroup = NIOTSEventLoopGroup(loopCount: 1)
      let bind = NIOTSListenerBootstrap(group: group)
        .childChannelInitializer(childInitializer)
        .withNWListener(listener)
    #else
      let group: any EventLoopGroup = MultiThreadedEventLoopGroup(numberOfThreads: 1)
      let bind = ServerBootstrap(group: group)
        .serverChannelOption(.socketOption(.so_reuseaddr), value: 1)
        .childChannelInitializer(childInitializer)
        .bind(host: "127.0.0.1", port: Int(configuration.port))
    #endif
    do {
      let channel = try await bind.get()
      // An advertising NWListener has no required local endpoint, so NIOTS
      // reports no local address; the listener itself knows the port.
      #if canImport(Network)
        let bound = listener.port?.rawValue
      #else
        let bound = channel.localAddress?.port.map(UInt16.init)
      #endif
      guard let bound, bound != 0 else {
        try await channel.close()
        throw ServerError.noPort
      }
      return HandoverServer(port: bound, group: group, channel: channel)
    } catch {
      try? await group.shutdownGracefully()
      throw error
    }
  }

  private init(port: UInt16, group: any EventLoopGroup, channel: any Channel) {
    self.port = port
    self.group = group
    self.channel = channel
  }

  func stop() async {
    try? await channel.close()
    try? await group.shutdownGracefully()
  }
}

enum ServerError: Error, CustomStringConvertible, Sendable {
  case identityRejected
  case noPort

  var description: String {
    switch self {
    case .identityRejected: "Network.framework rejected the TLS identity"
    case .noPort: "the listener reported no port"
    }
  }
}
