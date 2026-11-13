import Foundation
import Security

/// The PC this iPhone is paired with: where Swiftlane runs, and the pairing code.
struct Pairing: Equatable {
    let base: URL      // e.g. http://192.168.1.24:8765/
    let code: String   // the pairing code from the QR code (kept in the Keychain)

    /// The page to open: the PC's phone page, with the code after "#" so it never travels to the network.
    var pageURL: URL {
        var c = URLComponents(url: base, resolvingAgainstBaseURL: false)!
        c.fragment = code
        return c.url!
    }

    /// Reads the link from the QR code on the PC ("http://192.168.1.24:8765/#CODE").
    /// Only accepts addresses on your own network, so a QR code from somewhere else can't
    /// point the app at a website on the internet.
    static func parse(_ text: String) -> Pairing? {
        guard var c = URLComponents(string: text.trimmingCharacters(in: .whitespacesAndNewlines)),
              let scheme = c.scheme?.lowercased(), scheme == "http" || scheme == "https",
              let host = c.host, isOwnNetwork(host),
              let code = c.fragment, !code.isEmpty, code.count <= 200 else { return nil }
        c.scheme = scheme
        c.path = "/"
        c.query = nil
        c.fragment = nil
        c.user = nil
        c.password = nil
        guard let base = c.url else { return nil }
        return Pairing(base: base, code: code)
    }

    /// Private IPv4 ranges (home networks, link-local, Tailscale) and .local names.
    static func isOwnNetwork(_ host: String) -> Bool {
        let h = host.lowercased()
        if h.hasSuffix(".local") { return true }
        let parts = h.split(separator: ".").compactMap { Int($0) }
        guard parts.count == 4, parts.allSatisfy({ (0...255).contains($0) }) else { return false }
        switch (parts[0], parts[1]) {
        case (10, _), (192, 168), (169, 254): return true
        case (172, 16...31): return true
        case (100, 64...127): return true   // Tailscale addresses
        default: return false
        }
    }

    /// Two PCs are the same site if scheme, host and port match.
    func isSameSite(_ url: URL) -> Bool {
        url.scheme?.lowercased() == base.scheme && url.host?.lowercased() == base.host?.lowercased()
            && (url.port ?? defaultPort(url)) == (base.port ?? defaultPort(base))
    }

    private func defaultPort(_ url: URL) -> Int { url.scheme == "https" ? 443 : 80 }
}

/// Keeps the pairing in the iPhone's Keychain (only readable by this app, only on this device).
final class PairingStore: ObservableObject {
    @Published private(set) var pairing: Pairing?
    private let service = "io.github.lewy366.swiftlane.pairing"

    init() {
        if let text = load() { pairing = Pairing.parse(text) }
    }

    func save(_ p: Pairing) {
        store(p.pageURL.absoluteString)
        pairing = p
    }

    func forget() {
        SecItemDelete(query() as CFDictionary)
        pairing = nil
    }

    private func query() -> [String: Any] {
        [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: "pc"]
    }

    private func store(_ text: String) {
        SecItemDelete(query() as CFDictionary)
        var q = query()
        q[kSecValueData as String] = Data(text.utf8)
        q[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        SecItemAdd(q as CFDictionary, nil)
    }

    private func load() -> String? {
        var q = query()
        q[kSecReturnData as String] = true
        q[kSecMatchLimit as String] = kSecMatchLimitOne
        var out: AnyObject?
        guard SecItemCopyMatching(q as CFDictionary, &out) == errSecSuccess, let data = out as? Data else { return nil }
        return String(data: data, encoding: .utf8)
    }
}
