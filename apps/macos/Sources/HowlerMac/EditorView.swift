import AppKit
import SwiftUI

struct EditorSelectionPresentationContext: Equatable {
    let noteID: RustIdentity?
    let generation: UInt64?
    let revision: UInt64
    let presentsPendingDraft: Bool
}

struct NativeSelectionTracker {
    private(set) var context: EditorSelectionPresentationContext?
    private(set) var selection: RustSelection?

    func shouldApplyAuthoritativeSelection(in context: EditorSelectionPresentationContext) -> Bool {
        self.context != context
    }

    func selection(in context: EditorSelectionPresentationContext) -> RustSelection? {
        self.context == context ? selection : nil
    }

    mutating func record(_ selection: RustSelection?, in context: EditorSelectionPresentationContext) {
        self.context = context
        self.selection = selection
    }
}

@MainActor
struct EditorView: NSViewRepresentable {
    @ObservedObject var model: AppModel

    func makeCoordinator() -> Coordinator { Coordinator(model: model) }

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView()
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true

        let contentSize = scroll.contentSize
        let textView = NSTextView(frame: NSRect(origin: .zero, size: contentSize))
        textView.delegate = context.coordinator
        textView.isRichText = false
        textView.isEditable = model.hasActiveNote && !model.hasPendingNativeDraft
        textView.isSelectable = model.hasActiveNote
        textView.allowsUndo = false
        textView.drawsBackground = false
        textView.font = .preferredFont(forTextStyle: .body)
        textView.string = model.editorSource
        textView.minSize = NSSize(width: 0, height: contentSize.height)
        textView.maxSize = NSSize(
            width: CGFloat.greatestFiniteMagnitude,
            height: CGFloat.greatestFiniteMagnitude
        )
        textView.isVerticallyResizable = true
        textView.isHorizontallyResizable = false
        textView.autoresizingMask = [.width]
        textView.textContainer?.containerSize = NSSize(
            width: contentSize.width,
            height: CGFloat.greatestFiniteMagnitude
        )
        textView.textContainer?.widthTracksTextView = true
        scroll.documentView = textView
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        guard let textView = scroll.documentView as? NSTextView else { return }
        textView.isEditable = model.hasActiveNote && !model.hasPendingNativeDraft
        textView.isSelectable = model.hasActiveNote
        guard !textView.hasMarkedText() else { return }
        context.coordinator.applyingSnapshot = true
        if textView.string != model.editorSource {
            textView.string = model.editorSource
        }
        let selectionContext = model.selectionPresentationContext
        if model.hasPendingNativeDraft {
            context.coordinator.recordPresentationContext(selectionContext)
        } else if let selection = model.snapshot.selections.first,
                  context.coordinator.shouldApplyAuthoritativeSelection(in: selectionContext) {
            if let range = UTF8Range.nsRange(anchor: selection.anchor, head: selection.head, in: model.snapshot.source) {
                if textView.selectedRange() != range { textView.setSelectedRange(range) }
                context.coordinator.recordAuthoritativeSelection(selection, in: selectionContext)
            } else {
                context.coordinator.recordPresentationContext(selectionContext)
            }
        } else if model.snapshot.selections.isEmpty {
            context.coordinator.recordPresentationContext(selectionContext)
        }
        context.coordinator.applyingSnapshot = false
    }

    @MainActor
    final class Coordinator: NSObject, NSTextViewDelegate {
        private let model: AppModel
        var applyingSnapshot = false
        private var selectionTracker = NativeSelectionTracker()

        init(model: AppModel) { self.model = model }

        func shouldApplyAuthoritativeSelection(in context: EditorSelectionPresentationContext) -> Bool {
            selectionTracker.shouldApplyAuthoritativeSelection(in: context)
        }

        func recordAuthoritativeSelection(
            _ selection: RustSelection,
            in context: EditorSelectionPresentationContext
        ) {
            selectionTracker.record(selection, in: context)
        }

        func recordPresentationContext(_ context: EditorSelectionPresentationContext) {
            if selectionTracker.context != context {
                selectionTracker.record(nil, in: context)
            }
        }

        func textViewDidChangeSelection(_ notification: Notification) {
            guard !applyingSnapshot,
                  let textView = notification.object as? NSTextView,
                  let range = UTF8Range.byteRange(textView.selectedRange(), in: textView.string) else { return }
            let nativeSelection = UTF8Range.selection(
                range,
                affinity: textView.selectionAffinity,
                previous: selectionTracker.selection(in: model.selectionPresentationContext)
                    ?? model.snapshot.selections.first,
                revision: model.snapshot.revision
            )
            selectionTracker.record(nativeSelection, in: model.selectionPresentationContext)
        }

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
            let nativeRange = textView.selectedRange()
            guard let selectedBytes = UTF8Range.byteRange(nativeRange, in: new) else { return }
            let selection = UTF8Range.selection(
                selectedBytes,
                affinity: textView.selectionAffinity,
                previous: selectionTracker.selection(in: model.selectionPresentationContext)
                    ?? model.snapshot.selections.first,
                revision: model.snapshot.revision + 1
            )
            selectionTracker.record(selection, in: model.selectionPresentationContext)
            model.apply(
                range: difference.range,
                replacement: difference.replacement,
                selectionAnchor: selection.anchor,
                selectionHead: selection.head,
                affinity: selection.affinity,
                nativeSource: new
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

    static func nsRange(anchor: Int, head: Int, in source: String) -> NSRange? {
        nsRange(min(anchor, head)..<max(anchor, head), in: source)
    }

    static func selection(
        _ range: Range<Int>,
        affinity: NSSelectionAffinity,
        previous: RustSelection?,
        revision: UInt64
    ) -> RustSelection {
        let reversed = previous.map { selection in
            if selection.anchor == range.upperBound { return true }
            if selection.anchor == range.lowerBound { return false }
            return selection.anchor > selection.head
                && min(selection.anchor, selection.head) == range.lowerBound
                && max(selection.anchor, selection.head) == range.upperBound
        } ?? false
        return RustSelection(
            anchor: reversed ? range.upperBound : range.lowerBound,
            head: reversed ? range.lowerBound : range.upperBound,
            affinity: affinity == .upstream ? .upstream : .downstream,
            revision: revision
        )
    }
}
