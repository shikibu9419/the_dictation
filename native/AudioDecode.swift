import Foundation
import AVFoundation

struct DecodeError: Error { let message: String }
@main struct AudioDecode {
    static func main() {
        do {
            guard CommandLine.arguments.count == 2 else { throw DecodeError(message: "Missing audio path") }
            let file = try AVAudioFile(forReading: URL(fileURLWithPath: CommandLine.arguments[1]))
            let source = file.processingFormat
            guard let target = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: 16000, channels: 1, interleaved: true),
                  let converter = AVAudioConverter(from: source, to: target),
                  let input = AVAudioPCMBuffer(pcmFormat: source, frameCapacity: 8192),
                  let output = AVAudioPCMBuffer(pcmFormat: target, frameCapacity: 8192) else {
                throw DecodeError(message: "Unsupported audio format")
            }
            var pcm = Data()
            var readError: Error?
            while true {
                var conversionError: NSError?
                let status = converter.convert(to: output, error: &conversionError) { count, inputStatus in
                    do {
                        let remaining = file.length - file.framePosition
                        if remaining <= 0 { inputStatus.pointee = .endOfStream; return nil }
                        try file.read(into: input, frameCount: min(count, input.frameCapacity, AVAudioFrameCount(min(remaining, Int64(UInt32.max)))))
                        inputStatus.pointee = input.frameLength > 0 ? .haveData : .endOfStream
                        return input.frameLength > 0 ? input : nil
                    } catch {
                        readError = error; inputStatus.pointee = .endOfStream; return nil
                    }
                }
                if let readError { throw readError }
                if let conversionError { throw conversionError }
                if output.frameLength > 0, let samples = output.int16ChannelData?[0] {
                    pcm.append(Data(bytes: samples, count: Int(output.frameLength) * 2))
                }
                if status == .endOfStream { break }
                if status == .error { throw DecodeError(message: "Audio conversion failed") }
            }
            guard !pcm.isEmpty else { throw DecodeError(message: "No audio samples in uploaded file") }
            let data = try JSONSerialization.data(withJSONObject: ["type": "audio", "rate": 16000, "pcm": pcm.base64EncodedString()])
            FileHandle.standardOutput.write(data + Data([10]))
        } catch {
            let data = try! JSONSerialization.data(withJSONObject: ["type": "error", "text": String(describing: error)])
            FileHandle.standardOutput.write(data + Data([10])); exit(1)
        }
    }
}
