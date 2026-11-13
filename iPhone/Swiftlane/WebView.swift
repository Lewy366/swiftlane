import SwiftUI
import WebKit

/// Shows the PC's phone page. Only the paired PC is shown inside the app: any other link opens
/// in Safari. Files from the PC download into the app and then go to the share sheet ("Save to Files").
struct WebView: UIViewRepresentable {
    let pairing: Pairing
    let reloadToken: Int
    @Binding var failed: String?

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeUIView(context: Context) -> WKWebView {
        let config = WKWebViewConfiguration()
        config.allowsInlineMediaPlayback = true
        let web = WKWebView(frame: .zero, configuration: config)
        web.navigationDelegate = context.coordinator
        web.uiDelegate = context.coordinator
        web.isOpaque = false
        web.backgroundColor = .clear
        web.scrollView.contentInsetAdjustmentBehavior = .never   // the page handles the notch itself
        web.allowsBackForwardNavigationGestures = false
        context.coordinator.lastToken = reloadToken
        web.load(URLRequest(url: pairing.pageURL))
        return web
    }

    func updateUIView(_ web: WKWebView, context: Context) {
        context.coordinator.parent = self
        if context.coordinator.lastToken != reloadToken {
            context.coordinator.lastToken = reloadToken
            web.load(URLRequest(url: pairing.pageURL))
        }
    }

    final class Coordinator: NSObject, WKNavigationDelegate, WKUIDelegate, WKDownloadDelegate {
        var parent: WebView
        var lastToken = 0
        private var files: [WKDownload: URL] = [:]

        init(_ parent: WebView) { self.parent = parent }

        // ---- what may load inside the app
        func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction,
                     decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
            guard let url = action.request.url else { return decisionHandler(.cancel) }
            if action.shouldPerformDownload { return decisionHandler(.download) }
            if parent.pairing.isSameSite(url) || url.scheme == "about" || url.scheme == "blob" {
                return decisionHandler(.allow)
            }
            if action.navigationType == .linkActivated, ["http", "https"].contains(url.scheme ?? "") {
                UIApplication.shared.open(url)   // other sites open in Safari, never inside the app
            }
            decisionHandler(.cancel)
        }

        func webView(_ webView: WKWebView, decidePolicyFor response: WKNavigationResponse,
                     decisionHandler: @escaping (WKNavigationResponsePolicy) -> Void) {
            let attachment = (response.response as? HTTPURLResponse)?
                .value(forHTTPHeaderField: "Content-Disposition")?.lowercased().hasPrefix("attachment") ?? false
            decisionHandler(attachment || !response.canShowMIMEType ? .download : .allow)
        }

        func webView(_ webView: WKWebView, navigationAction: WKNavigationAction, didBecome download: WKDownload) { download.delegate = self }
        func webView(_ webView: WKWebView, navigationResponse: WKNavigationResponse, didBecome download: WKDownload) { download.delegate = self }

        func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) { parent.failed = nil }
        func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
            if (error as NSError).code != NSURLErrorCancelled { parent.failed = error.localizedDescription }
        }

        // ---- downloads: into a private temporary folder, then the share sheet
        func download(_ download: WKDownload, decideDestinationUsing response: URLResponse, suggestedFilename: String,
                      completionHandler: @escaping (URL?) -> Void) {
            let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
            try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
            let name = (suggestedFilename as NSString).lastPathComponent
            let url = folder.appendingPathComponent(name.isEmpty ? "download" : name)
            files[download] = url
            completionHandler(url)
        }

        func downloadDidFinish(_ download: WKDownload) {
            guard let url = files.removeValue(forKey: download) else { return }
            let sheet = UIActivityViewController(activityItems: [url], applicationActivities: nil)
            sheet.completionWithItemsHandler = { _, _, _, _ in try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }
            present(sheet)
        }

        func download(_ download: WKDownload, didFailWithError error: Error, resumeData: Data?) {
            if let url = files.removeValue(forKey: download) { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }
            alert("Download failed", error.localizedDescription)
        }

        // ---- the page's confirm() and alert() (shut down, sleep, ...)
        func webView(_ webView: WKWebView, runJavaScriptAlertPanelWithMessage message: String,
                     initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping () -> Void) {
            let a = UIAlertController(title: nil, message: message, preferredStyle: .alert)
            a.addAction(UIAlertAction(title: "OK", style: .default) { _ in completionHandler() })
            present(a, fallback: completionHandler)
        }

        func webView(_ webView: WKWebView, runJavaScriptConfirmPanelWithMessage message: String,
                     initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping (Bool) -> Void) {
            let a = UIAlertController(title: nil, message: message, preferredStyle: .alert)
            a.addAction(UIAlertAction(title: "Cancel", style: .cancel) { _ in completionHandler(false) })
            a.addAction(UIAlertAction(title: "OK", style: .default) { _ in completionHandler(true) })
            present(a, fallback: { completionHandler(false) })
        }

        private func alert(_ title: String, _ text: String) {
            let a = UIAlertController(title: title, message: text, preferredStyle: .alert)
            a.addAction(UIAlertAction(title: "OK", style: .default))
            present(a)
        }

        private func present(_ vc: UIViewController, fallback: (() -> Void)? = nil) {
            guard let top = Self.topController() else { fallback?(); return }
            if let pop = vc.popoverPresentationController {   // iPad: anchor the share sheet
                pop.sourceView = top.view
                pop.sourceRect = CGRect(x: top.view.bounds.midX, y: top.view.bounds.midY, width: 1, height: 1)
            }
            top.present(vc, animated: true)
        }

        static func topController() -> UIViewController? {
            let scene = UIApplication.shared.connectedScenes.first { $0.activationState == .foregroundActive } as? UIWindowScene
            var top = scene?.windows.first { $0.isKeyWindow }?.rootViewController
            while let next = top?.presentedViewController { top = next }
            return top
        }
    }
}
