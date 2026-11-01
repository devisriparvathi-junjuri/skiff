/**
 * OSC 52: a program in the terminal puts text on the clipboard. Claude Code
 * copies its own mouse selection this way, and so do Neovim, tmux and others
 * over SSH. The sequence is `ESC ] 52 ; <targets> ; <base64> BEL`. Every
 * target goes to the clipboard.
 *
 * Writes only. A query (`?`) would hand the clipboard to whatever runs in
 * the pane, so it gets no answer.
 */
import type { Terminal } from "@xterm/xterm";
import { copyText } from "../platform/clipboard";
import { showError } from "../ui/alerts";
import { clipboardWrites } from "../app/hosts";

/** The most base64 a program may send: about 6 MB of text. */
const MAX = 8 << 20;

export function handleOsc52(term: Terminal) {
  term.parser.registerOscHandler(52, (data) => {
    const payload = data.slice(data.indexOf(";") + 1);
    if (!data.includes(";") || !payload || payload === "?" || payload.length > MAX) return true;
    let text: string;
    try {
      const bin = atob(payload);
      text = new TextDecoder().decode(Uint8Array.from(bin, (c) => c.charCodeAt(0)));
    } catch {
      return true;
    }
    // A remote Host's programs write this Mac's clipboard only when allowed.
    void clipboardWrites()
      .then((ok) => (ok ? copyText(text) : undefined))
      .catch(showError);
    return true;
  });
}
