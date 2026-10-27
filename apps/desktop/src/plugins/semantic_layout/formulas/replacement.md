## Replacement fidelity
Recognizing the characters is not enough to replace an image. Set
replacement_safe=true only when the complete meaning of the original can be
preserved in the proposed representation under ordinary reader reflow and
typography. This requirement applies equally to Unicode text, LaTeX and XHTML tables.

Judge the exact representation returned, not a hypothetical richer
reconstruction. Unicode text is rendered as ordinary prose, with no additional
visual or semantic markup. Preserving the character sequence alone is
insufficient if the image encodes distinctions or relationships that this
string cannot express. Do not assume that information is decorative merely
because it is typographic. Without a clear basis for treating it as incidental,
keep the original image.

If any meaningful information depends on visual presentation that the proposed
representation does not preserve, keep the original image. Do not approximate
that information with spaces, line breaks or a text-only LaTeX rendering.
Use the surrounding context to understand the purpose of the image, without
inventing missing content. When uncertain about replacement fidelity, keep the
original. Return not_formula with replacement_safe=false and null content.

For standard writing characters and ordinary prose, Unicode identity and reading
order can carry the full meaning. Rendering the same characters in the reader's
normal typeface is not itself information loss: preserve their identity and
semantic role, rather than the original pixels. This permits ordinary text
replacement, while still requiring retention when the image assigns distinctions
or relationships that the proposed content does not preserve. For mathematics,
the LaTeX must preserve the complete mathematical structure. The literal flag
only controls translation; it is not evidence that replacement is safe.
