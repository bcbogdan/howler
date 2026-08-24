import XCTest
@testable import HowlerMac

final class UTF8RangeTests: XCTestCase {
    func testUnicodeRoundTrip() {
        let source = "a😀e\u{301}אב"
        let original = NSRange(location: 1, length: 2)
        let bytes = UTF8Range.byteRange(original, in: source)
        XCTAssertEqual(bytes, 1..<5)
        XCTAssertEqual(UTF8Range.nsRange(bytes!, in: source), original)
    }

    func testReversedUnicodeSelectionNormalizesForAppKit() {
        let source = "a😀b"
        XCTAssertEqual(UTF8Range.nsRange(anchor: 5, head: 1, in: source), NSRange(location: 1, length: 2))
    }

    func testNativeRangePreservesKnownReversedDirection() {
        let previous = RustSelection(anchor: 5, head: 1, affinity: .upstream, revision: 2)
        let selection = UTF8Range.selection(1..<5, affinity: .upstream, previous: previous, revision: 3)
        XCTAssertEqual(selection, RustSelection(anchor: 5, head: 1, affinity: .upstream, revision: 3))
    }

    func testSelectionResizeRetainsStableAnchorDirection() {
        let forward = RustSelection(anchor: 1, head: 5, affinity: .downstream, revision: 2)
        XCTAssertEqual(
            UTF8Range.selection(1..<7, affinity: .downstream, previous: forward, revision: 2),
            RustSelection(anchor: 1, head: 7, affinity: .downstream, revision: 2)
        )

        let reversed = RustSelection(anchor: 5, head: 1, affinity: .upstream, revision: 2)
        XCTAssertEqual(
            UTF8Range.selection(0..<5, affinity: .upstream, previous: reversed, revision: 2),
            RustSelection(anchor: 5, head: 0, affinity: .upstream, revision: 2)
        )
    }

    func testEqualRevisionNoteTransitionRequiresAuthoritativeSelection() {
        let first = EditorSelectionPresentationContext(
            noteID: RustIdentity(kind: .provisional, value: "first"),
            generation: 4,
            revision: 2,
            presentsPendingDraft: false
        )
        let second = EditorSelectionPresentationContext(
            noteID: RustIdentity(kind: .provisional, value: "second"),
            generation: 5,
            revision: 2,
            presentsPendingDraft: false
        )
        let nativeSelection = RustSelection(anchor: 8, head: 8, affinity: .downstream, revision: 2)
        var tracker = NativeSelectionTracker()
        tracker.record(nativeSelection, in: first)

        XCTAssertFalse(tracker.shouldApplyAuthoritativeSelection(in: first))
        XCTAssertEqual(tracker.selection(in: first), nativeSelection)
        XCTAssertTrue(tracker.shouldApplyAuthoritativeSelection(in: second))
        XCTAssertNil(tracker.selection(in: second))
    }

    func testPendingDraftResolutionAtEqualRevisionRequiresAuthoritativeSelection() {
        let owner = RustIdentity(kind: .provisional, value: "note")
        let editor = EditorSelectionPresentationContext(
            noteID: owner,
            generation: 7,
            revision: 3,
            presentsPendingDraft: false
        )
        let pending = EditorSelectionPresentationContext(
            noteID: owner,
            generation: 7,
            revision: 3,
            presentsPendingDraft: true
        )
        var tracker = NativeSelectionTracker()
        tracker.record(RustSelection(anchor: 4, head: 4, affinity: .downstream, revision: 3), in: editor)

        XCTAssertTrue(tracker.shouldApplyAuthoritativeSelection(in: pending))
        tracker.record(nil, in: pending)
        XCTAssertTrue(tracker.shouldApplyAuthoritativeSelection(in: editor))
        XCTAssertNil(tracker.selection(in: editor))
    }
}
