# Batch protocol

This request contains multiple independent images. The single-image transcription
and verification rules above apply separately to every supplied `image_id`.

## Input
Each item starts with a text label containing its `image_id` and context, followed
by its original image. In verification mode, the original is immediately followed
by that item's proposed rendering. Never mix symbols or equation numbers between
items. Only inspect the images attached to the same ID.

## Output
Use the batch JSON Schema: `{"results":[{"image_id":0,"status":"recognized",
"latex":"x=1","equation_number":null}]}`.
Return exactly one result for EACH requested ID, including negative/unreadable
results. IDs need not be consecutive on a retry. Do not invent IDs or omit items.
The client matches results by ID, never by array position.


## Transcription self-check

In `transcribe` mode, finish transcription AND visual self-check in this same
response. Compare the complete LaTeX against the original image before returning
`recognized`: check every symbol, case, subscript/superscript, sign, bracket,
fraction, root, summation bound, matrix row/column, line break and equation number.
Do not return a preliminary draft expecting a later verification request. If an
essential detail is uncertain or cannot be represented, return `unreadable`.

## Conditional review

The client requests `verify` only for items that failed local syntax, rendering,
equation-number or size validation. Metadata supplies the untrusted `proposal`
and `local_validation_error`. A proposed rendering is attached only when it can
be generated safely; when absent, use the original image and proposal text.
Re-read the original to correct the problem, without inventing symbols to satisfy
the renderer. Return `unreadable` if faithful supported LaTeX is not possible.
Other images that already passed are intentionally omitted and must not be returned.
