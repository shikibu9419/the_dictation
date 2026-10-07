import Foundation
import CoreBluetooth

let serviceID = CBUUID(string: "607b5c9b-3700-4e94-f44a-2df900bcb0c3")
let bridgeQueue = DispatchQueue(label: "pebble-index.bluetooth")
let outputLock = NSLock()
func bluetoothError(_ error: Error) -> String {
    let native = error as NSError
    if native.domain == CBErrorDomain && native.code == CBError.peerRemovedPairingInformation.rawValue {
        return "Peer removed pairing information"
    }
    return error.localizedDescription
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
    var ready = false
    override init() {
        super.init()
        central = CBCentralManager(delegate: self, queue: bridgeQueue)
    }
    func reply(_ value: Any = NSNull(), error: String? = nil) {
        guard let request = pending else { return }
        timer?.cancel(); timer = nil; pending = nil
        var result: [String: Any] = ["type": "reply", "id": request["id"] ?? NSNull(), "value": value]
        if let error { result["error"] = error }
        emit(result)
    }
    var operation: String { pending?["type"] as? String ?? "" }
    func centralManagerDidUpdateState(_ central: CBCentralManager) {
        if central.state == .poweredOn {
            if !ready { ready = true; emit(["type": "ready"]) }
        } else if central.state != .unknown && central.state != .resetting {
            emit(["type": "error", "text": "Bluetooth is unavailable (state \(central.state.rawValue)). Check Bluetooth power and permission."])
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
                self.central.stopScan()
                if self.operation == "scan" { self.reply(self.advertisements.keys.compactMap { self.devices[$0].map(self.description) }) }
                else { self.reply() }
            } else {
                if self.operation == "connect", let peripheral = self.current { self.central.cancelPeripheralConnection(peripheral) }
                self.reply(error: "Timeout: \(self.operation)")
            }
        }
        timer = expiry
        expiry.resume()
        switch operation {
        case "cached":
            guard let value = request["address"] as? String, let uuid = UUID(uuidString: value) else { reply(); return }
            if let peripheral = central.retrievePeripherals(withIdentifiers: [uuid]).first {
                devices[uuid] = peripheral; reply(description(peripheral))
            } else { reply() }
        case "scan", "find":
            advertisements.removeAll()
            central.scanForPeripherals(withServices: [serviceID], options: [CBCentralManagerScanOptionAllowDuplicatesKey: true])
        case "connect":
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
        if operation == "scan", pending?["first"] as? Bool == true {
            central.stopScan()
            reply([description(peripheral)])
        }
        if operation == "find", let address = pending?["address"] as? String,
           peripheral.identifier.uuidString.lowercased() == address.lowercased() { central.stopScan(); reply(description(peripheral)) }
    }
    func centralManager(_ central: CBCentralManager, didConnect peripheral: CBPeripheral) {
        guard operation == "connect", peripheral === current else { central.cancelPeripheralConnection(peripheral); return }
        peripheral.discoverServices(nil)
    }
    func peripheral(_ peripheral: CBPeripheral, didDiscoverServices error: Error?) {
        guard operation == "connect" else { return }
        if let error { reply(error: error.localizedDescription); return }
        serviceCount = peripheral.services?.count ?? 0
        if serviceCount == 0 { reply(error: "Peripheral has no services"); return }
        for service in peripheral.services ?? [] { peripheral.discoverCharacteristics(nil, for: service) }
    }
    func peripheral(_ peripheral: CBPeripheral, didDiscoverCharacteristicsFor service: CBService, error: Error?) {
        guard operation == "connect" else { return }
        if let error { reply(error: error.localizedDescription); return }
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
        if operation == "notify", (pending?["uuid"] as? String)?.lowercased() == characteristic.uuid.uuidString.lowercased() { reply(error: error?.localizedDescription) }
    }
    func peripheral(_ peripheral: CBPeripheral, didWriteValueFor characteristic: CBCharacteristic, error: Error?) {
        if operation == "write" { reply(error: error?.localizedDescription) }
    }
    func peripheral(_ peripheral: CBPeripheral, didUpdateValueFor characteristic: CBCharacteristic, error: Error?) {
        if let error { emit(["type": "error", "text": error.localizedDescription]); return }
        if let data = characteristic.value { emit(["type": "notification", "uuid": characteristic.uuid.uuidString.lowercased(), "data": data.base64EncodedString()]) }
    }
}

@main struct BridgeMain {
    static func main() {
        let bluetooth = Bluetooth()
        DispatchQueue.global().async {
            while let line = readLine() {
                guard let data = line.data(using: .utf8), let request = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { continue }
                bridgeQueue.async { bluetooth.handle(request) }
            }
            bridgeQueue.async {
                bluetooth.central.stopScan()
                if let peripheral = bluetooth.current { bluetooth.central.cancelPeripheralConnection(peripheral) }
                bridgeQueue.asyncAfter(deadline: .now() + 0.2) { exit(0) }
            }
        }
        dispatchMain()
    }
}
