import Foundation

// Build with -D TEST_BLUETOOTH_POLICY and native/Bluetooth.swift. This main
// never constructs Bluetooth, CBCentralManager, NSWorkspace, or a window.
@main struct BluetoothPolicyTests {
    static func main() {
        let baseline = "00000001:2:0:0"
        var idle = AdvertisementGate(baseline: baseline, now: 0, retryInterval: 0)
        for ms in stride(from: 100, through: 120_000, by: 100) {
            precondition(idle.observe(baseline, now: Double(ms) / 1000) == nil)
        }
        precondition(idle.observe(baseline, now: 123) == "advertisement_reappeared")
        idle.accepted(baseline, now: 123)
        precondition(idle.observe(baseline, now: 123.1) == nil)
        precondition(idle.observe("00000001:3:0:0", now: 123.2) == "advertisement_changed")
        // A change seen between two watch commands remains pending.
        precondition(idle.observe("00000001:3:0:0", now: 123.3) == "advertisement_changed")
        idle.accepted("00000001:3:0:0", now: 123.3)
        precondition(idle.observe("00000001:3:0:0", now: 123.4) == nil)

        var failure = AdvertisementGate(baseline: baseline, now: 0, retryInterval: 30)
        for ms in stride(from: 100, to: 30_000, by: 100) {
            precondition(failure.observe(baseline, now: Double(ms) / 1000) == nil)
        }
        precondition(failure.observe(baseline, now: 30) == "recovery_probe")
        failure.accepted(baseline, now: 30)
        precondition(failure.observe(baseline, now: 30.1) == nil)
        precondition(failure.observe("00000001:2:0:1", now: 30.2) == "advertisement_changed")

        var first = AdvertisementGate(baseline: nil, now: 0, retryInterval: 30)
        precondition(first.observe(baseline, now: 0.01) == "first_advertisement")
        precondition(activitySignature(Data([0xea, 0x0e, 1, 0, 0, 0, 2, 0])) == baseline)
        precondition(activitySignature(Data([0xea, 0x0e, 1, 0, 0, 0, 2, 0xa0])) == "00000001:2:1:1")
        precondition(activitySignature(Data([1, 2])) == nil)
        precondition(activitySignature(nil) == nil)
        precondition(bluetoothError(NSError(domain: "SyntheticBluetooth", code: 9)).contains("SyntheticBluetooth code=9"))
        print("Bluetooth advertisement policy: 6 headless scenarios passed")
    }
}
