# re_i18n

Minimal runtime i18n for this fork's viewer: a global English/Chinese switch plus
helpers ([`tr`] and the [`trf!`] macro) to pick between an English and a Chinese
string at each call site.

The viewer is immediate-mode, so flipping the language and requesting a repaint
re-renders every widget in the new language — there is no cached text to invalidate.
