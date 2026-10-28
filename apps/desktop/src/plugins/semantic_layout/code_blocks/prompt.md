## Code and quotation containers
Use the complete literal `body` for `targets.classify_code_or_quote`, including every line. These authored containers are eligible, not protected boundaries.
For `targets.classify_code_or_quote`, return one `code_or_quote` per confident complete-container judgment. Do not also return another quote proposal for the same block.
- Set `presentation: code` for program/configuration/markup examples, considering the whole container and surrounding explanation. Preserve every line, including natural-language values inside XML or JSON. Supply a confident `language` or null; malformed illustrative snippets can still be code.
- Set `presentation: quote` for quoted prose and non-program specimens. Set `language: null`. A small inline code fragment inside quoted prose does not make the entire quote code.
- `authored_preformatted` is strong supporting evidence, not proof. Monospace fonts or class names alone are insufficient: recipes, chat, email, emoticons and punctuation specimens can be prose.
- Examples: `<span` followed by `id="page001"`, `epub:type="pagebreak"`, `role="doc-pagebreak"`, `aria-label="1"/>` is one XML code block. XML metadata containing a prose accessibility summary and JSON accessibility metadata are code as a whole, even with imperfect JSON syntax. An accessibility statement in ordinary prose and a specimen of apostrophes/quote marks remain quotations.
- `captions` may contain only adjacent explicit figure/table labels, captions or written source/credit paragraphs attached to this example. Keep them outside its body. Do not attach unrelated explanation or following narrative. Return [] when absent.
- For prose, `body_index` may identify its final eligible explicit credit. Otherwise return null. Never invent a credit or treat a code line as attribution.
Keep all code/quote URLs and authored link text intact. Never replace them with website icons or rewrite their spelling.
Do not select inline citations or formulas inside containers classified as code.
