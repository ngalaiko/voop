import Foundation
import Observation

@Observable
final class AppSettings {
    var rimBsdMillimeters: Int {
        didSet { UserDefaults.standard.set(rimBsdMillimeters, forKey: "rimBsdMillimeters") }
    }

    var tireWidthMillimeters: Int {
        didSet { UserDefaults.standard.set(tireWidthMillimeters, forKey: "tireWidthMillimeters") }
    }

    var chainringTeeth: Int {
        didSet { UserDefaults.standard.set(chainringTeeth, forKey: "chainringTeeth") }
    }

    var cogTeeth: Int {
        didSet { UserDefaults.standard.set(cogTeeth, forKey: "cogTeeth") }
    }

    var minCadenceRpm: Int {
        didSet { UserDefaults.standard.set(minCadenceRpm, forKey: "minCadenceRpm") }
    }

    var minDistanceMeters: Int {
        didSet { UserDefaults.standard.set(minDistanceMeters, forKey: "minDistanceMeters") }
    }

    var gapThresholdSeconds: Int {
        didSet { UserDefaults.standard.set(gapThresholdSeconds, forKey: "gapThresholdSeconds") }
    }

    /// Rolling circumference from rim bead-seat diameter + tire, treating the wheel as a
    /// circle of diameter BSD + 2×tire width.
    var wheelCircumferenceMeters: Double {
        Double.pi * Double(rimBsdMillimeters + 2 * tireWidthMillimeters) / 1000.0
    }

    /// Chainring teeth ÷ cog teeth — how far the wheel turns per crank revolution.
    var gearRatio: Double {
        Double(chainringTeeth) / Double(cogTeeth)
    }

    /// A pause longer than this ends a ride and clears the live card.
    var gapThreshold: TimeInterval {
        TimeInterval(gapThresholdSeconds)
    }

    static let rimPresets: [(label: String, bsd: Int)] = [
        ("700c", 622),
        ("650b", 584),
        ("26\"", 559),
    ]

    struct TirePreset {
        let label: String
        let bsd: Int
        let width: Int
    }

    static let tirePresets: [TirePreset] = [
        TirePreset(label: "700×25c", bsd: 622, width: 25),
        TirePreset(label: "700×28c", bsd: 622, width: 28),
        TirePreset(label: "700×32c", bsd: 622, width: 32),
        TirePreset(label: "650×47b", bsd: 584, width: 47),
    ]

    struct GearPreset {
        let label: String
        let chainring: Int
        let cog: Int
    }

    static let gearPresets: [GearPreset] = [
        GearPreset(label: "46×16", chainring: 46, cog: 16),
        GearPreset(label: "48×17", chainring: 48, cog: 17),
        GearPreset(label: "44×16", chainring: 44, cog: 16),
        GearPreset(label: "46×18", chainring: 46, cog: 18),
    ]

    init() {
        let bsd = UserDefaults.standard.integer(forKey: "rimBsdMillimeters")
        rimBsdMillimeters = bsd > 0 ? bsd : 622
        let width = UserDefaults.standard.integer(forKey: "tireWidthMillimeters")
        tireWidthMillimeters = width > 0 ? width : 25
        let chain = UserDefaults.standard.integer(forKey: "chainringTeeth")
        chainringTeeth = chain > 0 ? chain : 46
        let cog = UserDefaults.standard.integer(forKey: "cogTeeth")
        cogTeeth = cog > 0 ? cog : 16
        let m = UserDefaults.standard.integer(forKey: "minCadenceRpm")
        minCadenceRpm = m > 0 ? m : 20
        // 0 ("record everything") is a legal stored value here — the Stepper allows it — so
        // "unset" must be detected via object(forKey:), not the 0 that integer(forKey:)
        // returns for both. The other settings' ranges exclude their sentinel.
        minDistanceMeters = UserDefaults.standard.object(forKey: "minDistanceMeters") == nil
            ? 500
            : UserDefaults.standard.integer(forKey: "minDistanceMeters")
        let gap = UserDefaults.standard.integer(forKey: "gapThresholdSeconds")
        gapThresholdSeconds = gap > 0 ? gap : 5 * 60
    }
}
