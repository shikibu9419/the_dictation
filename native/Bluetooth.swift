import Foundation
import CoreBluetooth
import AppKit

let serviceID = CBUUID(string: "607b5c9b-3700-4e94-f44a-2df900bcb0c3")
let bridgeQueue = DispatchQueue(label: "pebble-index.bluetooth")
let outputLock = NSLock()
func bluetoothError(_ error: Error) -> String {
    let native = error as NSError
    if native.domain == CBErrorDomain && native.code == CBError.peerRemovedPairingInformation.rawValue {
        return "Peer removed pairing information (\(native.domain) code=\(native.code))"
    }
    return "\(error.localizedDescription) (\(native.domain) code=\(native.code))"
}

// Pure advertisement filter; no CoreBluetooth or UI calls. A continuously
// repeated advertisement is not a fresh user action or a reason to reconnect.
struct AdvertisementGate {
    var baseline: String?
    var lastSeen: TimeInterval
    var lastAttempt: TimeInterval
    var retryInterval: TimeInterval
    var pendingReason: String?

    init(baseline: String?, now: TimeInterval, retryInterval: TimeInterval) {
        self.baseline = baseline
        self.lastSeen = now
        self.lastAttempt = now
        self.retryInterval = retryInterval
    }
    mutating func observe(_ signature: String?, now: TimeInterval) -> String? {
        if now - lastSeen >= 2 { pendingReason = "advertisement_reappeared" }
        lastSeen = now
        if let signature, signature != baseline {
            pendingReason = baseline == nil ? "first_advertisement" : "advertisement_changed"
        }
        if pendingReason == nil && retryInterval > 0 && now - lastAttempt >= retryInterval {
            pendingReason = "recovery_probe"
        }
        return pendingReason
    }
    mutating func accepted(_ signature: String?, now: TimeInterval) {
        baseline = signature
        lastAttempt = now
        pendingReason = nil
    }
}

// "ready" describes the IPC bridge. Radio availability may change while the
// same bridge stays alive, including starting the application with power off.
struct RadioReadiness {
    private var announced = false
    private var available = false
    mutating func update(_ state: CBManagerState) -> (announce: Bool, available: Bool, resumed: Bool) {
        let next = state == .poweredOn
        let result = (!announced, next, announced && !available && next)
        announced = true
        available = next
        return result
    }
    static func waitsForRadio(operation: String, watch: Bool, state: CBManagerState) -> Bool {
        operation == "find" && watch && state != .poweredOn
    }
}

func activitySignature(_ manufacturer: Data?) -> String? {
    guard let data = manufacturer, data.count == 8 else { return nil }
    let b = [UInt8](data.dropFirst(2))
    let fingerprint = UInt32(b[0]) | UInt32(b[1]) << 8 | UInt32(b[2]) << 16 | UInt32(b[3]) << 24
    return String(format: "%08x:%u:%u:%u", fingerprint, b[4], b[5] & 32 == 0 ? 0 : 1, b[5] & 128 == 0 ? 0 : 1)
}
func normalizedUUID(_ uuid: CBUUID) -> String {
    let value = uuid.uuidString.lowercased()
    if value.count == 4 { return "0000\(value)-0000-1000-8000-00805f9b34fb" }
    if value.count == 8 { return "\(value)-0000-1000-8000-00805f9b34fb" }
    return value
}
func emit(_ value: [String: Any]) {
    outputLock.lock(); defer { outputLock.unlock() }
    do {
        let data = try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
        FileHandle.standardOutput.write(data + Data([10]))
    } catch {
        let failure = ["type": "error", "text": "Bluetooth JSON encoding failed: \(error)"]
        if let data = try? JSONSerialization.data(withJSONObject: failure) {
            FileHandle.standardOutput.write(data + Data([10]))
        }
    }
}

final class Bluetooth: NSObject, CBCentralManagerDelegate, CBPeripheralDelegate {
    var central: CBCentralManager!
    var devices: [UUID: CBPeripheral] = [:]
    var advertisements: [UUID: [String: Any]] = [:]
    var current: CBPeripheral?
    var characteristics: [String: CBCharacteristic] = [:]
    var pending: [String: Any]?
    var timer: DispatchSourceTimer?
    var serviceCount = 0
    var radio = RadioReadiness()
    var watchAddress: String?
    var advertisementGate: AdvertisementGate?
    var workspaceObservers: [NSObjectProtocol] = []
    var resumeRequested: String?
    override init() {
        super.init()
        central = CBCentralManager(delegate: self, queue: bridgeQueue)
        for name in [NSWorkspace.didWakeNotification, NSWorkspace.sessionDidBecomeActiveNotification] {
            workspaceObservers.append(NSWorkspace.shared.notificationCenter.addObserver(forName: name, object: nil, queue: nil) { [weak self] _ in
                bridgeQueue.async { self?.resumeHint("mac_resumed") }
            })
        }
    }
    func resumeHint(_ reason: String) {
        if current?.state != .connected { resumeRequested = reason }
        guard central.state == .poweredOn else { return }
        guard operation == "find", pending?["watch"] as? Bool == true,
              let address = pending?["address"] as? String else { return }
        resumeRequested = nil
        central.stopScan()
        advertisementGate = nil
        reply(["address": address, "reconnect_reason": reason])
    }
    func reply(_ value: Any = NSNull(), error: String? = nil) {
        guard let request = pending else { return }
        timer?.cancel(); timer = nil; pending = nil
        var result: [String: Any] = ["type": "reply", "id": request["id"] ?? NSNull(), "value": value]
        if let error { result["error"] = error }
        emit(result)
    }
    var operation: String { pending?["type"] as? String ?? "" }
    var radioError: String {
        "Bluetooth is unavailable (state \(central.state.rawValue)). Check Bluetooth power and permission."
    }
    func centralManagerDidUpdateState(_ central: CBCentralManager) {
        let change = radio.update(central.state)
        if change.announce { emit(["type": "ready", "available": change.available, "state": central.state.rawValue]) }
        emit(["type": "status", "available": change.available, "state": central.state.rawValue,
              "text": change.available ? "Bluetooth available" : radioError])
        if change.available {
            if change.resumed { resumeHint("bluetooth_resumed") }
        } else if pending != nil && !RadioReadiness.waitsForRadio(
            operation: operation, watch: pending?["watch"] as? Bool == true, state: central.state) {
            reply(error: radioError)
        }
    }
    func description(_ peripheral: CBPeripheral) -> [String: Any] {
        var result = advertisements[peripheral.identifier] ?? [:]
        result["address"] = peripheral.identifier.uuidString
        if result["name"] == nil { result["name"] = peripheral.name ?? NSNull() as Any }
        return result
    }
    func handle(_ request: [String: Any]) {
        guard pending == nil else {
            emit(["type": "reply", "id": request["id"] ?? NSNull(), "error": "A Bluetooth command is already active"]); return
        }
        pending = request
        let timeout = request["timeout"] as? Double ?? 5
        let id = request["id"] as? Int
        // asyncAfter coalesces long delays; its scan reply can miss the IPC deadline.
        let expiry = DispatchSource.makeTimerSource(flags: .strict, queue: bridgeQueue)
        expiry.schedule(deadline: .now() + timeout, leeway: .milliseconds(10))
        expiry.setEventHandler { [weak self] in
            guard let self, self.pending?["id"] as? Int == id else { return }
            if self.operation == "scan" || self.operation == "find" {
                // Keep observing between watch replies; restarting discovery
                // on every timeout would create artificial advertisement gaps.
                if self.pending?["watch"] as? Bool != true { self.central.stopScan() }
                if self.operation == "scan" { self.reply(self.advertisements.keys.compactMap { self.devices[$0].map(self.description) }) }
                else { self.reply() }
            } else {
                if self.operation == "connect", let peripheral = self.current { self.central.cancelPeripheralConnection(peripheral) }
                self.reply(error: "Timeout: \(self.operation)")
            }
        }
        timer = expiry
        expiry.resume()
        if central.state != .poweredOn {
            if operation == "disconnect" {
                current = nil
                characteristics.removeAll()
                reply()
                return
            }
            if !RadioReadiness.waitsForRadio(operation: operation, watch: request["watch"] as? Bool == true, state: central.state) {
                reply(error: radioError)
                return
            }
        }
        switch operation {
        case "cached":
            guard let value = request["address"] as? String, let uuid = UUID(uuidString: value) else { reply(); return }
            if let peripheral = central.retrievePeripherals(withIdentifiers: [uuid]).first {
                devices[uuid] = peripheral; reply(description(peripheral))
            } else { reply() }
        case "scan", "find":
            if request["watch"] as? Bool == true {
                if let reason = resumeRequested, let address = request["address"] as? String,
                   central.state == .poweredOn {
                    resumeRequested = nil
                    reply(["address": address, "reconnect_reason": reason])
                    return
                }
                let address = (request["address"] as? String)?.lowercased()
                if advertisementGate == nil || watchAddress != address || !central.isScanning {
                    advertisementGate = AdvertisementGate(baseline: request["baseline"] as? String,
                        now: ProcessInfo.processInfo.systemUptime,
                        retryInterval: (request["retry_interval_ms"] as? Double ?? 0) / 1000)
                }
                advertisementGate?.retryInterval = (request["retry_interval_ms"] as? Double ?? 0) / 1000
                watchAddress = address
            } else {
                advertisementGate = nil
                watchAddress = nil
                advertisements.removeAll()
            }
            if !central.isScanning && central.state == .poweredOn {
                central.scanForPeripherals(withServices: [serviceID], options: [CBCentralManagerScanOptionAllowDuplicatesKey: true])
            }
        case "connect":
            central.stopScan()
            resumeRequested = nil
            advertisementGate = nil
            watchAddress = nil
            guard let value = request["address"] as? String, let uuid = UUID(uuidString: value),
                  let peripheral = devices[uuid] ?? central.retrievePeripherals(withIdentifiers: [uuid]).first else {
                reply(error: "Unknown peripheral"); return
            }
            current = peripheral; devices[uuid] = peripheral; peripheral.delegate = self
            characteristics.removeAll(); central.connect(peripheral)
        case "disconnect":
            if let peripheral = current, peripheral.state != .disconnected { central.cancelPeripheralConnection(peripheral) }
            else { current = nil; reply() }
        case "notify", "write":
            guard let peripheral = current, peripheral.state == .connected,
                  let uuid = request["uuid"] as? String, let characteristic = characteristics[uuid.lowercased()] else {
                reply(error: "Peripheral disconnected or characteristic missing"); return
            }
            if operation == "notify" { peripheral.setNotifyValue(request["enabled"] as? Bool ?? true, for: characteristic) }
            else {
                guard let encoded = request["data"] as? String, let data = Data(base64Encoded: encoded) else { reply(error: "Invalid write data"); return }
                let response = request["response"] as? Bool ?? false
                peripheral.writeValue(data, for: characteristic, type: response ? .withResponse : .withoutResponse)
                if !response { reply() }
            }
        case "inspect":
            guard let peripheral = current else { reply(error: "Not connected"); return }
            reply((peripheral.services ?? []).map { service in
                ["uuid": normalizedUUID(service.uuid), "characteristics": (service.characteristics ?? []).map { char in
                    ["uuid": normalizedUUID(char.uuid),
                     "handle": char.responds(to: NSSelectorFromString("handle")) ? (char.value(forKey: "handle") ?? NSNull()) : NSNull(),
                     "properties": properties(char.properties)] as [String: Any]
                }] as [String: Any]
            })
        default: reply(error: "Unknown Bluetooth command")
        }
    }
    func properties(_ p: CBCharacteristicProperties) -> [String] {
        let mapping: [(CBCharacteristicProperties, String)] = [(.broadcast,"broadcast"),(.read,"read"),(.writeWithoutResponse,"write-without-response"),(.write,"write"),(.notify,"notify"),(.indicate,"indicate"),(.authenticatedSignedWrites,"authenticated-signed-writes"),(.extendedProperties,"extended-properties")]
        return mapping.filter { p.contains($0.0) }.map { $0.1 }
    }
    func centralManager(_ central: CBCentralManager, didDiscover peripheral: CBPeripheral, advertisementData: [String: Any], rssi RSSI: NSNumber) {
        devices[peripheral.identifier] = peripheral
        var info: [String: Any] = ["rssi": RSSI, "name": advertisementData[CBAdvertisementDataLocalNameKey] ?? peripheral.name ?? NSNull() as Any, "manufacturer_data": [:]]
        if let bytes = advertisementData[CBAdvertisementDataManufacturerDataKey] as? Data, bytes.count >= 2 {
            let vendor = UInt16(bytes[bytes.startIndex]) | UInt16(bytes[bytes.startIndex + 1]) << 8
            info["manufacturer_data"] = [String(vendor): bytes.dropFirst(2).map { String(format: "%02x", $0) }.joined()]
        }
        advertisements[peripheral.identifier] = info
        let signature = activitySignature(advertisementData[CBAdvertisementDataManufacturerDataKey] as? Data)
        var watchReason: String?
        if watchAddress == peripheral.identifier.uuidString.lowercased() {
            watchReason = advertisementGate?.observe(signature, now: ProcessInfo.processInfo.systemUptime)
        }
        if operation == "scan", pending?["first"] as? Bool == true {
            central.stopScan()
            reply([description(peripheral)])
        }
        if operation == "find", let address = pending?["address"] as? String,
           peripheral.identifier.uuidString.lowercased() == address.lowercased() {
            if pending?["watch"] as? Bool == true && watchReason == nil { return }
            var result = description(peripheral)
            if let watchReason { result["reconnect_reason"] = watchReason }
            advertisementGate?.accepted(signature, now: ProcessInfo.processInfo.systemUptime)
            central.stopScan()
            reply(result)
        }
    }
    func centralManager(_ central: CBCentralManager, didConnect peripheral: CBPeripheral) {
        guard operation == "connect", peripheral === current else { central.cancelPeripheralConnection(peripheral); return }
        resumeRequested = nil
        peripheral.discoverServices(nil)
    }
    func peripheral(_ peripheral: CBPeripheral, didDiscoverServices error: Error?) {
        guard operation == "connect" else { return }
        if let error { reply(error: bluetoothError(error)); return }
        serviceCount = peripheral.services?.count ?? 0
        if serviceCount == 0 { reply(error: "Peripheral has no services"); return }
        for service in peripheral.services ?? [] { peripheral.discoverCharacteristics(nil, for: service) }
    }
    func peripheral(_ peripheral: CBPeripheral, didDiscoverCharacteristicsFor service: CBService, error: Error?) {
        guard operation == "connect" else { return }
        if let error { reply(error: bluetoothError(error)); return }
        for characteristic in service.characteristics ?? [] { characteristics[characteristic.uuid.uuidString.lowercased()] = characteristic }
        serviceCount -= 1
        if serviceCount == 0 { reply(description(peripheral)) }
    }
    func centralManager(_ central: CBCentralManager, didFailToConnect peripheral: CBPeripheral, error: Error?) {
        if operation == "connect" { reply(error: error.map(bluetoothError) ?? "Connection failed") }
    }
    func centralManager(_ central: CBCentralManager, didDisconnectPeripheral peripheral: CBPeripheral, error: Error?) {
        let native = error as NSError?
        emit(["type": "disconnected", "address": peripheral.identifier.uuidString,
              "requested": operation == "disconnect", "operation": operation,
              "error_domain": native?.domain ?? "", "error_code": native?.code ?? 0,
              "reason": error.map(bluetoothError) ?? "No error supplied by CoreBluetooth"])
        if operation == "disconnect" { current = nil; reply() }
        else if pending != nil && operation != "scan" && operation != "find" && operation != "cached" {
            reply(error: error.map(bluetoothError) ?? "Ring disconnected")
        }
    }
    func peripheral(_ peripheral: CBPeripheral, didUpdateNotificationStateFor characteristic: CBCharacteristic, error: Error?) {
        if operation == "notify", (pending?["uuid"] as? String)?.lowercased() == characteristic.uuid.uuidString.lowercased() { reply(error: error.map(bluetoothError)) }
    }
    func peripheral(_ peripheral: CBPeripheral, didWriteValueFor characteristic: CBCharacteristic, error: Error?) {
        if operation == "write" { reply(error: error.map(bluetoothError)) }
    }
    func peripheral(_ peripheral: CBPeripheral, didUpdateValueFor characteristic: CBCharacteristic, error: Error?) {
        if let error { emit(["type": "error", "text": bluetoothError(error)]); return }
        if let data = characteristic.value { emit(["type": "notification", "uuid": characteristic.uuid.uuidString.lowercased(), "data": data.base64EncodedString()]) }
    }
}

#if !TEST_BLUETOOTH_POLICY
@main struct BridgeMain {
    static func main() {
        let bluetooth = Bluetooth()
        DispatchQueue.global().async {
            while let line = readLine() {
                guard let data = line.data(using: .utf8), let request = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { continue }
                bridgeQueue.async { bluetooth.handle(request) }
            }
            bridgeQueue.async {
                if bluetooth.central.state == .poweredOn {
                    bluetooth.central.stopScan()
                    if let peripheral = bluetooth.current { bluetooth.central.cancelPeripheralConnection(peripheral) }
                }
                bridgeQueue.asyncAfter(deadline: .now() + 0.2) { exit(0) }
            }
        }
        dispatchMain()
    }
}
#endif
