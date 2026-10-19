// Progressive enhancement: the textarea remains the no-JavaScript review editor.
(() => {
  const form = document.querySelector('[data-redaction-form]');
  if (!form) return;
  const editor = form.querySelector('[data-redaction-editor]');
  const textarea = form.elements.redacted;
  const metadata = form.querySelector('[data-redaction-parts]');
  const tools = form.querySelector('[data-redaction-tools]');
  const hide = form.querySelector('[data-hide-selection]');
  const panel = form.querySelector('[data-reveal-panel]');
  const status = form.querySelector('[data-redaction-status]');
  let selectionRange = null;
  let activeChip = null;
  let customId = 0;

  // Build only text and buttons: pasted HTML and submitted metadata are never executed.
  function chip(part) {
    const button = document.createElement('button');
    button.type = 'button';
    button.contentEditable = 'false';
    button.className = 'redaction-chip';
    button.dataset.kind = part.kind;
    button.dataset.original = part.original;
    button.dataset.placeholder = part.text;
    button.textContent = `${part.kind} hidden`;
    button.setAttribute('aria-label', `Review ${part.kind} redaction`);
    return button;
  }
  function parts() {
    const result = [];
    const visit = (node) => {
      if (node.nodeType === Node.TEXT_NODE) {
        result.push({ original: '', kind: '', text: node.textContent });
      } else if (node.matches?.('[data-placeholder]')) {
        result.push({ original: node.dataset.original, kind: node.dataset.kind, text: node.dataset.placeholder });
      } else if (node.nodeName === 'BR') {
        result.push({ original: '', kind: '', text: '\n' });
      } else {
        node.childNodes.forEach(visit);
      }
    };
    editor.childNodes.forEach(visit);
    return result;
  }
  function sync() {
    const value = parts();
    textarea.value = value.map(p => p.text).join('');
    metadata.value = JSON.stringify(value);
  }
  function changed(message) {
    sync();
    status.textContent = message;
  }
  function dismiss() {
    panel.hidden = true;
    activeChip = null;
  }
  function currentRange() {
    const selection = window.getSelection();
    if (!selection?.rangeCount || selection.isCollapsed) return null;
    const range = selection.getRangeAt(0);
    if (!editor.contains(range.startContainer) || !editor.contains(range.endContainer)) return null;
    // A chip is atomic; hide plain text only to keep its original value recoverable.
    if (Array.from(editor.querySelectorAll('[data-placeholder]')).some(c => range.intersectsNode(c))) return null;
    return range;
  }

  // Restore structured segments, including custom redactions after a server error.
  const initial = JSON.parse(metadata.value);
  editor.replaceChildren(...initial.map(p => p.kind ? chip(p) : document.createTextNode(p.text)));
  editor.contentEditable = 'true';
  editor.hidden = false;
  textarea.hidden = true;
  tools.hidden = false;
  sync();
  editor.addEventListener('input', () => { dismiss(); selectionRange = null; hide.disabled = true; sync(); });
  editor.addEventListener('keydown', event => {
    // Force line breaks rather than browser-specific nested block elements.
    if (event.key === 'Enter' && !event.target.closest('[data-placeholder]')) {
      event.preventDefault();
      insertText('\n');
    }
    if (event.key === 'Escape') dismiss();
  });
  function insertText(text) {
    const selection = window.getSelection();
    if (!selection?.rangeCount) return;
    const range = selection.getRangeAt(0);
    if (!editor.contains(range.commonAncestorContainer)) return;
    range.deleteContents();
    const node = document.createTextNode(text);
    range.insertNode(node);
    range.setStartAfter(node);
    range.collapse(true);
    selection.removeAllRanges();
    selection.addRange(range);
    sync();
  }
  editor.addEventListener('paste', event => {
    event.preventDefault();
    insertText(event.clipboardData.getData('text/plain'));
  });
  // Disallow HTML drops into the editable surface.
  editor.addEventListener('drop', event => event.preventDefault());
  editor.addEventListener('click', event => {
    const target = event.target.closest('[data-placeholder]');
    if (!target) return;
    activeChip = target;
    form.querySelector('[data-reveal-value]').textContent = target.dataset.original;
    panel.hidden = false;
    form.querySelector('[data-reveal-confirm]').focus();
  });
  form.querySelector('[data-reveal-confirm]').addEventListener('click', () => {
    if (!activeChip || !editor.contains(activeChip)) return dismiss();
    activeChip.replaceWith(document.createTextNode(activeChip.dataset.original));
    dismiss();
    editor.focus();
    changed('Text revealed. It will be included if you submit to AI.');
  });
  form.querySelector('[data-reveal-cancel]').addEventListener('click', () => {
    const target = activeChip;
    dismiss();
    target?.focus();
  });
  document.addEventListener('selectionchange', () => {
    const range = currentRange();
    if (range) selectionRange = range.cloneRange();
    // Preserve the range while the user tabs to the toolbar button.
    if (range || editor.contains(document.activeElement)) hide.disabled = !range;
  });
  hide.addEventListener('mousedown', event => event.preventDefault());
  hide.addEventListener('click', () => {
    const range = currentRange() || selectionRange;
    if (!range || range.collapsed || !editor.contains(range.commonAncestorContainer)) return;
    const original = range.toString();
    if (!original) return;
    // Avoid collisions with placeholders already present in an edited message.
    let placeholder;
    do { placeholder = `‹CUSTOM_${++customId}›`; } while (textarea.value.includes(placeholder));
    const hidden = chip({ original, kind: 'CUSTOM', text: placeholder });
    range.deleteContents();
    range.insertNode(hidden);
    window.getSelection().removeAllRanges();
    selectionRange = null;
    hide.disabled = true;
    changed('Selected text hidden. Click its chip to reveal it.');
    hidden.focus();
  });
  form.addEventListener('submit', () => sync());
})();
