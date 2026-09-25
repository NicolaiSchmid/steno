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

    OnDestroy {
      self.browser.stop()
    }

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
          promise.reject(StenoLinkException.resolve(error.localizedDescription))
        }
      }
    }
  }
}

enum StenoLinkException {
  static func resolve(_ message: String) -> Exception {
    Exception(name: "ERR_STENO_RESOLVE", description: message)
  }
}
