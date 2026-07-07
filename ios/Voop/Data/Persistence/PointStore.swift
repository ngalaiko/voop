import Foundation
import SwiftData

@MainActor
final class PointStore {
    private let container: ModelContainer

    private static let storeURL = URL.documentsDirectory.appending(path: "rawpoints.store")

    init(inMemory: Bool = false) throws {
        let config: ModelConfiguration = inMemory
            ? ModelConfiguration("rawpoints", isStoredInMemoryOnly: true)
            : ModelConfiguration("rawpoints", url: Self.storeURL)
        container = try ModelContainer(for: RawPoint.self, configurations: config)
    }

    /// Opens the on-disk store, recovering rather than crashing on an unopenable file: move the
    /// store (and its SQLite sidecars) aside and retry with a fresh one, then fall back to an
    /// in-memory store so the app still launches.
    ///
    /// Moved aside, never deleted: raw points are the app's *only* persisted data, and an open
    /// failure isn't necessarily corruption — a failed migration after a future `RawPoint`
    /// schema change, a transient lock, or a full disk land here too. A `.backup-<timestamp>`
    /// file can be recovered by hand; a deleted one is a silently erased ride history.
    static func openOrRecover() -> PointStore {
        if let store = try? PointStore() { return store }
        let backupPath = storeURL.path + ".backup-\(Int(Date.now.timeIntervalSince1970))"
        for suffix in ["", "-wal", "-shm"] {
            try? FileManager.default.moveItem(
                at: URL(fileURLWithPath: storeURL.path + suffix),
                to: URL(fileURLWithPath: backupPath + suffix)
            )
        }
        if let store = try? PointStore() { return store }
        if let store = try? PointStore(inMemory: true) { return store }
        // An in-memory container for this trivial model effectively cannot fail; if it somehow
        // does, there's no usable persistence layer left to launch with.
        fatalError("Unable to create even an in-memory point store")
    }

    /// Inserts a point into the context *without* saving. Returns the stored model so the caller
    /// can mirror it in memory without re-fetching. Call `save()` to flush a batch to disk —
    /// losing the last few unflushed seconds on a crash is acceptable for a fitness logger.
    @discardableResult
    func insert(_ point: DataPoint) -> RawPoint {
        let raw = RawPoint(from: point)
        container.mainContext.insert(raw)
        return raw
    }

    func save() throws {
        try container.mainContext.save()
    }

    func fetchAll() throws -> [RawPoint] {
        let descriptor = FetchDescriptor<RawPoint>(sortBy: [SortDescriptor(\.receivedAt)])
        return try container.mainContext.fetch(descriptor)
    }

    func delete(_ points: [RawPoint]) throws {
        for point in points {
            container.mainContext.delete(point)
        }
        try container.mainContext.save()
    }
}
