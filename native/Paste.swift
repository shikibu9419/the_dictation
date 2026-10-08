import AppKit
import ApplicationServices

@MainActor
final class PasteService {
    private var targets: [Int: (pid_t, AXUIElement?, AXUIElement?)] = [:]
    private var busy = false

    func emit(_ value: [String: Any]) {
        guard let data = try? JSONSerialization.data(withJSONObject: value) else { return }
        FileHandle.standardOutput.write(data + Data([10]))
    }
    func attribute(_ element: AXUIElement, _ key: String) -> CFTypeRef? {
        var value: CFTypeRef?
        return AXUIElementCopyAttributeValue(element, key as CFString, &value) == .success ? value : nil
    }
    func element(_ source: AXUIElement, _ key: String) -> AXUIElement? {
        guard let value = attribute(source, key), CFGetTypeID(value) == AXUIElementGetTypeID() else { return nil }
        return (value as! AXUIElement)
    }
    func permissions(prompt: Bool) {
        let trusted = AXIsProcessTrustedWithOptions([kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: prompt] as CFDictionary)
        emit(["type": "paste_permission", "allowed": trusted,
              "text": trusted ? "Background paste ready" : "自動貼り付けには Index Voice のアクセシビリティ許可が必要です"])
    }
    func pasteItem(_ node: AXUIElement, depth: Int = 0) -> AXUIElement? {
        if depth > 5 { return nil }
        if (attribute(node, kAXMenuItemCmdCharAttribute) as? String)?.lowercased() == "v",
           (attribute(node, kAXMenuItemCmdModifiersAttribute) as? NSNumber)?.intValue == 0,
           (attribute(node, kAXEnabledAttribute) as? Bool) == true {
            return node
        }
        for child in (attribute(node, kAXChildrenAttribute) as? [AXUIElement]) ?? [] {
            if let item = pasteItem(child, depth: depth + 1) { return item }
        }
        return nil
    }
    func handle(_ message: [String: Any]) async {
        let type = message["type"] as? String ?? ""
        if type == "permission" { permissions(prompt: true); return }
        let id = message["request"] as? Int ?? 0
        let pid = pid_t(message["target"] as? Int ?? 0)
        if type == "forget_target" { targets.removeValue(forKey: id); return }
        if type == "capture_target" {
            guard pid > 0, targets[id] == nil else { return }
            let app = AXUIElementCreateApplication(pid)
            targets[id] = (pid, element(app, kAXFocusedWindowAttribute), element(app, kAXFocusedUIElementAttribute))
            return
        }
        if type == "copy", let text = message["text"] as? String {
            while busy { try? await Task.sleep(for: .milliseconds(10)) }
            // Empty taps must preserve the user's existing clipboard.
            var success = true
            if !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                NSPasteboard.general.clearContents()
                success = NSPasteboard.general.setString(text, forType: .string)
            }
            emit(["type": "copy_result", "request": id, "success": success,
                  "text": success ? "" : "クリップボードへコピーできませんでした"])
            return
        }
        let currentClipboard = type == "paste_current"
        guard currentClipboard || type == "paste" else { return }
        guard currentClipboard || message["text"] is String else { return }
        let text = message["text"] as? String ?? ""
        func result(_ success: Bool, _ detail: String) {
            emit(["type": currentClipboard ? "gesture_paste_result" : "paste_result", "request": id, "success": success, "text": detail])
        }
        guard !busy else { result(false, "Paste request already in progress"); return }
        busy = true
        defer { busy = false; targets.removeValue(forKey: id) }
        if !currentClipboard {
            NSPasteboard.general.clearContents()
            guard NSPasteboard.general.setString(text, forType: .string) else {
                result(false, "クリップボードへコピーできませんでした"); return
            }
        }
        guard let app = NSRunningApplication(processIdentifier: pid), !app.isTerminated, pid != getpid() else {
            result(false, "貼り付け先が閉じられています。文字はコピー済みです"); return
        }
        let session = CGSessionCopyCurrentDictionary() as? [String: Any]
        guard session?["CGSSessionScreenIsLocked"] as? Bool != true,
              app.bundleIdentifier != "com.apple.loginwindow" else {
            result(false, "画面のロック中は貼り付けできません。文字はコピー済みです"); return
        }
        app.activate(options: [])
        guard AXIsProcessTrusted() else {
            result(false, "自動貼り付けには Index Voice のアクセシビリティ許可が必要です。文字はコピー済みです"); return
        }
        let target = targets[id]
        for _ in 0..<80 {
            try? await Task.sleep(for: .milliseconds(25))
            guard NSWorkspace.shared.frontmostApplication?.processIdentifier == pid else { continue }
            if let window = target?.1 { AXUIElementPerformAction(window, kAXRaiseAction as CFString) }
            if let focus = target?.2 { AXUIElementSetAttributeValue(focus, kAXFocusedAttribute as CFString, kCFBooleanTrue) }
            // Let the restored responder and the Enter key-up settle before dispatching Paste.
            try? await Task.sleep(for: .milliseconds(100))
            guard NSWorkspace.shared.frontmostApplication?.processIdentifier == pid else {
                result(false, "フォーカスが移動したため貼り付けを中止しました。文字はコピー済みです"); return
            }
            let axApp = AXUIElementCreateApplication(pid)
            if let menu = element(axApp, kAXMenuBarAttribute), let item = pasteItem(menu),
               AXUIElementPerformAction(item, kAXPressAction as CFString) == .success {
                result(true, "Paste dispatched through target application's menu"); return
            }
            guard CGPreflightPostEventAccess(),
                  let down = CGEvent(keyboardEventSource: nil, virtualKey: 9, keyDown: true),
                  let up = CGEvent(keyboardEventSource: nil, virtualKey: 9, keyDown: false) else {
                result(false, "貼り付けイベントの送信が許可されていません。文字はコピー済みです"); return
            }
            down.flags = .maskCommand
            up.flags = []
            down.postToPid(pid)
            up.postToPid(pid)
            result(true, "Paste keyboard shortcut dispatched to target application"); return
        }
        result(false, "元の入力先へ切り替えられませんでした。文字はコピー済みです")
    }
}

@main
struct Main {
    @MainActor static func main() {
        let app = NSApplication.shared
        app.setActivationPolicy(.prohibited)
        let service = PasteService()
        service.permissions(prompt: true)
        service.emit(["type": "ready"])
        DispatchQueue.global().async {
            while let line = readLine() {
                guard let data = line.data(using: .utf8),
                      let message = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { continue }
                Task { @MainActor in await service.handle(message) }
            }
            DispatchQueue.main.async { NSApplication.shared.terminate(nil) }
        }
        app.run()
    }
}
