import AppKit
import UniformTypeIdentifiers
import os

// Push's Share extension. It never uploads and never sees the token (no network, no keychain):
// it turns whatever was shared into file URLs and hands them to the containing Push.app as an
// ordinary "open these documents" request, then completes straight away.
//
// Debug: log stream --level debug --predicate 'subsystem == "io.github.itsfutsy.push.share"'

private let log = Logger(subsystem: "io.github.itsfutsy.push.share", category: "share")

@objc(ShareViewController) // Info.plist NSExtensionPrincipalClass, unmangled
final class ShareViewController: NSViewController {
    override func loadView() {
        view = NSView(frame: NSRect(x: 0, y: 0, width: 1, height: 1)) // no nib, no visible UI
        preferredContentSize = .zero
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        let items = extensionContext?.inputItems as? [NSExtensionItem] ?? []
        let providers = items.flatMap { $0.attachments ?? [] }
        log.info("share request: \(items.count, privacy: .public) item(s), \(providers.count, privacy: .public) attachment(s)")
        Task {
            // Load every attachment concurrently (Photos exports can be slow), keeping their order.
            let loads = providers.map { provider in Task { await Intake.resolve(provider) } }
            var urls: [URL] = []
            for load in loads {
                if let url = await load.value { urls.append(url) }
            }
            await Intake.handOff(urls)
            // Always complete, also on errors and when nothing usable was shared.
            extensionContext?.completeRequest(returningItems: nil)
        }
    }
}

enum Intake {
    /// ~/Library/Caches/io.github.itsfutsy.push/inbox. Inside the sandbox NSHomeDirectory() is the container,
    /// so resolve the real home via getpwuid. Writable through the temporary-exception entitlement.
    static let inbox: URL = {
        let home = getpwuid(getuid()).map { String(cString: $0.pointee.pw_dir) } ?? NSHomeDirectory()
        return URL(fileURLWithPath: home, isDirectory: true)
            .appendingPathComponent("Library/Caches/io.github.itsfutsy.push/inbox", isDirectory: true)
    }()

    /// One attachment -> one local file URL Push can read after this extension has gone away.
    /// Main actor because NSItemProvider is not Sendable; the loads themselves run in the background.
    @MainActor
    static func resolve(_ provider: NSItemProvider) async -> URL? {
        let types = provider.registeredTypeIdentifiers
        log.debug("attachment types: \(types, privacy: .public)")
        // File URLs first (Finder, Photos exports). loadObject(URL) also sidesteps a Photos bug
        // where loadFileRepresentation fails for .jpeg files exported as .jpg.
        if provider.canLoadObject(ofClass: URL.self), let url = await loadFileURL(provider) {
            return url
        }
        // Data-only attachments: take the first (highest fidelity) concrete data type.
        let dataType = types.first { id in
            guard let type = UTType(id) else { return false }
            return type.conforms(to: .data) && !type.conforms(to: .url)
        }
        guard let dataType else {
            log.info("skipped an attachment with no file or data")
            return nil
        }
        let suggested = provider.suggestedName
        return await withCheckedContinuation { done in
            _ = provider.loadFileRepresentation(forTypeIdentifier: dataType) { tmp, error in
                if let error { log.error("loading data failed: \(error.localizedDescription, privacy: .public)") }
                // The temporary file is deleted when this handler returns, so copy it now.
                done.resume(returning: tmp.flatMap { copyToInbox($0, name: fileName(suggested, tmp: $0, type: dataType)) })
            }
        }
    }

    /// The attachment's file URL, or nil if it has none (web links, folders, failures).
    @MainActor
    private static func loadFileURL(_ provider: NSItemProvider) async -> URL? {
        await withCheckedContinuation { done in
            _ = provider.loadObject(ofClass: URL.self) { url, error in
                if let error { log.error("loading URL failed: \(error.localizedDescription, privacy: .public)") }
                guard let url, url.isFileURL else { return done.resume(returning: nil) }
                guard !isDirectory(url) else {
                    log.info("skipped a folder or package")
                    return done.resume(returning: nil)
                }
                guard isTransient(url) else { return done.resume(returning: url) } // the original file
                // Copy inside the handler, before the host can clean up.
                done.resume(returning: copyToInbox(url, name: url.lastPathComponent) ?? url)
            }
        }
    }

    /// Files in temp folders or other apps' containers (Photos and Mail exports) can vanish once the
    /// share completes, or be privacy protected from Push, so those are copied. Everything else is a
    /// real file the user picked and passes through as its original path.
    private static func isTransient(_ url: URL) -> Bool {
        let path = url.path
        return ["/var/folders/", "/private/var/folders/", "/tmp/", "/private/tmp/"].contains { path.hasPrefix($0) }
            || path.contains("/Library/Containers/") || path.contains("/Library/Group Containers/")
    }

    /// Prefer the provider's suggested name ("IMG_0042"), keeping the real file extension.
    /// Never a path: no slashes or colons, no leading dots.
    private static func fileName(_ suggested: String?, tmp: URL, type: String) -> String {
        let name = String((suggested ?? "")
            .replacingOccurrences(of: "/", with: "-")
            .replacingOccurrences(of: ":", with: "-")
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .drop { $0 == "." })
        guard !name.isEmpty else { return tmp.lastPathComponent }
        let ext = tmp.pathExtension.isEmpty ? (UTType(type)?.preferredFilenameExtension ?? "") : tmp.pathExtension
        let hasExt = (name as NSString).pathExtension.caseInsensitiveCompare(ext) == .orderedSame
        return ext.isEmpty || hasExt ? name : "\(name).\(ext)"
    }

    /// Copies (APFS clones) `source` into a new inbox/<uuid>/ folder. One file per folder, because
    /// Push deletes the folder once its file is uploaded.
    private static func copyToInbox(_ source: URL, name: String) -> URL? {
        let dir = inbox.appendingPathComponent(UUID().uuidString, isDirectory: true)
        do {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            let dest = dir.appendingPathComponent(name)
            try copy(source, to: dest)
            return dest
        } catch {
            log.error("copy to inbox failed: \(error.localizedDescription, privacy: .public)")
            try? FileManager.default.removeItem(at: dir)
            return nil
        }
    }

    /// Folders and packages (.app, .rtfd) are directories. Push can't upload them, and copying one
    /// would duplicate its whole tree into the inbox. Unreadable metadata is left for Push to judge.
    private static func isDirectory(_ url: URL) -> Bool {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        return (try? url.resourceValues(forKeys: [.isDirectoryKey]))?.isDirectory == true
    }

    /// Copies (APFS clones) a regular file. Anything else (folders, packages, symlinks) is refused.
    private static func copy(_ source: URL, to dest: URL) throws {
        let scoped = source.startAccessingSecurityScopedResource()
        defer { if scoped { source.stopAccessingSecurityScopedResource() } }
        guard try source.resourceValues(forKeys: [.isRegularFileKey]).isRegularFile == true else {
            throw POSIXError(.EFTYPE)
        }
        try FileManager.default.copyItem(at: source, to: dest)
    }

    @MainActor
    static func handOff(_ urls: [URL]) async {
        guard !urls.isEmpty else { return log.info("nothing usable was shared") }
        // .../Push.app/Contents/PlugIns/PushShare.appex -> .../Push.app
        let app = Bundle.main.bundleURL
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        do {
            _ = try await NSWorkspace.shared.open(urls, withApplicationAt: app, configuration: quietOpen())
            log.info("handed \(urls.count, privacy: .public) file(s) to Push")
        } catch {
            log.error("document open failed (\(error.localizedDescription, privacy: .public)), using push:// fallback")
            await openViaInbox(urls)
        }
    }

    /// Fallback if LaunchServices refuses the document open: gather every file into one
    /// inbox/<uuid>/ folder and send Push only that opaque id. Push must never accept paths over
    /// push://, or any app or web page could make it upload arbitrary files.
    @MainActor
    private static func openViaInbox(_ urls: [URL]) async {
        let id = UUID().uuidString
        let dir = inbox.appendingPathComponent(id, isDirectory: true)
        let fm = FileManager.default
        var count = 0
        do {
            try fm.createDirectory(at: dir, withIntermediateDirectories: true)
            for url in urls {
                let dest = uniqueURL(in: dir, name: url.lastPathComponent)
                do {
                    if url.path.hasPrefix(inbox.path + "/") {
                        // Already copied into its own inbox folder: move it over and drop the empty folder.
                        try fm.moveItem(at: url, to: dest)
                        try? fm.removeItem(at: url.deletingLastPathComponent())
                    } else {
                        try copy(url, to: dest)
                    }
                    count += 1
                } catch {
                    log.error("fallback copy failed: \(error.localizedDescription, privacy: .public)")
                }
            }
        } catch {
            log.error("creating the inbox failed: \(error.localizedDescription, privacy: .public)")
        }
        guard count > 0 else {
            try? fm.removeItem(at: dir)
            return
        }
        var link = URLComponents()
        link.scheme = "push"
        link.host = "upload"
        link.queryItems = [URLQueryItem(name: "id", value: id)]
        guard let url = link.url else { return }
        do {
            _ = try await NSWorkspace.shared.open(url, configuration: quietOpen())
            log.info("handed \(count, privacy: .public) file(s) to Push via the inbox")
        } catch {
            log.error("push:// open failed: \(error.localizedDescription, privacy: .public)")
        }
    }

    /// "name.ext", or "name 2.ext", "name 3.ext"... if taken.
    private static func uniqueURL(in dir: URL, name: String) -> URL {
        let base = (name as NSString).deletingPathExtension, ext = (name as NSString).pathExtension
        var url = dir.appendingPathComponent(name)
        var n = 2
        while FileManager.default.fileExists(atPath: url.path) {
            url = dir.appendingPathComponent(ext.isEmpty ? "\(base) \(n)" : "\(base) \(n).\(ext)")
            n += 1
        }
        return url
    }

    /// Open in the background without touching Recent Items.
    @MainActor
    private static func quietOpen() -> NSWorkspace.OpenConfiguration {
        let config = NSWorkspace.OpenConfiguration()
        config.activates = false
        config.addsToRecentItems = false
        return config
    }
}
