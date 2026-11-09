# Escape-injection test set

**Do not `cat` these files in a terminal:** they hold real escape bytes,
which would act on it (change its title, try to set the clipboard). Open
them in an editor that shows control characters, or with `cat -v`.

Each `NAME.md` places one terminal attack in every Markdown context
(body, heading, inline and block code, link text and target, wikilink,
list, quote, table, tag line). `md::render::tests::escape_corpus` renders
each at 60 columns, as Markdown and as plain text, and checks:

1. no cell of the screen holds a control or BiDi character;
2. the Markdown rendering matches `NAME.golden` exactly (escapes spelled
   out as `<U+XXXX>`).

To accept a deliberate change of the rendering, run
`UPDATE_GOLDEN=1 cargo test escape_corpus` and review the diff.
