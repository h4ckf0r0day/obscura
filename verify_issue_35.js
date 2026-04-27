
if (typeof CharacterData === 'undefined') {
    throw new Error("CharacterData is not defined in global scope");
}
console.log("✓ CharacterData is defined");

const textNode = document.createTextNode("Hello");
console.log("Text node created: " + textNode.nodeName);

if (!(textNode instanceof Text)) throw new Error("textNode instanceof Text failed");
console.log("✓ textNode instanceof Text");

if (!(textNode instanceof CharacterData)) throw new Error("textNode instanceof CharacterData failed");
console.log("✓ textNode instanceof CharacterData");

if (!(textNode instanceof Node)) throw new Error("textNode instanceof Node failed");
console.log("✓ textNode instanceof Node");

if (textNode.data !== "Hello") throw new Error("textNode.data failed");
console.log("✓ textNode.data is correct");

textNode.appendData(" World!!!");
if (textNode.data !== "Hello World!!!") throw new Error("appendData failed");
console.log("✓ appendData works");

if (textNode.substringData(0, 5) !== "Hello") throw new Error("substringData failed");
console.log("✓ substringData works");

textNode.insertData(6, "Beautiful ");
textNode.deleteData(6, 10);
if (textNode.data !== "Hello World!!!") throw new Error("insert/deleteData failed");
console.log("✓ insert/deleteData works");

textNode.replaceData(6, 5, "Earth");
if (textNode.data !== "Hello Earth!!!") throw new Error("replaceData failed");
console.log("✓ replaceData works");

const div = document.createElement("div");
const t1 = document.createTextNode("AB");
div.appendChild(t1);
const t2 = t1.splitText(1);
if (t1.data !== "A" || t2.data !== "B" || div.childNodes.length !== 2) throw new Error("splitText failed");
console.log("✓ splitText works");

const comment = document.createComment("hidden");
if (!(comment instanceof Comment) || !(comment instanceof CharacterData)) throw new Error("Comment inheritance failed");
if (comment.nodeType !== 8 || comment.data !== "hidden") throw new Error("Comment properties failed");
console.log("✓ Comment node works");

// Test Comment textContent behavior (Regression Fix)
console.log("Testing textContent regression...");
const parentDiv = document.createElement('div');
const textBefore = document.createTextNode('Hello ');
const secretComment = document.createComment('Secret');
const textAfter = document.createTextNode('World');
parentDiv.appendChild(textBefore);
parentDiv.appendChild(secretComment);
parentDiv.appendChild(textAfter);

console.log("Parent textContent: " + parentDiv.textContent);
if (parentDiv.textContent !== 'Hello World') {
    throw new Error("Element.textContent should NOT include comment text. Got: " + parentDiv.textContent);
}
console.log("✓ Element.textContent correctly excludes comments");

console.log("Comment textContent: " + secretComment.textContent);
if (secretComment.textContent !== 'Secret') {
    throw new Error("Comment.textContent SHOULD include its data. Got: " + secretComment.textContent);
}
console.log("✓ Comment.textContent correctly includes its data");

console.log("\nFINAL STATUS: All tests passed! Regression fix verified.");
