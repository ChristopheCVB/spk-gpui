import AppKit
import AVFoundation

let title = CommandLine.arguments[1]
let ready = URL(fileURLWithPath: CommandLine.arguments[2])
let stop = CommandLine.arguments[3]

let app = NSApplication.shared
app.setActivationPolicy(.regular)
let window = NSWindow(
    contentRect: NSRect(x: 100, y: 100, width: 600, height: 400),
    styleMask: [.titled, .closable],
    backing: .buffered,
    defer: false
)
window.title = title
window.backgroundColor = .systemBlue
window.makeKeyAndOrderFront(nil)
app.activate()

// Play from the window's process so the window filter can capture the tone.
let engine = AVAudioEngine()
let player = AVAudioPlayerNode()
let format = AVAudioFormat(standardFormatWithSampleRate: 48_000, channels: 2)!
let tone = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 48_000)!
tone.frameLength = tone.frameCapacity
for frame in 0..<Int(tone.frameLength) {
    tone.floatChannelData![0][frame] = Float(0.2 * sin(Double(frame) * 2 * .pi * 440 / 48_000))
    tone.floatChannelData![1][frame] = Float(0.2 * sin(Double(frame) * 2 * .pi * 660 / 48_000))
}
engine.attach(player)
engine.connect(player, to: engine.mainMixerNode, format: format)
try engine.start()
player.scheduleBuffer(tone, at: nil, options: .loops)
player.play()

Timer.scheduledTimer(withTimeInterval: 0.04, repeats: true) { timer in
    if FileManager.default.fileExists(atPath: stop) {
        NSApplication.shared.terminate(nil)
        return
    }
    window.backgroundColor = window.backgroundColor == .systemBlue ? .systemPurple : .systemBlue
    if !FileManager.default.fileExists(atPath: ready.path) {
        try! Data().write(to: ready)
    }
}
app.run()
