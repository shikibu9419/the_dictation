import AVFoundation
import Foundation

// JSONL on stdin: {"type":"audio","item":"<id>","rate":24000,"pcm":"<base64 s16le mono>"} and {"type":"clear"}.
// JSONL on stdout: {"type":"ready"}, {"type":"played","item":"<id>","ms":N} while playing, {"type":"cleared"}.
// `played` names the item under the play head and how much of it has been heard.
final class Player {
    private let engine = AVAudioEngine()
    private let node = AVAudioPlayerNode()
    private var format: AVAudioFormat?
    let queue = DispatchQueue(label: "PebbleJev.Playback")
    private var timer: DispatchSourceTimer?
    /// Scheduled buffers in play order since the last clear: (item, frames).
    private var scheduled: [(item: String, frames: Int)] = []

    func emit(_ value: [String: Any]) {
        guard let data = try? JSONSerialization.data(withJSONObject: value) else { return }
        FileHandle.standardOutput.write(data + Data([10]))
    }
    private func ensureEngine(rate: Double) throws {
        if let format, format.sampleRate == rate, engine.isRunning { return }
        if engine.isRunning { engine.stop() }
        guard let target = AVAudioFormat(standardFormatWithSampleRate: rate, channels: 1) else {
            throw NSError(domain: "Playback", code: 1, userInfo: [NSLocalizedDescriptionKey: "unsupported rate \(rate)"])
        }
        if node.engine == nil { engine.attach(node) }
        engine.connect(node, to: engine.mainMixerNode, format: target)
        engine.prepare()
        try engine.start()
        format = target
    }
    func push(base64: String, rate: Double, item: String) {
        guard let data = Data(base64Encoded: base64), data.count >= 2 else { return }
        do { try ensureEngine(rate: rate) } catch {
            emit(["type": "error", "text": error.localizedDescription]); return
        }
        let frames = data.count / 2
        guard let format, let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(frames)) else { return }
        buffer.frameLength = AVAudioFrameCount(frames)
        data.withUnsafeBytes { raw in
            let source = raw.bindMemory(to: Int16.self)
            let destination = buffer.floatChannelData![0]
            for i in 0..<frames { destination[i] = Float(Int16(littleEndian: source[i])) / 32768 }
        }
        scheduled.append((item, frames))
        node.scheduleBuffer(buffer, completionCallbackType: .dataPlayedBack) { [weak self] _ in
            self?.queue.async { self?.report() }
        }
        if !node.isPlaying { node.play() }
        startTimer()
    }
    func clear() {
        node.stop()
        node.reset()
        scheduled.removeAll()
        stopTimer()
        emit(["type": "cleared"])
    }
    private func playedFrames() -> Int {
        guard let nodeTime = node.lastRenderTime, let playerTime = node.playerTime(forNodeTime: nodeTime) else { return 0 }
        return Int(playerTime.sampleTime)
    }
    /// The item under the play head and the frames of it already rendered.
    private func position() -> (item: String, frames: Int)? {
        let head = playedFrames()
        var cursor = 0
        var current: (item: String, frames: Int)?
        for entry in scheduled {
            let start = cursor
            cursor += entry.frames
            if current?.item == entry.item {
                current!.frames += max(0, min(entry.frames, head - start))
            } else if head >= start || current == nil {
                current = (entry.item, max(0, min(entry.frames, head - start)))
            }
            if head < cursor { break }
        }
        return current
    }
    private func report() {
        guard let (item, frames) = position(), let rate = format?.sampleRate else { return }
        emit(["type": "played", "item": item, "ms": Int(Double(frames) * 1000 / rate)])
    }
    private func startTimer() {
        guard timer == nil else { return }
        let source = DispatchSource.makeTimerSource(queue: queue)
        source.schedule(deadline: .now() + 0.1, repeating: 0.1)
        source.setEventHandler { [weak self] in
            guard let self else { return }
            if self.node.isPlaying { self.report() } else { self.stopTimer() }
        }
        source.resume()
        timer = source
    }
    private func stopTimer() {
        timer?.cancel()
        timer = nil
    }
}

@main struct Main {
    static func main() {
        let player = Player()
        player.emit(["type": "ready"])
        DispatchQueue.global().async {
            while let line = readLine() {
                guard let data = line.data(using: .utf8),
                      let request = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { continue }
                player.queue.async {
                    switch request["type"] as? String {
                    case "audio":
                        let rate = (request["rate"] as? Double) ?? 24000
                        player.push(base64: request["pcm"] as? String ?? "", rate: rate, item: request["item"] as? String ?? "")
                    case "clear":
                        player.clear()
                    default:
                        break
                    }
                }
            }
            player.queue.async { exit(0) }
        }
        dispatchMain()
    }
}
