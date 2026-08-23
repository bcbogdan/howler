import AppKit
import SwiftUI

@MainActor
struct EditorView: NSViewRepresentable {
    @ObservedObject var model: AppModel

    func makeCoordinator() -> Coordinator { Coordinator(model: model) }

    func makeNSView(context: Context) -> NSScrollView {
        let textView = NSTextView()
        textView.delegate = context.coordinator
        textView.isRichText = false
        textView.allowsUndo = false
        textView.drawsBackground = false
        textView.font = .preferredFont(forTextStyle: .body)
        textView.string = model.snapshot.source
        let scroll = NSScrollView()
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.documentView = textView
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        guard let textView = scroll.documentView as? NSTextView else { return }
        guard !textView.hasMarkedText() else { return }
        if textView.string != model.snapshot.source {
            context.coordinator.applyingSnapshot = true
            textView.string = model.snapshot.source
            context.coordinator.applyingSnapshot = false
        }
        if let selection = model.snapshot.selections.first,
           let range = UTF8Range.nsRange(selection.anchor..<selection.head, in: model.snapshot.source) {
            textView.setSelectedRange(range)
        }
    }

    @MainActor
    final class Coordinator: NSObject, NSTextViewDelegate {
        private let model: AppModel
        var applyingSnapshot = false

        init(model: AppModel) { self.model = model }

        func textDidChange(_ notification: Notification) {
            guard !applyingSnapshot,
                  let textView = notification.object as? NSTextView else { return }
            if textView.hasMarkedText() {
                model.compositionChanged(active: true)
                return
            }
            model.compositionChanged(active: false)
            guard textView.string != model.snapshot.source else { return }
            let old = model.snapshot.source
            let new = textView.string
            let difference = UTF8Range.difference(from: old, to: new)
            let nativeSelection = textView.selectedRange()
            guard let selectedBytes = UTF8Range.byteRange(nativeSelection, in: new) else { return }
            model.apply(
                range: difference.range,
                replacement: difference.replacement,
                selection: selectedBytes.upperBound
            )
        }
    }
}

enum UTF8Range {
    static func difference(from old: String, to new: String) -> (range: Range<Int>, replacement: String) {
        var oldStart = old.startIndex
        var newStart = new.startIndex
        while oldStart < old.endIndex, newStart < new.endIndex, old[oldStart] == new[newStart] {
            old.formIndex(after: &oldStart)
            new.formIndex(after: &newStart)
        }
        var oldEnd = old.endIndex
        var newEnd = new.endIndex
        while oldEnd > oldStart, newEnd > newStart {
            let oldPrevious = old.index(before: oldEnd)
            let newPrevious = new.index(before: newEnd)
            guard old[oldPrevious] == new[newPrevious] else { break }
            oldEnd = oldPrevious
            newEnd = newPrevious
        }
        let lower = old[..<oldStart].utf8.count
        let upper = lower + old[oldStart..<oldEnd].utf8.count
        return (lower..<upper, String(new[newStart..<newEnd]))
    }

    static func byteRange(_ range: NSRange, in source: String) -> Range<Int>? {
        guard let swiftRange = Range(range, in: source),
              let lower = swiftRange.lowerBound.samePosition(in: source.utf8),
              let upper = swiftRange.upperBound.samePosition(in: source.utf8) else { return nil }
        return source.utf8.distance(from: source.utf8.startIndex, to: lower)..<source.utf8.distance(from: source.utf8.startIndex, to: upper)
    }

    static func nsRange(_ range: Range<Int>, in source: String) -> NSRange? {
        guard range.lowerBound >= 0, range.upperBound <= source.utf8.count,
              let lowerUTF8 = source.utf8.index(source.utf8.startIndex, offsetBy: range.lowerBound, limitedBy: source.utf8.endIndex),
              let upperUTF8 = source.utf8.index(source.utf8.startIndex, offsetBy: range.upperBound, limitedBy: source.utf8.endIndex),
              let lower = lowerUTF8.samePosition(in: source), let upper = upperUTF8.samePosition(in: source) else { return nil }
        return NSRange(lower..<upper, in: source)
    }
}
