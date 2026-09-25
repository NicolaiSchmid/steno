import ExpoModulesCore

/// The JS-facing shapes, one struct per type in `src/StenoLink.types.ts`
/// under the same name. Inputs (`PinnedRequest`, `UploadSpec`) are decoded
/// from JS arguments; outputs cross the bridge through `toDictionary()`, so
/// a mistyped key is a compile error here instead of a silent `undefined`
/// on the phone. `native-contract.test.ts` compares the fields by name.
///
/// Each struct spells out `init()` because `Record` requires it and the
/// value initialiser below would otherwise replace the synthesized one.

struct MacService: Record {
  @Field var name: String = ""
  @Field var macID: String?

  init() {}
  init(name: String, macID: String?) {
    self.name = name
    self.macID = macID
  }
}

struct ResolvedMac: Record {
  @Field var host: String = ""
  @Field var port: Int = 0

  init() {}
  init(host: String, port: Int) {
    self.host = host
    self.port = port
  }
}

struct BrowserState: Record {
  @Field var state: String = "ready"
  @Field var policyDenied: Bool = false

  init() {}
  init(state: String, policyDenied: Bool) {
    self.state = state
    self.policyDenied = policyDenied
  }
}

struct PinnedRequest: Record {
  @Field var url: String = ""
  @Field var method: String = "GET"
  @Field var headers: [String: String] = [:]
  @Field var body: String?
  @Field var fingerprint: String = ""
  @Field var timeoutMs: Double = 10_000
}

struct PinnedResponse: Record {
  @Field var status: Int = 0
  @Field var headers: [String: String] = [:]
  @Field var body: String = ""

  init() {}
  init(status: Int, headers: [String: String], body: String) {
    self.status = status
    self.headers = headers
    self.body = body
  }
}

struct UploadSpec: Record {
  @Field var taskID: String = ""
  @Field var url: String = ""
  @Field var headers: [String: String] = [:]
  @Field var fingerprint: String = ""
  @Field var filePath: String = ""
  @Field var offset: Double = 0
  @Field var length: Double = 0
}

struct UploadProgress: Record {
  @Field var taskID: String = ""
  @Field var bytesSent: Int64 = 0
  @Field var totalBytes: Int64 = 0

  init() {}
  init(taskID: String, bytesSent: Int64, totalBytes: Int64) {
    self.taskID = taskID
    self.bytesSent = bytesSent
    self.totalBytes = totalBytes
  }
}

struct UploadFinished: Record {
  @Field var taskID: String = ""
  @Field var status: Int = 0
  @Field var body: String = ""

  init() {}
  init(taskID: String, status: Int, body: String) {
    self.taskID = taskID
    self.status = status
    self.body = body
  }
}

struct UploadFailed: Record {
  @Field var taskID: String = ""
  @Field var message: String = ""
  @Field var retryable: Bool = true

  init() {}
  init(taskID: String, message: String, retryable: Bool) {
    self.taskID = taskID
    self.message = message
    self.retryable = retryable
  }
}
