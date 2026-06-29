import Foundation

/// Shared metric formatting. The design is metric throughout (km / m), so distances are forced
/// to kilometers rather than going through `.road` usage, which localizes to miles.
enum VoopFormat {
    /// Adaptive distance: metres under 1 km, kilometres above (one decimal).
    static func distance(_ meters: Double) -> String {
        if meters < 1000 {
            return "\(Int(meters.rounded())) m"
        }
        return "\(kilometers(meters)) km"
    }

    /// Kilometre number only (no unit). Drops the decimal at/above 100 km.
    static func kilometers(_ meters: Double, fractionDigits: Int? = nil) -> String {
        let km = meters / 1000
        let digits = fractionDigits ?? (km >= 100 ? 0 : 1)
        return km.formatted(.number.precision(.fractionLength(digits)))
    }
}
