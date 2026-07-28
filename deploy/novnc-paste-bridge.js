
// --- rerun deployment addition: seamless paste ---
// Appended to noVNC's app/ui.js by the Dockerfile (so `UI` is in scope).
//
// Stock noVNC only offers the sidebar clipboard panel: paste there, then Ctrl+V inside
// the session. Worse, noVNC's key grabbing swallows Cmd+V/Ctrl+V before the browser can
// fire a `paste` event, so a plain paste shortcut does nothing at all.
//
// This bridge intercepts the paste shortcut in the capture phase (before noVNC sees it),
// reads the clipboard via the async clipboard API (the browser asks for permission once;
// 127.0.0.1 and https count as secure contexts), forwards the text to the VNC server's
// clipboard (vncconfig relays it to the X clipboard), then sends Ctrl+V into the session
// so the application pastes immediately.

function rerunSendCtrlV() {
    UI.rfb.sendKey(0xffe3, 'ControlLeft', true); // Ctrl down
    UI.rfb.sendKey(0x0076, 'KeyV', true);
    UI.rfb.sendKey(0x0076, 'KeyV', false);
    UI.rfb.sendKey(0xffe3, 'ControlLeft', false); // Ctrl up
}

function rerunForwardClipboard(text) {
    if (!text || !UI.rfb) return;
    UI.rfb.clipboardPasteFrom(text);
    // Give the clipboard a moment to reach the X server before the app reads it.
    setTimeout(rerunSendCtrlV, 150);
}

window.addEventListener(
    'keydown',
    (event) => {
        const pasteCombo = (event.key === 'v' || event.key === 'V') && (event.metaKey || event.ctrlKey);
        if (!pasteCombo || !UI.rfb) return;
        // Pastes aimed at noVNC's own inputs (e.g. the clipboard sidebar) stay untouched.
        const target = event.target;
        if (target && (target.tagName === 'TEXTAREA' || target.tagName === 'INPUT')) return;
        if (!navigator.clipboard || !navigator.clipboard.readText) return;

        event.preventDefault();
        event.stopImmediatePropagation(); // keep noVNC from forwarding the raw shortcut too
        navigator.clipboard
            .readText()
            .then(rerunForwardClipboard)
            .catch((error) => console.warn('paste bridge: clipboard read failed:', error));
    },
    true // capture phase: run before noVNC's key grabbing
);

// Fallback for browsers where a real `paste` event still gets through.
window.addEventListener('paste', (event) => {
    try {
        const text = event.clipboardData && event.clipboardData.getData('text');
        const target = event.target;
        if (target && (target.tagName === 'TEXTAREA' || target.tagName === 'INPUT')) return;
        rerunForwardClipboard(text);
    } catch (error) {
        console.warn('paste bridge failed:', error);
    }
});
