// MIT, codex-chatgpt-web contributors. Type annotations removed from browser-user-message.ts.
/** Runs in the page. Read the submitted text, excluding only observed renderer chrome. */
function readChatGptUserMessage(
  target,
  connectorName,
) {
  let leadingConnector = false;
  let unsupported = false;
  let seenText = false;
  const rich = target.querySelector('[data-markdown-text-tone="user-message"].rich-text-user-turn');
  if (target.querySelectorAll('[data-markdown-text-tone="user-message"].rich-text-user-turn').length > 1) return;
  const text = (node) => {
    if (node.nodeType === Node.TEXT_NODE) {
      const value = node.textContent ?? "";
      if (value.trim()) seenText = true;
      return value;
    }
    if (!(node instanceof HTMLElement)) { unsupported = true; return ""; }
    if (node.matches('[data-prompt-link-href^="app://"]')
      || node.matches('a:has(> [data-prompt-link-href^="app://"])')) {
      if (seenText || leadingConnector || !connectorName || node.textContent !== connectorName) unsupported = true;
      leadingConnector = true;
      return "";
    }
    if (node.tagName === "BR") return "\n";
    if (node.matches('pre[data-markdown-copy="code-block"]')) {
      const code = node.querySelector('code[data-markdown-copy-content="true"][data-language]');
      if (!code || node.children.length !== 1) { unsupported = true; return ""; }
      seenText = true;
      return `\`\`\`${code.getAttribute("data-language")}\n${code.textContent ?? ""}\n\`\`\``;
    }
    if (node === rich) {
      const blocks = [...node.childNodes];
      if (blocks.some(block => !(block instanceof HTMLElement)
        || !(block.tagName === "P" || block.matches('[data-markdown-copy="blank-lines"]')))) {
        unsupported = true;
        return "";
      }
      const value = blocks.map(block => text(block) + ((block).tagName === "P" ? "\n" : "")).join("");
      return (blocks.at(-1))?.tagName === "P" ? value.slice(0, -1) : value;
    }
    // These are text wrappers in the captured plain and rich-text user renderers.
    // Other markup must not be silently flattened into a matching prompt.
    if (!["DIV", "SPAN", "P"].includes(node.tagName)) { unsupported = true; return ""; }
    return [...node.childNodes].map(text).join("");
  };
  const value = text(target);
  if (unsupported) return;
  // attachPrompt inserts a separating space after the plugin. ChatGPT can retain
  // its own chip separator too. Only this leading transport spacing is excluded.
  return { text: leadingConnector ? value.trimStart() : value, leadingConnector };
}

// Match the caller's literal marker, never a shortened or inferred turn_ prefix.
function literalTokenReceipts(text, token) {
  if(typeof token!=='string'||!token.length)return [];
  const receipts=[];let offset=0;
  const word=c=>typeof c==='string'&&/^[A-Za-z0-9_-]$/.test(c);
  while((offset=text.indexOf(token,offset))!==-1){
    if((word(token[0])&&word(text[offset-1]))||(word(token.at(-1))&&word(text[offset+token.length])))return [];
    receipts.push(token);offset+=1;
  }
  return receipts;
}
