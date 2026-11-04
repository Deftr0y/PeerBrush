// External test driver: read only this editor's rendered windows and send ordinary UI events.
import AppKit
import Vision

func run() throws -> Bool {
    let args = CommandLine.arguments
    guard args.count == 4, let pid = Int32(args[1]),
          let app = NSRunningApplication(processIdentifier: pid),
          app.bundleIdentifier == "com.peerbrush.PeerBrush" else { return false }
    if args[2] == "quit" { return app.terminate() }
    guard ["click", "see"].contains(args[2]) else { return false }
    if args[2] == "click" {
        guard app.activate(options: [.activateIgnoringOtherApps]) else { return false }
    }
    let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
    for row in windows where row[kCGWindowOwnerPID as String] as? Int32 == pid {
        guard let id = row[kCGWindowNumber as String] as? Int,
              let bounds = row[kCGWindowBounds as String] as? NSDictionary,
              let frame = CGRect(dictionaryRepresentation: bounds as CFDictionary), frame.width > 80 else { continue }
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + ".png")
        defer { try? FileManager.default.removeItem(at: url) }
        let capture = Process()
        capture.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
        capture.arguments = ["-x", "-o", "-l", String(id), url.path]
        try capture.run()
        capture.waitUntilExit()
        guard capture.terminationStatus == 0 else { continue }
        let text = VNRecognizeTextRequest()
        text.recognitionLevel = .accurate
        try VNImageRequestHandler(url: url).perform([text])
        for observation in text.results ?? [] {
            guard let best = observation.topCandidates(1).first,
                  let range = best.string.range(of: args[3], options: [.caseInsensitive]) else { continue }
            if args[2] == "see" { return true }
            guard let match = try best.boundingBox(for: range) else { continue }
            let point = CGPoint(x: frame.minX + match.boundingBox.midX * frame.width,
                                y: frame.minY + (1 - match.boundingBox.midY) * frame.height)
            for kind in [CGEventType.mouseMoved, .leftMouseDown, .leftMouseUp] {
                guard let event = CGEvent(mouseEventSource: nil, mouseType: kind, mouseCursorPosition: point,
                                          mouseButton: .left) else { return false }
                event.postToPid(pid)
                Thread.sleep(forTimeInterval: 0.08)
            }
            return true
        }
    }
    return false
}
do { exit(try run() ? 0 : 1) } catch { exit(2) }
