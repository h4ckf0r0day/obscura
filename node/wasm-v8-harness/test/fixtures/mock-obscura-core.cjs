function findElement(html, selector) {
  let namePattern;
  let attributePattern = "[^>]*";
  if (selector.startsWith("#")) {
    namePattern = "[a-zA-Z][\\w:-]*";
    attributePattern = `[^>]*\\bid=["']${selector.slice(1)}["'][^>]*`;
  } else if (selector.startsWith(".")) {
    namePattern = "[a-zA-Z][\\w:-]*";
    attributePattern = `[^>]*\\bclass=["'][^"']*\\b${selector.slice(1)}\\b[^"']*["'][^>]*`;
  } else {
    namePattern = selector;
  }
  const match = new RegExp(`<(${namePattern})${attributePattern}>([\\s\\S]*?)<\\/\\1>`, "i").exec(html);
  if (!match) return null;
  return { outerHTML: match[0], textContent: match[2].replace(/<[^>]*>/g, "") };
}

class ObscuraCore {
  constructor(html) {
    this.source = html;
    this.freed = false;
  }

  queryText(selector) {
    this.#assertOpen();
    return findElement(this.source, selector)?.textContent;
  }

  query_html(selector) {
    this.#assertOpen();
    if (selector === "async-result") return Promise.resolve("<async-result></async-result>");
    return findElement(this.source, selector)?.outerHTML;
  }

  documentElementHtml() {
    this.#assertOpen();
    return this.source.replace(/^\s*<!doctype[^>]*>\s*/i, "");
  }

  free() {
    if (this.freed) throw new Error("core freed twice");
    this.freed = true;
  }

  #assertOpen() {
    if (this.freed) throw new Error("core is freed");
  }
}

module.exports = { ObscuraCore };
