import Foundation

/// The background `URLSession` that carries chunk uploads while the phone is
/// locked (plan decision 4). One upload task per chunk, body from a temp
/// file, identity pinned per task through `taskDescription` so the delegate
/// can answer trust challenges after a relaunch when JS has no state yet.
///
/// Events go to JS through `eventSink`. When JS is not running (the app was
/// relaunched in the background to finish the session) the events are lost by
/// design; the coordinator reconciles with `pendingUploads()` and the Mac's
/// `GET /v1/recordings/{id}` on the next foreground.
final class UploadSession: NSObject, URLSessionDataDelegate {
  static let identifier = "uno.schmid.steno.upload"
  static let chunkHashHeader = "X-Steno-Chunk-SHA256"
  static let shared = UploadSession()

  typealias EventSink = (_ name: String, _ body: [String: Any]) -> Void

  struct Spec {
    var taskID: String
    var url: String
    var headers: [String: String]
    /// Standard base64 of the 32-byte leaf fingerprint.
    var fingerprint: String
    var filePath: String
    var offset: UInt64
    var length: UInt64
  }

  /// Persisted on each task as JSON in `taskDescription`.
  private struct TaskInfo: Codable {
    var taskID: String
    var fingerprint: String
  }

  var eventSink: EventSink?
  /// Set by the app delegate subscriber; called once the session drained.
  var backgroundCompletionHandler: (() -> Void)?

  private let lock = NSLock()
  private var responseBodies: [Int: Data] = [:]
  /// Tasks whose server trust failed the pin; they complete as `NSURLErrorCancelled`.
  private var pinRejectedTasks: Set<Int> = []

  private lazy var session: URLSession = {
    let configuration = URLSessionConfiguration.background(withIdentifier: UploadSession.identifier)
    configuration.isDiscretionary = false
    configuration.sessionSendsLaunchEvents = true
    configuration.allowsCellularAccess = false
    return URLSession(configuration: configuration, delegate: self, delegateQueue: nil)
  }()

  private override init() {
    super.init()
  }

  /// Creates the session if needed so queued delegate events are delivered.
  func reconnect() {
    _ = session
  }

  func start(_ spec: Spec) throws {
    guard let url = URL(string: spec.url) else {
      throw StenoLinkError.badURL(spec.url)
    }
    guard let fingerprint = Data(base64Encoded: spec.fingerprint), fingerprint.count == 32 else {
      throw StenoLinkError.badFingerprint
    }
    let chunkURL = try UploadSession.chunkFileURL(for: spec.taskID)
    let digest = try FileHashing.copySlice(
      of: URL(fileURLWithPath: spec.filePath),
      offset: spec.offset,
      length: spec.length,
      to: chunkURL
    )

    var request = URLRequest(url: url)
    request.httpMethod = "PUT"
    for (name, value) in spec.headers {
      request.setValue(value, forHTTPHeaderField: name)
    }
    request.setValue(digest.base64EncodedString(), forHTTPHeaderField: UploadSession.chunkHashHeader)
    request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")

    let task = session.uploadTask(with: request, fromFile: chunkURL)
    let info = TaskInfo(taskID: spec.taskID, fingerprint: spec.fingerprint)
    task.taskDescription = String(data: try JSONEncoder().encode(info), encoding: .utf8)
    task.resume()
  }

  func cancel(taskID: String, completion: @escaping () -> Void) {
    session.getAllTasks { tasks in
      for task in tasks where UploadSession.info(of: task)?.taskID == taskID {
        task.cancel()
      }
      completion()
    }
  }

  func pendingTaskIDs(completion: @escaping ([String]) -> Void) {
    session.getAllTasks { tasks in
      completion(tasks.compactMap { UploadSession.info(of: $0)?.taskID })
    }
  }

  // MARK: - URLSessionTaskDelegate

  func urlSession(
    _ session: URLSession,
    task: URLSessionTask,
    didReceive challenge: URLAuthenticationChallenge,
    completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
  ) {
    guard let info = UploadSession.info(of: task),
      let fingerprint = Data(base64Encoded: info.fingerprint)
    else {
      completionHandler(.cancelAuthenticationChallenge, nil)
      return
    }
    let (disposition, credential) = PinnedTrustEvaluator.respond(to: challenge, pinnedFingerprint: fingerprint)
    if disposition == .cancelAuthenticationChallenge {
      lock.lock()
      pinRejectedTasks.insert(task.taskIdentifier)
      lock.unlock()
    }
    completionHandler(disposition, credential)
  }

  func urlSession(
    _ session: URLSession,
    task: URLSessionTask,
    didSendBodyData bytesSent: Int64,
    totalBytesSent: Int64,
    totalBytesExpectedToSend: Int64
  ) {
    guard let info = UploadSession.info(of: task) else { return }
    eventSink?(
      "uploadProgress",
      [
        "taskID": info.taskID,
        "bytesSent": totalBytesSent,
        "totalBytes": totalBytesExpectedToSend,
      ])
  }

  func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
    lock.lock()
    responseBodies[dataTask.taskIdentifier, default: Data()].append(data)
    lock.unlock()
  }

  func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
    lock.lock()
    let body = responseBodies.removeValue(forKey: task.taskIdentifier) ?? Data()
    let pinRejected = pinRejectedTasks.remove(task.taskIdentifier) != nil
    lock.unlock()

    guard let info = UploadSession.info(of: task) else { return }
    if let chunkURL = try? UploadSession.chunkFileURL(for: info.taskID) {
      try? FileManager.default.removeItem(at: chunkURL)
    }

    if let error {
      // A rejected pin arrives as a cancellation too; name it, and let the
      // coordinator back off like any other failure (the Mac may have been
      // re-paired, or another Mac took the name for a moment).
      let cancelled = (error as NSError).code == NSURLErrorCancelled
      let message = pinRejected ? StenoLinkError.pinMismatch.localizedDescription : error.localizedDescription
      eventSink?(
        "uploadFailed",
        [
          "taskID": info.taskID,
          "message": message,
          "retryable": pinRejected || !cancelled,
        ])
      return
    }
    guard let http = task.response as? HTTPURLResponse else {
      eventSink?(
        "uploadFailed",
        ["taskID": info.taskID, "message": "Response was not HTTP", "retryable": true])
      return
    }
    eventSink?(
      "uploadFinished",
      [
        "taskID": info.taskID,
        "status": http.statusCode,
        "body": String(data: body, encoding: .utf8) ?? "",
      ])
  }

  func urlSessionDidFinishEvents(forBackgroundURLSession session: URLSession) {
    DispatchQueue.main.async {
      self.backgroundCompletionHandler?()
      self.backgroundCompletionHandler = nil
    }
  }

  // MARK: - Helpers

  private static func info(of task: URLSessionTask) -> TaskInfo? {
    guard let description = task.taskDescription, let data = description.data(using: .utf8) else {
      return nil
    }
    return try? JSONDecoder().decode(TaskInfo.self, from: data)
  }

  /// `tmp/steno-upload/<taskID>.bin`; task ids are `<recordingID>/<chunk>`.
  private static func chunkFileURL(for taskID: String) throws -> URL {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent("steno-upload", isDirectory: true)
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    let safeName = taskID.replacingOccurrences(of: "/", with: "_")
    return directory.appendingPathComponent("\(safeName).bin")
  }
}
