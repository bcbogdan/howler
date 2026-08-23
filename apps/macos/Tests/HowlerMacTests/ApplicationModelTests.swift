import XCTest
@testable import HowlerMac

final class ApplicationModelTests: XCTestCase {
    func testApplicationJSONContractsDecode() throws {
        let snapshot = try JSONDecoder().decode(EditorSnapshot.self, from: Data(#"{"revision":2,"source":"a😀","selections":[{"anchor":5,"head":5,"affinity":"Downstream","revision":2}],"can_undo":true,"can_redo":false}"#.utf8))
        XCTAssertEqual(snapshot.source, "a😀")
        XCTAssertEqual(snapshot.selections.first?.anchor, 5)

        let results = try JSONDecoder().decode([SearchResult].self, from: Data(#"[{"note":{"id":{"kind":"provisional","value":"id"},"relative_path":"note.md","title":"Note","content_hash":"hash"},"snippet":"body","reason":"fuzzy_title"}]"#.utf8))
        XCTAssertEqual(results.first?.note.id.value, "id")
        XCTAssertEqual(results.first?.reason, "fuzzy_title")
    }

    func testPostEditCaretUsesUTF8Bytes() {
        let source = "ab"
        let nativeRange = NSRange(location: 1, length: 0)
        let bytes = UTF8Range.byteRange(nativeRange, in: source)!
        let caret = bytes.lowerBound + "😀".utf8.count
        XCTAssertEqual(caret, 5)
    }
}
