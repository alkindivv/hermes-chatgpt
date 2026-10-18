// Native JavaScriptCore asset syntax and pure helper regression checks. No Node runtime.
import Foundation
import JavaScriptCore
let context = JSContext()!
var failure: String? = nil
context.exceptionHandler = { _, exception in failure = exception?.toString() }
func run(_ source: String) {
    _ = context.evaluateScript(source)
    if let error = failure { fatalError(error) }
}
let root = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "crates/browser/assets"
for asset in ["user-message.js", "classifier.js", "page.js"] {
    let source = try String(contentsOfFile: root + "/" + asset, encoding: .utf8)
    if asset == "page.js" { run("void (" + source + ");") } else { run(source) }
}
run("""
function check(condition) { if (!condition) throw Error('helper regression'); }
const longToken='turn_'+'0123456789abcdef'.repeat(4);
check(literalTokenReceipts('marker: '+longToken+'.',longToken).length===1);
check(literalTokenReceipts(longToken+' '+longToken,longToken).length===2);
check(literalTokenReceipts(longToken+'suffix',longToken).length===0);
const literal='marker:[random.*+?]';
check(literalTokenReceipts(' '+literal+' ',literal)[0]===literal);
check(literalTokenReceipts('anything','').length===0);
check(literalTokenReceipts('prefix','different').length===0);
const oldComment = { closest: s => s.includes('streaming') ? {} : null, compareDocumentPosition: () => 4 };
const answer = { closest: s => s.includes('content-search') ? {} : null, compareDocumentPosition: () => 0 };
const result = selectChatGptAnswerRoots([oldComment, answer], [{}]);
check(result.answerRoots.length === 1 && result.answerRoots[0] === answer);
check(result.commentaryRoots.length === 1);
const cot = { closest: s => s.includes('cot-v5') ? {} : null, compareDocumentPosition: () => 0 };
check(selectChatGptAnswerRoots([cot], []).answerRoots.length === 0);
class HTMLElement {
  constructor(tag, children = []) { this.tagName = tag; this.childNodes = children; this.children = children.filter(c => c instanceof HTMLElement); }
  querySelector() { return null; } querySelectorAll() { return []; } matches() { return false; }
}
const Node = { TEXT_NODE: 3 };
const text = value => ({ nodeType: 3, textContent: value });
check(readChatGptUserMessage(new HTMLElement('DIV', [text('hello'), new HTMLElement('BR'), text('world')])).text === 'hello\\nworld');
check(readChatGptUserMessage(new HTMLElement('DIV', [new HTMLElement('SCRIPT')])) === undefined);
""")
print("PASS: three browser assets parse; commentary ordering and user text helper regressions")
