import SwiftUI
import VisionKit

/// First start: straight to scanning the PC's QR code (with pasting the link as a fallback).
struct PairView: View {
    let onPaired: (Pairing) -> Void
    @State private var scanning = false
    @State private var message: String?

    var body: some View {
        VStack(spacing: 18) {
            Spacer()
            Image(systemName: "qrcode.viewfinder")
                .font(.system(size: 64, weight: .light))
                .foregroundStyle(Color(red: 0.08, green: 0.72, blue: 0.88))
            Text("Connect to your PC").font(.largeTitle.bold()).multilineTextAlignment(.center)
            Text("On your PC, open Swiftlane → Connect phone and click the blurred code to show it. Then scan it here.")
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
            if let message {
                Text(message).font(.callout).foregroundStyle(.orange).multilineTextAlignment(.center)
            }
            Spacer()
            Button {
                if QRScanner.canScan { scanning = true } else { message = "This iPhone can't scan here. Use \"Paste link\" instead." }
            } label: {
                Label("Scan QR code", systemImage: "camera").frame(maxWidth: .infinity).padding(.vertical, 6)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            Button("Paste link instead") { use(UIPasteboard.general.string ?? "") }
                .padding(.bottom, 8)
            Text("Your iPhone and PC need to be on the same Wi-Fi.").font(.footnote).foregroundStyle(.secondary)
        }
        .padding(28)
        .frame(maxWidth: 520)
        .sheet(isPresented: $scanning) {
            QRScanner { text in
                scanning = false
                use(text)
            }
            .ignoresSafeArea()
        }
    }

    private func use(_ text: String) {
        if let p = Pairing.parse(text) {
            message = nil
            onPaired(p)
        } else if text.isEmpty {
            message = "Nothing to paste. Copy the link from Connect phone on your PC first."
        } else {
            message = "That isn't a Swiftlane code from a PC on your network."
        }
    }
}

/// Apple's live text scanner, looking only for QR codes.
struct QRScanner: UIViewControllerRepresentable {
    let onFound: (String) -> Void

    static var canScan: Bool { DataScannerViewController.isSupported && DataScannerViewController.isAvailable }

    func makeUIViewController(context: Context) -> DataScannerViewController {
        let vc = DataScannerViewController(recognizedDataTypes: [.barcode(symbologies: [.qr])],
                                           qualityLevel: .balanced, isHighlightingEnabled: true)
        vc.delegate = context.coordinator
        try? vc.startScanning()
        return vc
    }

    func updateUIViewController(_ vc: DataScannerViewController, context: Context) {}

    func makeCoordinator() -> Coordinator { Coordinator(onFound: onFound) }

    final class Coordinator: NSObject, DataScannerViewControllerDelegate {
        let onFound: (String) -> Void
        private var done = false
        init(onFound: @escaping (String) -> Void) { self.onFound = onFound }

        func dataScanner(_ scanner: DataScannerViewController, didAdd items: [RecognizedItem], allItems: [RecognizedItem]) {
            for item in items {
                if case .barcode(let code) = item, let text = code.payloadStringValue, !done {
                    done = true
                    scanner.stopScanning()
                    onFound(text)
                    return
                }
            }
        }
    }
}
