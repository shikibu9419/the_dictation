import Foundation
import AVFoundation
import Speech
import CoreMedia

struct InputMessage: Decodable {
    let type: String
    let pcm: String?
    let sample_rate: Double?
}

let outputLock = NSLock()

func emit(_ value: [String: Any]) {
    outputLock.lock()
    defer { outputLock.unlock() }
    let data = try! JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
    FileHandle.standardOutput.write(data + Data([10]))
}

struct StreamError: Error, CustomStringConvertible {
    let description: String
    init(_ description: String) { self.description = description }
}

@available(macOS 26.0, *)
final class Session {
    let analyzer: SpeechAnalyzer
    let continuation: AsyncStream<AnalyzerInput>.Continuation
    let collector: Task<String, Error>
    let format: AVAudioFormat
    var converter: AVAudioConverter?
    var sourceRate: Double?
    var sampleCount: Int64 = 0
    var inputSampleCount: Int64 = 0

    private init(analyzer: SpeechAnalyzer, continuation: AsyncStream<AnalyzerInput>.Continuation,
                 collector: Task<String, Error>, format: AVAudioFormat) {
        self.analyzer = analyzer
        self.continuation = continuation
        self.collector = collector
        self.format = format
    }

    static func start(locale: Locale, batch: Bool) async throws -> Session {
        let started = ProcessInfo.processInfo.systemUptime
        emit(["type": "status", "text": "Preparing recognition session…"])
        let preset = batch ? SpeechTranscriber.Preset.transcription : SpeechTranscriber.Preset.progressiveTranscription
        let transcriber = SpeechTranscriber(
            locale: locale, transcriptionOptions: preset.transcriptionOptions,
            reportingOptions: batch ? preset.reportingOptions : preset.reportingOptions.union([.fastResults]),
            attributeOptions: preset.attributeOptions)
        guard let format = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [transcriber]) else {
            throw StreamError("No compatible SpeechAnalyzer audio format")
        }
        let analyzer = SpeechAnalyzer(modules: [transcriber])
        let (input, continuation) = AsyncStream<AnalyzerInput>.makeStream()
        let collector = Task<String, Error> {
            // Replace volatile results by their audio ranges, rather than appending revisions.
            var pieces: [(start: Double, end: Double, text: String)] = []
            var committed: [(start: Double, end: Double, text: String)] = []
            var previous = ""
            for try await result in transcriber.results {
                let start = CMTimeGetSeconds(result.range.start)
                let end = CMTimeGetSeconds(CMTimeRangeGetEnd(result.range))
                pieces.removeAll { $0.start == start || ($0.start < end && $0.end > start) }
                let piece = (start: start, end: end, text: String(result.text.characters))
                if result.isFinal {
                    emit(["type": "status", "text": String(format: "Final segment %.3f..%.3fs characters=%d", start, end, piece.text.count),
                          "segment_start": start, "segment_end": end])
                    // Final segments are immutable; a later overlapping volatile range
                    // must never remove the already finalized beginning of the recording.
                    if !committed.contains(where: { $0.start == start && $0.end == end }) {
                        committed.append(piece)
                    }
                } else {
                    pieces.append(piece)
                }
                let ordered = (committed + pieces).sorted { $0.start < $1.start }
                let text = ordered.map(\.text).joined().trimmingCharacters(in: .whitespacesAndNewlines)
                if !batch && text != previous {
                    emit(["type": "partial", "text": text])
                    previous = text
                }
            }
            return (committed + pieces).sorted { $0.start < $1.start }.map(\.text).joined().trimmingCharacters(in: .whitespacesAndNewlines)
        }
        do {
            try await analyzer.prepareToAnalyze(in: format)
            try await analyzer.start(inputSequence: input)
        } catch {
            continuation.finish()
            collector.cancel()
            await analyzer.cancelAndFinishNow()
            _ = await collector.result
            throw error
        }
        emit(["type": "status", "text": String(format: "Recognition session prepared in %.3fs", ProcessInfo.processInfo.systemUptime - started)])
        return Session(analyzer: analyzer, continuation: continuation, collector: collector, format: format)
    }

    func push(_ data: Data, rate: Double) throws {
        guard rate >= 1000, rate <= 192000, rate.isFinite, data.count % 2 == 0 else {
            throw StreamError("Invalid mono s16le PCM")
        }
        if data.isEmpty { return }
        if converter == nil {
            guard let source = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: rate,
                                             channels: 1, interleaved: false),
                  let conversion = AVAudioConverter(from: source, to: format) else {
                throw StreamError("Cannot convert ring PCM to analyzer format")
            }
            converter = conversion
            sourceRate = rate
        }
        guard sourceRate == rate, let converter else { throw StreamError("Sample rate changed during recording") }
        let frames = data.count / 2
        inputSampleCount += Int64(frames)
        guard let buffer = AVAudioPCMBuffer(pcmFormat: converter.inputFormat, frameCapacity: AVAudioFrameCount(frames)),
              let channel = buffer.floatChannelData?[0] else { throw StreamError("Cannot allocate PCM buffer") }
        buffer.frameLength = AVAudioFrameCount(frames)
        data.withUnsafeBytes { (bytes: UnsafeRawBufferPointer) in
            for index in 0..<frames {
                let word = UInt16(bytes[index * 2]) | (UInt16(bytes[index * 2 + 1]) << 8)
                channel[index] = Float(Int16(bitPattern: word)) / 32768
            }
        }
        let capacity = AVAudioFrameCount(ceil(Double(frames) * format.sampleRate / rate) + 64)
        var supplied = false
        try convert(capacity: capacity) { _, status in
            if supplied { status.pointee = .noDataNow; return nil }
            supplied = true
            status.pointee = .haveData
            return buffer
        }
    }

    private func convert(capacity: AVAudioFrameCount,
                         input: @escaping AVAudioConverterInputBlock) throws {
        guard let converter else { return }
        while true {
            guard let output = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: capacity) else {
                throw StreamError("Cannot allocate converted buffer")
            }
            var error: NSError?
            let status = converter.convert(to: output, error: &error, withInputFrom: input)
            if let error { throw error }
            if output.frameLength > 0 {
                let time = CMTime(value: sampleCount, timescale: CMTimeScale(format.sampleRate))
                continuation.yield(AnalyzerInput(buffer: output, bufferStartTime: time))
                sampleCount += Int64(output.frameLength)
            }
            switch status {
            case .haveData: continue
            case .inputRanDry, .endOfStream: return
            case .error: throw StreamError("PCM conversion failed")
            @unknown default: throw StreamError("Unknown PCM conversion status")
            }
        }
    }

    func finish(reportFinal: Bool = true) async throws {
        do {
            // Flush the resampler's delayed samples, then finalize the one continuous session.
            try convert(capacity: 4096) { _, status in status.pointee = .endOfStream; return nil }
            emit(["type": "status", "text": String(format: "Audio complete input=%.3fs converted=%.3fs", Double(inputSampleCount) / (sourceRate ?? 1), Double(sampleCount) / format.sampleRate)])
            continuation.finish()
            if sampleCount > 0 {
                try await analyzer.finalizeAndFinishThroughEndOfInput()
                let text = try await collector.value
                if reportFinal { emit(["type": "final", "text": text]) }
            } else {
                await cancel()
                if reportFinal { emit(["type": "final", "text": ""]) }
            }
        } catch {
            await cancel()
            throw error
        }
    }

    func cancel() async {
        continuation.finish()
        collector.cancel()
        await analyzer.cancelAndFinishNow()
        _ = await collector.result
    }
}

@main
struct SpeechStream {
    static func main() async {
        do {
            guard #available(macOS 26.0, *) else { throw StreamError("SpeechAnalyzer requires macOS 26+") }
            guard SpeechTranscriber.isAvailable else { throw StreamError("SpeechTranscriber unavailable on this Mac") }
            let requested = Locale(identifier: CommandLine.arguments.dropFirst().first ?? "ja-JP")
            guard let locale = await SpeechTranscriber.supportedLocale(equivalentTo: requested) else {
                throw StreamError("Unsupported speech language: \(requested.identifier)")
            }
            let module = SpeechTranscriber(locale: locale, preset: .progressiveTranscription)
            emit(["type": "status", "text": "Preparing Apple speech assets for \(locale.identifier)…"])
            if let installation = try await AssetInventory.assetInstallationRequest(supporting: [module]) {
                emit(["type": "status", "text": "Downloading Apple speech model…"])
                try await installation.downloadAndInstall()
            }
            guard await AssetInventory.status(forModules: [module]) == .installed else {
                throw StreamError("Apple speech assets for \(locale.identifier) are not installed")
            }
            let batch = CommandLine.arguments.dropFirst(2).first == "batch"
            var session: Session? = try await Session.start(locale: locale, batch: batch)
            emit(["type": "ready", "locale": locale.identifier])
            // Blocking stdin reads run outside Swift's cooperative executor.
            let (lines, continuation) = AsyncStream<String>.makeStream()
            let reader = Task.detached {
                while let line = readLine() { continuation.yield(line) }
                continuation.finish()
            }
            defer { reader.cancel() }
            do {
                for await line in lines {
                    guard let json = line.data(using: .utf8) else { throw StreamError("Invalid input encoding") }
                    let message = try JSONDecoder().decode(InputMessage.self, from: json)
                    switch message.type {
                    case "audio":
                        guard let encoded = message.pcm, let data = Data(base64Encoded: encoded),
                              let rate = message.sample_rate else { throw StreamError("Missing PCM or sample rate") }
                        if session == nil { session = try await Session.start(locale: locale, batch: batch) }
                        try session!.push(data, rate: rate)
                        emit(["type": "accepted"])
                    case "finish":
                        if let active = session { try await active.finish() }
                        else { emit(["type": "final", "text": ""]) }
                        session = nil
                        session = try await Session.start(locale: locale, batch: batch)
                    case "cancel":
                        let started = ProcessInfo.processInfo.systemUptime
                        // End input, flush converter, finish analyzer and await results closure.
                        // Avoid leaving a cancelled results task retaining the previous module.
                        if let active = session { try await active.finish(reportFinal: false) }
                        session = nil
                        emit(["type": "status", "text": String(format: "Live session drained in %.3fs", ProcessInfo.processInfo.systemUptime - started)])
                        session = try await Session.start(locale: locale, batch: batch)
                        emit(["type": "cancelled"])
                    default: throw StreamError("Unknown input message: \(message.type)")
                    }
                }
                if let active = session { await active.cancel() }
            } catch {
                if let active = session { await active.cancel() }
                throw error
            }
        } catch {
            emit(["type": "error", "text": String(describing: error)])
            exit(1)
        }
    }
}
