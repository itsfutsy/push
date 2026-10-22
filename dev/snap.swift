// Renders a page in a real offscreen WKWebView and writes a PNG. Preview harness only.
// usage: snap <url> <out.png> <width> <height> <light|dark>
//   Waits for <html data-ready="1"> (up to 55 s), then snapshots. Console errors, uncaught exceptions and
//   CSP violations from every frame are printed as "console: ..." lines.
// build: swiftc -O dev/snap.swift -o /tmp/push-snap
import AppKit
import WebKit

let args = CommandLine.arguments
guard args.count >= 6 else {
  print("usage: snap <url> <out.png> <width> <height> <light|dark>"); exit(2)
}
let target = args[1], out = args[2]
let w = CGFloat(Double(args[3])!), h = CGFloat(Double(args[4])!)
let dark = args[5] == "dark"
let scale: CGFloat = 2

// Forward console errors and CSP violations from all frames.
let probe = """
(() => {
  const send = (m) => window.webkit.messageHandlers.log.postMessage(String(m));
  for (const level of ['error', 'warn']) {
    const orig = console[level];
    console[level] = (...a) => { send(level + ': ' + a.join(' ')); orig.apply(console, a); };
  }
  addEventListener('error', (e) => send('exception: ' + e.message + ' at ' + e.filename + ':' + e.lineno));
  addEventListener('unhandledrejection', (e) => send('unhandled rejection: ' + e.reason));
  document.addEventListener('securitypolicyviolation', (e) => send('CSP: ' + e.violatedDirective + ' blocked ' + e.blockedURI));
})();
"""

final class Snapper: NSObject, WKNavigationDelegate, WKScriptMessageHandler {
  let web: WKWebView
  var tries = 0

  init(frame: NSRect) {
    let config = WKWebViewConfiguration()
    config.userContentController.addUserScript(WKUserScript(source: probe, injectionTime: .atDocumentStart, forMainFrameOnly: false))
    web = WKWebView(frame: frame, configuration: config)
    super.init()
    // An offscreen window counts as occluded, which hides the page and freezes its animations.
    // WebKit SPI turns occlusion detection off so the page renders as if on screen.
    let sel = NSSelectorFromString("_setWindowOcclusionDetectionEnabled:")
    if web.responds(to: sel) {
      typealias Setter = @convention(c) (AnyObject, Selector, Bool) -> Void
      unsafeBitCast(web.method(for: sel), to: Setter.self)(web, sel, false)
    }
    config.userContentController.add(self, name: "log")
    web.navigationDelegate = self
  }

  func userContentController(_ c: WKUserContentController, didReceive m: WKScriptMessage) {
    print("console:", m.body)
  }

  func webView(_ wv: WKWebView, didFinish n: WKNavigation!) { poll() }

  func webView(_ wv: WKWebView, didFail n: WKNavigation!, withError e: Error) { print("load failed:", e); exit(1) }
  func webView(_ wv: WKWebView, didFailProvisionalNavigation n: WKNavigation!, withError e: Error) { print("load failed:", e); exit(1) }

  func poll() {
    web.evaluateJavaScript("document.documentElement.dataset.ready === '1'") { ready, _ in
      self.tries += 1
      if (ready as? Bool) == true || self.tries > 550 {
        if self.tries > 550 { print("warning: page never set data-ready, snapshotting anyway") }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) { self.snap() }
      } else {
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) { self.poll() }
      }
    }
  }

  func snap() {
    let cfg = WKSnapshotConfiguration()
    cfg.afterScreenUpdates = true
    web.takeSnapshot(with: cfg) { img, err in
      guard let img else { print("snapshot failed:", err as Any); exit(1) }
      // Redraw at an explicit scale so the output doesn't depend on the display.
      let px = NSSize(width: w * scale, height: h * scale)
      guard let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(px.width), pixelsHigh: Int(px.height),
                                       bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                                       colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0) else { exit(1) }
      rep.size = NSSize(width: w, height: h)
      NSGraphicsContext.saveGraphicsState()
      NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
      NSGraphicsContext.current?.imageInterpolation = .high
      img.draw(in: NSRect(x: 0, y: 0, width: w, height: h))
      NSGraphicsContext.restoreGraphicsState()
      guard let png = rep.representation(using: .png, properties: [:]) else { exit(1) }
      do { try png.write(to: URL(fileURLWithPath: out)) } catch { print("write failed:", error); exit(1) }
      print("wrote", out, rep.pixelsWide, rep.pixelsHigh)
      exit(0)
    }
  }
}

let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
let window = NSWindow(contentRect: NSRect(x: -10000, y: -10000, width: w, height: h), styleMask: [.borderless], backing: .buffered, defer: false)
window.appearance = NSAppearance(named: dark ? .darkAqua : .aqua)
let snapper = Snapper(frame: NSRect(x: 0, y: 0, width: w, height: h))
snapper.web.appearance = window.appearance
window.contentView!.addSubview(snapper.web)
window.orderFrontRegardless()

snapper.web.load(URLRequest(url: URL(string: target)!))
DispatchQueue.main.asyncAfter(deadline: .now() + 60) { print("timeout"); exit(1) }
app.run()
