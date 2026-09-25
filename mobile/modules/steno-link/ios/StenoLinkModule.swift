import ExpoModulesCore

/// `StenoLink`: the phone's side of the handover transport. Bonjour browsing
/// (`Browser`), pinned foreground requests (`PinnedClient`), background chunk
/// uploads (`UploadSession`) and streaming file hashes (`FileHashing`).
///
/// Compiled only by `expo prebuild` on a Mac; `mobile-ci.yml` checks the
/// TypeScript side. Keep the JS-facing shapes in `src/StenoLink.types.ts` in
/// step with the records and event bodies here.
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
          promise.resolve(["host": resolved.host, "port": resolved.port])
        case .failure(let error):
          promise.reject(StenoLinkException("ERR_STENO_RESOLVE", error.localizedDescription))
        }
      }
    }

    // MARK: Pinned foreground request

    AsyncFunction("request") { (request: PinnedRequestRecord, promise: Promise) in
      let spec = PinnedClient.Request(
        url: request.url,
        method: request.method,
        headers: request.headers,
        body: request.body,
        fingerprint: request.fingerprint,
        timeout: max(request.timeoutMs, 1) / 1000
      )
      PinnedClient.perform(spec) { result in
        switch result {
        case .success(let response):
          promise.resolve([
            "status": response.status,
            "headers": response.headers,
            "body": response.body,
          ])
        case .failure(let error):
          promise.reject(StenoLinkException("ERR_STENO_REQUEST", error.localizedDescription))
        }
      }
    }

    // MARK: Background upload

    AsyncFunction("startUpload") { (spec: UploadSpecRecord) in
      guard spec.offset >= 0, spec.length > 0 else {
        throw StenoLinkException("ERR_STENO_UPLOAD", "offset must be >= 0 and length > 0")
      }
      try UploadSession.shared.start(
        UploadSession.Spec(
          taskID: spec.taskID,
          url: spec.url,
          headers: spec.headers,
          fingerprint: spec.fingerprint,
          filePath: StenoLinkModule.path(from: spec.filePath),
          offset: UInt64(spec.offset),
          length: UInt64(spec.length)
        ))
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

struct PinnedRequestRecord: Record {
  @Field var url: String = ""
  @Field var method: String = "GET"
  @Field var headers: [String: String] = [:]
  @Field var body: String?
  @Field var fingerprint: String = ""
  @Field var timeoutMs: Double = 10_000
}

struct UploadSpecRecord: Record {
  @Field var taskID: String = ""
  @Field var url: String = ""
  @Field var headers: [String: String] = [:]
  @Field var fingerprint: String = ""
  @Field var filePath: String = ""
  @Field var offset: Double = 0
  @Field var length: Double = 0
}

/// JS receives `code` as `error.code` and `message` as the description.
func StenoLinkException(_ code: String, _ message: String) -> Exception {
  Exception(name: code, description: message, code: code)
}
