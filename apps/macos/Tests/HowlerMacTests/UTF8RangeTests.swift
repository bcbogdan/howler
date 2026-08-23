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
}
