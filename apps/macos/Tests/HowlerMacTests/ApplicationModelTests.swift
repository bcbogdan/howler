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

    func testSessionResponseDecodesAuthoritativeStateAndIdentifiedEffect() throws {
        let data = Data(#"{"state":{"folder":{"path":"/notes","adopted":false,"generation":3},"active":null,"recoveries":[],"background_tasks":[]},"effects":[{"kind":"schedule_autosave","effect_id":"autosave-1","delay_ms":750,"target":{"note_id":{"kind":"provisional","value":"id"},"revision":2,"generation":3}}],"outcome":{"status":"applied","value":{"opened_note":null}}}"#.utf8)
        let response = try JSONDecoder().decode(ApplicationResponse<ConnectResult>.self, from: data)
        XCTAssertEqual(response.state.folder?.generation, 3)
        guard case let .scheduleAutosave(id, delay, target) = response.effects.first else {
            return XCTFail("Expected autosave effect")
        }
        XCTAssertEqual(id, "autosave-1")
        XCTAssertEqual(delay, 750)
        XCTAssertEqual(target.note_id.value, "id")
    }

    func testUnknownRequiredProblemCodeIsRejected() {
        let data = Data(#"{"state":{"folder":null,"active":null,"recoveries":[],"background_tasks":[]},"effects":[],"outcome":{"status":"rejected","value":{"code":"future_code","diagnostic":"future","details":null}}}"#.utf8)
        XCTAssertThrowsError(
            try JSONDecoder().decode(ApplicationResponse<ConnectResult>.self, from: data)
        )
    }
}
