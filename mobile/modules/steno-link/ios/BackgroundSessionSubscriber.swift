import ExpoModulesCore

/// Receives `application(_:handleEventsForBackgroundURLSession:completionHandler:)`
/// through expo-modules-core's subscriber manager when iOS relaunches the app
/// to finish uploads. Recreating `UploadSession.shared`'s session lets the
/// delegate drain the queued events; the stored completion handler is called
/// from `urlSessionDidFinishEvents`.
///
/// Registered in `expo-module.config.json` under `appDelegateSubscribers`.
public final class StenoLinkAppDelegateSubscriber: ExpoAppDelegateSubscriber {
  public func application(
    _ application: UIApplication,
    handleEventsForBackgroundURLSession identifier: String,
    completionHandler: @escaping () -> Void
  ) {
    guard identifier == UploadSession.identifier else {
      // Another module's session; the manager counts every subscriber.
      completionHandler()
      return
    }
    UploadSession.shared.backgroundCompletionHandler = completionHandler
    UploadSession.shared.reconnect()
  }
}
