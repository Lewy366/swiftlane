import SwiftUI

@main
struct SwiftlaneApp: App {
    @StateObject private var store = PairingStore()

    var body: some Scene {
        WindowGroup {
            Group {
                if let pairing = store.pairing {
                    PhonePage(pairing: pairing, onForget: { store.forget() })
                } else {
                    PairView { store.save($0) }
                }
            }
            .preferredColorScheme(.dark)
        }
    }
}

/// Swiftlane's own phone page from the PC, full screen, with a small menu in the corner.
struct PhonePage: View {
    let pairing: Pairing
    let onForget: () -> Void
    @State private var reloadToken = 0
    @State private var failed: String?
    @State private var confirmForget = false

    var body: some View {
        ZStack(alignment: .topTrailing) {
            Color(red: 0.03, green: 0.03, blue: 0.05).ignoresSafeArea()
            WebView(pairing: pairing, reloadToken: reloadToken, failed: $failed)
                .ignoresSafeArea()
            if let failed {
                VStack(spacing: 14) {
                    Image(systemName: "wifi.exclamationmark").font(.system(size: 44)).foregroundStyle(.secondary)
                    Text("Can't reach your PC").font(.title2.bold())
                    Text(failed).multilineTextAlignment(.center).foregroundStyle(.secondary)
                    Text("Is Swiftlane open on the PC, and is this iPhone on the same Wi-Fi?")
                        .font(.footnote).multilineTextAlignment(.center).foregroundStyle(.secondary)
                    Button("Try again") { self.failed = nil; reloadToken += 1 }.buttonStyle(.borderedProminent)
                    Button("Pair with another PC") { confirmForget = true }
                }
                .padding(28)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Color(red: 0.03, green: 0.03, blue: 0.05))
            }
            Menu {
                Button { reloadToken += 1 } label: { Label("Reload", systemImage: "arrow.clockwise") }
                Button(role: .destructive) { confirmForget = true } label: { Label("Pair with another PC", systemImage: "qrcode.viewfinder") }
            } label: {
                Image(systemName: "ellipsis")
                    .font(.system(size: 15, weight: .semibold))
                    .frame(width: 32, height: 32)
                    .background(.ultraThinMaterial, in: Circle())
            }
            .padding(.trailing, 12)
            .padding(.top, 2)
            .accessibilityLabel("Menu")
        }
        .confirmationDialog("Forget this PC?", isPresented: $confirmForget, titleVisibility: .visible) {
            Button("Forget and scan again", role: .destructive, action: onForget)
        } message: {
            Text("You'll need to scan the QR code on a PC again to connect.")
        }
    }
}
