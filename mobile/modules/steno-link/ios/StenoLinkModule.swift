import ExpoModulesCore

/// `StenoLink`: the phone's side of the handover transport. Bonjour browsing
/// (`Browser`), pinned foreground requests (`PinnedClient`), background chunk
/// uploads (`UploadSession`) and streaming file hashes (`FileHashing`).
///
/// Compiled only by `expo prebuild` on a Mac; `mobile-ci.yml` checks the
/// TypeScript side. Every JS-facing shape is a `Record` in `Records.swift`
/// named as its TypeScript type.
public final class StenoLinkModule: Module {
  private lazy var browser = Browser { [weak self] name, body in
    self?.sendEvent(name, body)
  }

  public func definition() -> ModuleDefinition {
    Name("StenoLink")

    Events(
      "serviceFound",
      "serviceLost",
      "browserState",
      "uploadProgress",
      "uploadFinished",
      "uploadFailed"
    )

    OnCreate {
      UploadSession.shared.eventSink = { [weak self] name, body in
        self?.sendEvent(name, body)
      }
      UploadSession.shared.reconnect()
    }

    OnDestroy {
      self.browser.stop()
      UploadSession.shared.eventSink = nil
    }

    // MARK: Discovery

    Function("startBrowsing") {
      self.browser.start()
    }

    Function("stopBrowsing") {
      self.browser.stop()
    }

    AsyncFunction("resolve") { (serviceName: String, promise: Promise) in
      self.browser.resolve(serviceName: serviceName) { result in
        switch result {
        case .success(let resolved):
          promise.resolve(resolved.toDictionary())
        case .failure(let error):
          promise.reject(StenoLinkException("ERR_STENO_RESOLVE", error.localizedDescription))
        }
      }
    }

    // MARK: Pinned foreground request

    AsyncFunction("request") { (request: PinnedRequest, promise: Promise) in
      PinnedClient.perform(request) { result in
        switch result {
        case .success(let response):
          promise.resolve(response.toDictionary())
        case .failure(let error):
          promise.reject(StenoLinkException("ERR_STENO_REQUEST", error.localizedDescription))
        }
      }
    }

    // MARK: Background upload

    AsyncFunction("startUpload") { (spec: UploadSpec) in
      guard spec.offset >= 0, spec.length > 0 else {
        throw StenoLinkException("ERR_STENO_UPLOAD", "offset must be >= 0 and length > 0")
      }
      try UploadSession.shared.start(spec)
    }

    AsyncFunction("cancelUpload") { (taskID: String, promise: Promise) in
      UploadSession.shared.cancel(taskID: taskID) {
        promise.resolve()
      }
    }

    AsyncFunction("pendingUploads") { (promise: Promise) in
      UploadSession.shared.pendingTaskIDs { ids in
        promise.resolve(ids)
      }
    }

    // MARK: Hashing

    AsyncFunction("sha256") { (filePath: String) -> String in
      let url = URL(fileURLWithPath: StenoLinkModule.path(from: filePath))
      return try FileHashing.sha256(fileAt: url).base64EncodedString()
    }
  }

  /// Accepts both `file://` URIs (expo-file-system) and plain paths.
  static func path(from filePath: String) -> String {
    if let url = URL(string: filePath), url.isFileURL {
      return url.path
    }
    return filePath
  }
}

/// JS receives `code` as `error.code` and `message` as the description.
func StenoLinkException(_ code: String, _ message: String) -> Exception {
  Exception(name: code, description: message, code: code)
}

/// Input errors shared by `PinnedClient` and `UploadSession`.
enum StenoLinkError: LocalizedError {
  case badURL(String)
  /// Only `https://` carries the pin; a bearer must never travel in clear.
  case notHTTPS(String)
  case badFingerprint
  case notHTTP
  /// The server's leaf certificate did not hash to the pinned fingerprint.
  case pinMismatch

  var errorDescription: String? {
    switch self {
    case .badURL(let url): return "Not a URL: \(url)"
    case .notHTTPS(let url): return "Not an https URL: \(url)"
    case .badFingerprint: return "Fingerprint must be 32 bytes of standard base64"
    case .notHTTP: return "Response was not HTTP"
    case .pinMismatch: return "The Mac's certificate does not match the pairing"
    }
  }
}
