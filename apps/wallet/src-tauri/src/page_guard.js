// Runs in the wallet window before the page (see lib.rs). The window is an
// app, not a browser: the web view's own reload, history, print, save and
// view-source functions are kept away from the keyboard, the mouse buttons
// and the context menu. A reload would drop the screen of a transfer that is
// being sent, and print or save could put recovery words on paper or disk.
// Text fields keep their context menu (cut, copy, paste); zoom keys stay.
(() => {
  const MACOS = __MACOS__;
  const LETTERS = new Set(['r', 'p', 's', 'u']);

  const editable = (target) =>
    target instanceof Element &&
    target.closest('input, textarea, [contenteditable]:not([contenteditable="false"])') !== null;

  // The letter of a shortcut, also on keyboard layouts without Latin letters.
  const letter = (event) => {
    if (/^[a-z]$/i.test(event.key)) return event.key.toLowerCase();
    return /^Key[A-Z]$/.test(event.code) ? event.code.slice(3).toLowerCase() : '';
  };

  const blocked = (event) => {
    switch (event.key) {
      case 'F5':
      case 'BrowserRefresh':
      case 'BrowserBack':
      case 'BrowserForward':
      case 'BrowserHome':
        return true;
      case 'ArrowLeft':
      case 'ArrowRight':
      case 'Home':
        // Option+arrow moves by word in macOS text fields.
        return event.altKey && !event.ctrlKey && !event.metaKey && !(MACOS && editable(event.target));
    }
    return (event.ctrlKey || event.metaKey) && !event.altKey && LETTERS.has(letter(event));
  };

  window.addEventListener(
    'keydown',
    (event) => {
      if (blocked(event)) event.preventDefault();
    },
    true,
  );
  window.addEventListener(
    'contextmenu',
    (event) => {
      if (!editable(event.target)) event.preventDefault();
    },
    true,
  );
  // The back and forward mouse buttons.
  for (const type of ['mousedown', 'mouseup', 'auxclick']) {
    window.addEventListener(
      type,
      (event) => {
        if (event.button === 3 || event.button === 4) event.preventDefault();
      },
      true,
    );
  }
})();
