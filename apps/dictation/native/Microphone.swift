import AppKit
import AVFoundation
import ApplicationServices

struct RightOption {
    private(set) var held = false
    mutating func update(keyCode: Int64, flags: UInt64) -> Bool? {
        guard keyCode == 61 else { return nil }
        let pressed = flags & 0x40 != 0
        guard pressed != held else { return nil }
        held = pressed
        return pressed
    }
    mutating func reset() { held = false }
}

final class MicrophoneSource: @unchecked Sendable {
    private let engine = AVAudioEngine()
    private let queue = DispatchQueue(label: "IndexVoice.Microphone")
    private var recording: String?
    private var held = false
    private var rightOption = RightOption()
    private var tap: CFMachPort?
    private var source: CFRunLoopSource?
    private var monitor: Any?

    private func emit(_ value: [String: Any]) {
        guard let data = try? JSONSerialization.data(withJSONObject: value) else { return }
        FileHandle.standardOutput.write(data + Data([10]))
    }
    private func state(_ pressed: Bool) {
        queue.async { [self] in
            guard pressed != held else { return }
            held = pressed
            if pressed {
                recording = UUID().uuidString
                emit(["type":"state", "collecting":true])
            } else if let key = recording {
                recording = nil
                emit(["type":"state", "collecting":false])
                emit(["type":"audio", "key":key, "rate":16000, "pcm":"", "final":true])
            }
        }
    }
    func key(_ event: CGEvent, type: CGEventType) {
        if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
            rightOption.reset()
            state(false)
            if let tap { CGEvent.tapEnable(tap: tap, enable: true) }
            return
        }
        if let pressed = rightOption.update(keyCode:event.getIntegerValueField(.keyboardEventKeycode),flags:event.flags.rawValue) { state(pressed) }
    }
    func start() throws {
        guard CGPreflightListenEventAccess() || CGRequestListenEventAccess() else {
            throw NSError(domain: "Microphone", code: 1, userInfo: [NSLocalizedDescriptionKey:"右Option入力には、システム設定の入力監視でIndex Voiceを許可してください"])
        }
        let mask = CGEventMask(1 << CGEventType.flagsChanged.rawValue)
        tap = CGEvent.tapCreate(tap: .cgSessionEventTap, place: .headInsertEventTap,
            options: .listenOnly, eventsOfInterest: mask,
            callback: { _, type, event, context in
                guard let context else { return Unmanaged.passUnretained(event) }
                let owner = Unmanaged<MicrophoneSource>.fromOpaque(context).takeUnretainedValue()
                owner.key(event, type: type)
                return Unmanaged.passUnretained(event)
            }, userInfo: Unmanaged.passUnretained(self).toOpaque())
        guard let tap else { throw NSError(domain:"Microphone",code:2,userInfo:[NSLocalizedDescriptionKey:"右Optionの監視を開始できませんでした"])}
        source = CFMachPortCreateRunLoopSource(nil, tap, 0)
        CFRunLoopAddSource(CFRunLoopGetMain(), source, .commonModes)
        CGEvent.tapEnable(tap: tap, enable: true)
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        guard format.sampleRate > 0, format.channelCount > 0,
              let target = AVAudioFormat(commonFormat:.pcmFormatInt16, sampleRate:16000, channels:1, interleaved:false),
              let converter = AVAudioConverter(from:format, to:target) else {
            throw NSError(domain:"Microphone",code:3,userInfo:[NSLocalizedDescriptionKey:"マイク入力フォーマットを取得できませんでした"])
        }
        input.installTap(onBus:0, bufferSize:1024, format:format) { [self] buffer, _ in
            let capacity = AVAudioFrameCount(ceil(Double(buffer.frameLength) * 16000 / format.sampleRate) + 32)
            guard let converted = AVAudioPCMBuffer(pcmFormat:target,frameCapacity:capacity) else { return }
            var supplied = false
            var error: NSError?
            converter.convert(to:converted,error:&error) { _, status in
                if supplied { status.pointee = .noDataNow; return nil }
                supplied = true; status.pointee = .haveData; return buffer
            }
            guard error == nil, let samples = converted.int16ChannelData?[0], converted.frameLength > 0 else { return }
            let data = Data(bytes:samples,count:Int(converted.frameLength)*2)
            queue.async { [self] in
                guard let key = recording else { return }
                emit(["type":"audio","key":key,"rate":16000,"pcm":data.base64EncodedString(),"final":false])
            }
        }
        engine.prepare()
        try engine.start()
        monitor = NSWorkspace.shared.notificationCenter.addObserver(forName:NSWorkspace.willSleepNotification,object:nil,queue:.main) { [self] _ in rightOption.reset(); state(false) }
        emit(["type":"source_ready"])
    }
    deinit {
        engine.stop()
        if let tap { CFMachPortInvalidate(tap) }
        if let monitor { NSWorkspace.shared.notificationCenter.removeObserver(monitor) }
    }
}
@main struct Main {
    static func main() async {
        if CommandLine.arguments.contains("--self-test") {
            var key = RightOption()
            precondition(key.update(keyCode:58,flags:0x80020) == nil)
            precondition(key.update(keyCode:61,flags:0x80060) == true)
            precondition(key.update(keyCode:61,flags:0x80060) == nil)
            precondition(key.update(keyCode:58,flags:0x80040) == nil)
            precondition(key.update(keyCode:61,flags:0x80020) == false)
            precondition(key.update(keyCode:61,flags:0x80040) == true)
            key.reset(); precondition(!key.held)
            print("Right Option transitions: 7 assertions passed")
            return
        }
        guard await AVCaptureDevice.requestAccess(for:.audio) else {
            FileHandle.standardError.write(Data("マイクのアクセスを許可してください\n".utf8)); exit(1)
        }
        await MainActor.run {
            let app = NSApplication.shared
            app.setActivationPolicy(.prohibited)
            let source = MicrophoneSource()
            do { try source.start() } catch {
                FileHandle.standardError.write(Data("\(error.localizedDescription)\n".utf8)); exit(1)
            }
            withExtendedLifetime(source) { app.run() }
        }
    }
}
